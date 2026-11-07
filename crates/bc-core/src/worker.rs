//! One thread per monitor. Every DDC/WMI call for a monitor goes through its
//! worker, so reads and writes never overlap on the bus. Writes are coalesced:
//! only the latest value per feature is sent ("last value wins").

use crate::backend::MonitorBackend;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Fresh value read from the hardware.
    Value { monitor: String, code: u8, current: u32, max: u32 },
    /// A write or read failed after all retries.
    Failed { monitor: String, code: u8, write: bool },
}

pub type Sink = Arc<dyn Fn(Event) + Send + Sync>;

enum Cmd {
    Set(u8, u32),
    Read(Vec<u8>),
    Stop,
}

pub struct Worker {
    pub id: String,
    tx: Sender<Cmd>,
    join: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn spawn(id: String, backend: Box<dyn MonitorBackend>, sink: Sink) -> Self {
        let (tx, rx) = mpsc::channel();
        let tid = id.clone();
        let join = std::thread::Builder::new()
            .name(format!("monitor-{id}"))
            .spawn(move || run(tid, backend, rx, sink))
            .expect("spawn worker");
        Self { id, tx, join: Some(join) }
    }

    /// Queue a raw value write. Older pending writes of the same code are dropped.
    pub fn set(&self, code: u8, raw: u32) {
        let _ = self.tx.send(Cmd::Set(code, raw));
    }

    /// Read the given codes and report them as [`Event::Value`].
    pub fn read(&self, codes: &[u8]) {
        let _ = self.tx.send(Cmd::Read(codes.to_vec()));
    }

    /// Stops the thread and waits for it (which releases the monitor handle).
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let _ = self.tx.send(Cmd::Stop);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Default)]
struct Batch {
    sets: Vec<(u8, u32)>,
    reads: Vec<u8>,
    stop: bool,
}

impl Batch {
    fn add(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Set(code, v) => {
                if let Some(e) = self.sets.iter_mut().find(|e| e.0 == code) {
                    e.1 = v;
                } else {
                    self.sets.push((code, v));
                }
                // A pending read of the same code would report a stale value.
                self.reads.retain(|&c| c != code);
            }
            Cmd::Read(codes) => {
                for c in codes {
                    // Sets run before reads, so a read queued after a set reports the new value.
                    if !self.reads.contains(&c) {
                        self.reads.push(c);
                    }
                }
            }
            Cmd::Stop => self.stop = true,
        }
    }
    fn is_empty(&self) -> bool {
        self.sets.is_empty() && self.reads.is_empty()
    }
}

fn drain(rx: &Receiver<Cmd>, batch: &mut Batch) {
    while let Ok(cmd) = rx.try_recv() {
        batch.add(cmd);
    }
}

/// Sleeps while still collecting commands, so a long settle time never
/// delays newer values: they simply replace what is queued.
fn settle(rx: &Receiver<Cmd>, batch: &mut Batch, dur: Duration) {
    let deadline = std::time::Instant::now() + dur;
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            return;
        }
        match rx.recv_timeout(deadline - now) {
            Ok(cmd) => {
                batch.add(cmd);
                if batch.stop {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => return,
            Err(RecvTimeoutError::Disconnected) => {
                batch.stop = true;
                return;
            }
        }
    }
}

fn run(id: String, mut backend: Box<dyn MonitorBackend>, rx: Receiver<Cmd>, sink: Sink) {
    let mut batch = Batch::default();
    loop {
        if batch.is_empty() && !batch.stop {
            match rx.recv() {
                Ok(cmd) => batch.add(cmd),
                Err(_) => break,
            }
        }
        drain(&rx, &mut batch);
        if batch.stop {
            // Last values (a profile applied right before Exit, say) still
            // go out, once each and without waiting.
            for (code, value) in batch.sets.drain(..) {
                if let Err(e) = backend.write(code, value) {
                    tracing::debug!("[{id}] final write {code:#04x}={value} failed: {e}");
                }
            }
            break;
        }

        if !batch.sets.is_empty() {
            let (code, value) = batch.sets.remove(0);
            let attempts = backend.attempts(code).max(1);
            let mut ok = false;
            for i in 0..attempts {
                match backend.write(code, value) {
                    Ok(()) => {
                        ok = true;
                        break;
                    }
                    Err(e) => {
                        tracing::debug!("[{id}] write {code:#04x}={value} attempt {} failed: {e}", i + 1);
                        settle(&rx, &mut batch, Duration::from_millis(50));
                    }
                }
            }
            if !ok {
                tracing::warn!("[{id}] write {code:#04x}={value} failed after {attempts} attempts");
                sink(Event::Failed { monitor: id.clone(), code, write: true });
            }
            let wait = backend.settle_time(code);
            settle(&rx, &mut batch, wait);
            continue;
        }

        if !batch.reads.is_empty() {
            let code = batch.reads.remove(0);
            let attempts = backend.attempts(code).max(1);
            let mut result = None;
            for i in 0..attempts {
                match backend.read(code) {
                    Ok(v) => {
                        result = Some(v);
                        break;
                    }
                    Err(e) => {
                        tracing::debug!("[{id}] read {code:#04x} attempt {} failed: {e}", i + 1);
                        settle(&rx, &mut batch, Duration::from_millis(40));
                    }
                }
            }
            match result {
                // A write queued while we were reading makes this value stale.
                Some(_) if batch.sets.iter().any(|s| s.0 == code) => {}
                Some((current, max)) => sink(Event::Value { monitor: id.clone(), code, current, max }),
                None => sink(Event::Failed { monitor: id.clone(), code, write: false }),
            }
            let wait = backend.settle_time(0);
            settle(&rx, &mut batch, wait);
        }
    }
    tracing::debug!("[{id}] worker stopped");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::FakeBackend;
    use std::sync::Mutex;

    struct Slow(FakeBackend);
    impl MonitorBackend for Slow {
        fn read(&mut self, code: u8) -> anyhow::Result<(u32, u32)> {
            self.0.read(code)
        }
        fn write(&mut self, code: u8, value: u32) -> anyhow::Result<()> {
            self.0.write(code, value)
        }
        fn settle_time(&self, _code: u8) -> Duration {
            Duration::from_millis(40)
        }
    }

    #[test]
    fn stop_still_sends_the_last_value() {
        let fake = FakeBackend::default();
        let log = fake.log.clone();
        let w = Worker::spawn("m".into(), Box::new(Slow(fake)), Arc::new(|_| {}));
        w.set(0x10, 10); // busy settling after this one...
        std::thread::sleep(Duration::from_millis(5));
        w.set(0x10, 70); // ...while this one is queued
        w.stop();
        assert_eq!(log.lock().unwrap().last(), Some(&(0x10, 70)));
    }

    #[test]
    fn coalesces_writes_and_keeps_last_value() {
        let mut fake = FakeBackend::default();
        fake.values.insert(0x10, (50, 100));
        let log = fake.log.clone();
        let events = Arc::new(Mutex::new(Vec::new()));
        let ev = events.clone();
        let w = Worker::spawn("m".into(), Box::new(Slow(fake)), Arc::new(move |e| ev.lock().unwrap().push(e)));
        for v in 0..=100 {
            w.set(0x10, v);
        }
        w.read(&[0x10]);
        std::thread::sleep(Duration::from_millis(400));
        w.stop();
        let log = log.lock().unwrap();
        assert!(log.len() < 10, "writes were not coalesced: {}", log.len());
        assert_eq!(log.last(), Some(&(0x10, 100)));
        let events = events.lock().unwrap();
        assert_eq!(
            events.last(),
            Some(&Event::Value { monitor: "m".into(), code: 0x10, current: 100, max: 100 })
        );
    }
}
