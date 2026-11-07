//! Abstraction over one controllable display (DDC/CI monitor or WMI panel).

use std::time::Duration;

pub trait MonitorBackend: Send {
    /// Returns (current, maximum).
    fn read(&mut self, code: u8) -> anyhow::Result<(u32, u32)>;
    fn write(&mut self, code: u8, value: u32) -> anyhow::Result<()>;
    /// How long the device needs to settle after a write of `code`.
    fn settle_time(&self, _code: u8) -> Duration {
        Duration::ZERO
    }
    /// Attempts for a write/read before giving up.
    fn attempts(&self, _code: u8) -> u32 {
        1
    }
}

/// In-memory backend for tests.
#[derive(Debug, Default, Clone)]
pub struct FakeBackend {
    pub values: std::collections::HashMap<u8, (u32, u32)>,
    pub log: std::sync::Arc<std::sync::Mutex<Vec<(u8, u32)>>>,
}

impl MonitorBackend for FakeBackend {
    fn read(&mut self, code: u8) -> anyhow::Result<(u32, u32)> {
        self.values
            .get(&code)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("unsupported code {code:#x}"))
    }
    fn write(&mut self, code: u8, value: u32) -> anyhow::Result<()> {
        self.log.lock().unwrap().push((code, value));
        let max = self.values.get(&code).map(|v| v.1).unwrap_or(100);
        self.values.insert(code, (value, max));
        Ok(())
    }
}
