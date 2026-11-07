//! Runtime state owned by the UI thread: monitors + their workers + settings.
//! The UI only renders this and forwards user intent; all rules live here.

use crate::discovery::{Discovered, MonitorKind};
use crate::legacy;
use crate::logic::{self, RepeatAccel, WheelAccel};
use crate::settings::*;
use crate::vcp;
use crate::worker::{Event, Sink, Worker};
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub struct Monitor {
    pub id: String,
    pub hw_name: String,
    pub description: String,
    pub kind: MonitorKind,
    pub brightness: u8,
    pub volume: u8,
    pub input: Option<u8>,
    pub max_brightness: u32,
    pub max_volume: u32,
    pub supports_volume: bool,
    pub supports_input: bool,
    worker: Worker,
}

impl Monitor {
    pub fn is_internal(&self) -> bool {
        self.kind == MonitorKind::Internal
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Nothing,
    ToggleFlyout,
    QuickMenu,
    /// Something changed that deserves an on-screen indicator.
    /// `monitor` is the display index, or `None` for all displays.
    Osd { title: String, value: u8, volume: bool, monitor: Option<usize> },
    Profile(String),
}

/// After we write a value, read-backs of that feature are ignored for a moment,
/// so a slow monitor can never make a slider jump back.
const WRITE_GUARD: Duration = Duration::from_millis(1500);

pub struct Session {
    pub settings: Settings,
    pub monitors: Vec<Monitor>,
    sink: Sink,
    wheel: WheelAccel,
    repeat: RepeatAccel,
    last_write: HashMap<(String, u8), Instant>,
}

impl Session {
    pub fn new(settings: Settings, sink: Sink) -> Self {
        Self {
            settings,
            monitors: Vec::new(),
            sink,
            wheel: WheelAccel::default(),
            repeat: RepeatAccel::default(),
            last_write: HashMap::new(),
        }
    }

    /// Replaces the monitor set with a fresh scan. Old workers are stopped
    /// first (which releases their handles). Returns true if settings changed.
    pub fn install(&mut self, found: Vec<Discovered>) -> bool {
        for m in self.monitors.drain(..) {
            m.worker.stop();
        }
        let mut changed = false;
        {
            let known: Vec<legacy::Known> = found
                .iter()
                .map(|d| legacy::Known {
                    id: &d.id,
                    internal: d.kind == MonitorKind::Internal,
                    description: &d.description,
                })
                .collect();
            changed |= legacy::resolve_ids(&mut self.settings, &known);
        }
        for d in found {
            if self.settings.monitor(&d.id).is_none() {
                let cfg = self.settings.monitor_mut(&d.id);
                cfg.inputs = vcp::all_input_codes();
                changed = true;
            }
            let (b, bmax) = d.caps.brightness.unwrap_or((50, 100));
            let (v, vmax) = d.caps.volume.unwrap_or((50, 100));
            self.monitors.push(Monitor {
                worker: Worker::spawn(d.id.clone(), d.backend, self.sink.clone()),
                id: d.id,
                hw_name: d.name,
                description: d.description,
                kind: d.kind,
                brightness: logic::raw_to_pct(b, bmax),
                volume: logic::raw_to_pct(v, vmax),
                input: d.caps.input.map(vcp::normalize_input),
                max_brightness: bmax,
                max_volume: vmax,
                supports_volume: d.caps.volume.is_some(),
                supports_input: d.caps.input.is_some(),
            });
        }
        changed
    }

    pub fn shutdown(&mut self) {
        for m in self.monitors.drain(..) {
            m.worker.stop();
        }
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.monitors.iter().position(|m| m.id == id)
    }

    pub fn display_name(&self, idx: usize) -> String {
        let m = &self.monitors[idx];
        self.settings
            .monitor(&m.id)
            .and_then(|c| c.alias.clone())
            .filter(|a| !a.trim().is_empty())
            .unwrap_or_else(|| m.hw_name.clone())
    }

    pub fn shows_volume(&self, idx: usize) -> bool {
        let m = &self.monitors[idx];
        !m.is_internal() && self.settings.monitor(&m.id).map(|c| c.show_volume).unwrap_or(false)
    }

    pub fn shows_input(&self, idx: usize) -> bool {
        let m = &self.monitors[idx];
        !m.is_internal() && self.settings.monitor(&m.id).map(|c| c.show_input).unwrap_or(false)
    }

    /// Input codes offered for a monitor, in table order.
    pub fn visible_inputs(&self, idx: usize) -> Vec<u8> {
        let enabled = self.settings.monitor(&self.monitors[idx].id).map(|c| c.inputs.clone()).unwrap_or_default();
        vcp::INPUTS
            .iter()
            .map(|i| i.code)
            .filter(|c| enabled.is_empty() || enabled.contains(c))
            .collect()
    }

    /// Re-read live values (called whenever the flyout opens).
    pub fn refresh(&self) {
        for (i, m) in self.monitors.iter().enumerate() {
            let mut codes = vec![vcp::BRIGHTNESS];
            if self.shows_volume(i) {
                codes.push(vcp::VOLUME);
            }
            if self.shows_input(i) {
                codes.push(vcp::INPUT_SOURCE);
            }
            m.worker.read(&codes);
        }
    }

    fn write(&mut self, idx: usize, code: u8, raw: u32) {
        let m = &self.monitors[idx];
        self.last_write.insert((m.id.clone(), code), Instant::now());
        m.worker.set(code, raw);
    }

    pub fn set_brightness(&mut self, idx: usize, pct: u8) {
        let pct = pct.min(100);
        if self.monitors[idx].brightness == pct {
            return;
        }
        self.monitors[idx].brightness = pct;
        let raw = logic::pct_to_raw(pct, self.monitors[idx].max_brightness);
        self.write(idx, vcp::BRIGHTNESS, raw);
    }

    pub fn set_volume(&mut self, idx: usize, pct: u8) {
        let pct = pct.min(100);
        if self.monitors[idx].volume == pct || self.monitors[idx].is_internal() {
            return;
        }
        self.monitors[idx].volume = pct;
        let raw = logic::pct_to_raw(pct, self.monitors[idx].max_volume);
        self.write(idx, vcp::VOLUME, raw);
    }

    pub fn set_input(&mut self, idx: usize, code: u8) {
        if self.monitors[idx].is_internal() {
            return;
        }
        self.monitors[idx].input = Some(code);
        self.write(idx, vcp::INPUT_SOURCE, code as u32);
    }

    pub fn has_master(&self) -> bool {
        self.monitors.len() > 1
    }

    /// Master shows the internal panel's value (or the first monitor's).
    pub fn master(&self) -> u8 {
        self.monitors
            .iter()
            .find(|m| m.is_internal())
            .or(self.monitors.first())
            .map(|m| m.brightness)
            .unwrap_or(50)
    }

    pub fn set_master(&mut self, pct: u8) {
        for i in 0..self.monitors.len() {
            self.set_brightness(i, pct);
        }
    }

    /// Applies a hardware read-back. Returns true if the UI must refresh.
    pub fn on_event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Value { monitor, code, current, max } => {
                if let Some(t) = self.last_write.get(&(monitor.clone(), code)) {
                    if t.elapsed() < WRITE_GUARD {
                        return false;
                    }
                }
                let Some(idx) = self.index_of(&monitor) else { return false };
                let m = &mut self.monitors[idx];
                match code {
                    vcp::BRIGHTNESS => {
                        if max > 0 {
                            m.max_brightness = max;
                        }
                        let pct = logic::raw_to_pct(current, m.max_brightness);
                        std::mem::replace(&mut m.brightness, pct) != pct
                    }
                    vcp::VOLUME => {
                        if max > 0 {
                            m.max_volume = max;
                        }
                        m.supports_volume = true;
                        let pct = logic::raw_to_pct(current, m.max_volume);
                        std::mem::replace(&mut m.volume, pct) != pct
                    }
                    vcp::INPUT_SOURCE => {
                        m.supports_input = true;
                        let v = Some(vcp::normalize_input(current));
                        std::mem::replace(&mut m.input, v) != v
                    }
                    _ => false,
                }
            }
            Event::Failed { .. } => false,
        }
    }

    /// Wheel over the tray icon: master (or the only monitor).
    pub fn tray_wheel(&mut self, up: bool, ctrl: bool) -> Outcome {
        if self.monitors.is_empty() {
            return Outcome::Nothing;
        }
        let step = self.wheel.step(Instant::now(), ctrl);
        let delta = if up { step } else { -step };
        let v = logic::clamp_pct(self.master() as i32 + delta);
        self.set_master(v);
        Outcome::Osd { title: self.master_title(), value: v, volume: false, monitor: None }
    }

    /// Wheel over a slider inside the flyout; returns the new value.
    pub fn wheel_step(&mut self, up: bool, ctrl: bool) -> i32 {
        let s = self.wheel.step(Instant::now(), ctrl);
        if up {
            s
        } else {
            -s
        }
    }

    fn master_title(&self) -> String {
        if self.has_master() {
            "All Displays".into()
        } else {
            self.display_name(0)
        }
    }

    pub fn run_action(&mut self, action: &Action) -> Outcome {
        match action.kind {
            ActionKind::ToggleFlyout => Outcome::ToggleFlyout,
            ActionKind::QuickMenu => Outcome::QuickMenu,
            ActionKind::ApplyProfile => match action.profile.and_then(|p| self.apply_profile(p)) {
                Some(name) => Outcome::Profile(name),
                None => Outcome::Nothing,
            },
            ActionKind::MasterUp | ActionKind::MasterDown => {
                if self.monitors.is_empty() {
                    return Outcome::Nothing;
                }
                let s = self.repeat.step(&action.key(), Instant::now());
                let d = if action.kind == ActionKind::MasterUp { s } else { -s };
                let v = logic::clamp_pct(self.master() as i32 + d);
                self.set_master(v);
                Outcome::Osd { title: self.master_title(), value: v, volume: false, monitor: None }
            }
            ActionKind::BrightnessUp | ActionKind::BrightnessDown | ActionKind::VolumeUp | ActionKind::VolumeDown => {
                let Some(idx) = action.monitor.as_deref().and_then(|id| self.index_of(id)) else {
                    return Outcome::Nothing;
                };
                let volume = matches!(action.kind, ActionKind::VolumeUp | ActionKind::VolumeDown);
                if volume && self.monitors[idx].is_internal() {
                    return Outcome::Nothing;
                }
                let s = self.repeat.step(&action.key(), Instant::now());
                let d = if matches!(action.kind, ActionKind::BrightnessUp | ActionKind::VolumeUp) { s } else { -s };
                let m = &self.monitors[idx];
                let v = logic::clamp_pct(if volume { m.volume } else { m.brightness } as i32 + d);
                if volume {
                    self.set_volume(idx, v);
                } else {
                    self.set_brightness(idx, v);
                }
                Outcome::Osd { title: self.display_name(idx), value: v, volume, monitor: Some(idx) }
            }
        }
    }

    pub fn apply_profile(&mut self, id: u32) -> Option<String> {
        let p = self.settings.profiles.iter().find(|p| p.id == id)?.clone();
        for v in &p.values {
            if let Some(idx) = self.index_of(&v.monitor) {
                self.set_brightness(idx, v.brightness);
                if let Some(vol) = v.volume {
                    if self.shows_volume(idx) {
                        self.set_volume(idx, vol);
                    }
                }
            }
        }
        Some(p.name)
    }

    /// Current values as profile entries (volume only where it is shown).
    pub fn capture(&self) -> Vec<ProfileValue> {
        self.monitors
            .iter()
            .enumerate()
            .map(|(i, m)| ProfileValue {
                monitor: m.id.clone(),
                brightness: m.brightness,
                volume: self.shows_volume(i).then_some(m.volume),
            })
            .collect()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::FakeBackend;
    use crate::discovery::Caps;
    use std::sync::{Arc, Mutex};

    fn fake(id: &str, kind: MonitorKind, b: u32) -> (Discovered, Arc<Mutex<Vec<(u8, u32)>>>) {
        let mut f = FakeBackend::default();
        f.values.insert(vcp::BRIGHTNESS, (b, 100));
        f.values.insert(vcp::VOLUME, (30, 100));
        let log = f.log.clone();
        (
            Discovered {
                id: id.into(),
                name: id.into(),
                description: "Generic PnP Monitor".into(),
                kind,
                caps: Caps { brightness: Some((b, 100)), volume: Some((30, 100)), input: Some(15) },
                backend: Box::new(f),
            },
            log,
        )
    }

    fn session() -> (Session, Arc<Mutex<Vec<(u8, u32)>>>, Arc<Mutex<Vec<(u8, u32)>>>) {
        let mut s = Session::new(Settings::default(), Arc::new(|_| {}));
        let (a, la) = fake("internal", MonitorKind::Internal, 40);
        let (b, lb) = fake("EXT-1", MonitorKind::External, 70);
        s.install(vec![a, b]);
        (s, la, lb)
    }

    #[test]
    fn master_writes_each_monitor_once() {
        let (mut s, la, lb) = session();
        assert_eq!(s.master(), 40);
        s.set_master(60);
        s.set_master(60);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(*la.lock().unwrap(), vec![(vcp::BRIGHTNESS, 60)]);
        assert_eq!(*lb.lock().unwrap(), vec![(vcp::BRIGHTNESS, 60)]);
    }

    #[test]
    fn stale_readback_is_ignored_after_write() {
        let (mut s, _, _) = session();
        s.set_brightness(1, 90);
        let changed = s.on_event(Event::Value { monitor: "EXT-1".into(), code: vcp::BRIGHTNESS, current: 70, max: 100 });
        assert!(!changed);
        assert_eq!(s.monitors[1].brightness, 90);
    }

    #[test]
    fn profile_roundtrip_respects_volume_visibility() {
        let (mut s, _, lb) = session();
        s.settings.monitor_mut("EXT-1").show_volume = true;
        s.set_volume(1, 80);
        let values = s.capture();
        assert_eq!(values[0].volume, None);
        assert_eq!(values[1].volume, Some(80));
        s.settings.profiles.push(Profile { id: 1, name: "night".into(), values });
        s.set_brightness(1, 10);
        assert_eq!(s.run_action(&Action::profile(1)), Outcome::Profile("night".into()));
        assert_eq!(s.monitors[1].brightness, 70);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(lb.lock().unwrap().last(), Some(&(vcp::BRIGHTNESS, 70)));
    }

    #[test]
    fn volume_hotkey_on_internal_does_nothing() {
        let (mut s, _, _) = session();
        let out = s.run_action(&Action::monitor(ActionKind::VolumeUp, "internal"));
        assert_eq!(out, Outcome::Nothing);
    }
}
