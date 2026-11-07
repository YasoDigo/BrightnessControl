//! Internal (laptop) panel brightness and EDID identity through WMI (`root\WMI`).

use crate::backend::MonitorBackend;
use serde::{Deserialize, Serialize};
use wmi::WMIConnection;

#[allow(non_snake_case, non_camel_case_types)]
#[derive(Deserialize, Debug)]
#[serde(rename = "WmiMonitorID")]
struct WmiMonitorID {
    InstanceName: String,
    Active: bool,
    ManufacturerName: Option<Vec<u16>>,
    ProductCodeID: Option<Vec<u16>>,
    SerialNumberID: Option<Vec<u16>>,
    UserFriendlyName: Option<Vec<u16>>,
}

#[allow(non_snake_case, non_camel_case_types)]
#[derive(Deserialize, Debug)]
#[serde(rename = "WmiMonitorBrightness")]
struct WmiMonitorBrightness {
    InstanceName: String,
    Active: bool,
    CurrentBrightness: u8,
}

#[allow(non_snake_case, non_camel_case_types)]
#[derive(Deserialize, Debug)]
#[serde(rename = "WmiMonitorBrightnessMethods")]
struct WmiMonitorBrightnessMethods {
    __Path: String,
    InstanceName: String,
    Active: bool,
}

#[allow(non_snake_case)]
#[derive(Serialize)]
struct SetBrightnessIn {
    Timeout: u32,
    Brightness: u8,
}

/// EDID identity of a connected display.
#[derive(Debug, Clone, Default)]
pub struct EdidInfo {
    pub instance: String,
    pub manufacturer: String,
    pub product: String,
    pub serial: String,
    pub name: String,
}

impl EdidInfo {
    /// Stable id that survives port/cable/order changes, e.g. `GBT2404-0000A1B2C3D4`.
    pub fn stable_id(&self) -> Option<String> {
        if self.manufacturer.is_empty() || self.product.is_empty() {
            return None;
        }
        let serial = self.serial.trim();
        if serial.is_empty() || serial.chars().all(|c| c == '0') {
            None
        } else {
            Some(format!("{}{}-{}", self.manufacturer, self.product, serial))
        }
    }
}

fn decode(v: &Option<Vec<u16>>) -> String {
    v.as_ref()
        .map(|v| {
            v.iter()
                .take_while(|&&c| c != 0)
                .filter_map(|&c| char::from_u32(c as u32))
                .collect::<String>()
        })
        .unwrap_or_default()
        .trim()
        .to_string()
}

pub fn connect() -> anyhow::Result<WMIConnection> {
    Ok(WMIConnection::with_namespace_path("ROOT\\WMI")?)
}

pub fn edid_list(con: &WMIConnection) -> Vec<EdidInfo> {
    match con.query::<WmiMonitorID>() {
        Ok(list) => list
            .into_iter()
            .filter(|m| m.Active)
            .map(|m| EdidInfo {
                instance: m.InstanceName.clone(),
                manufacturer: decode(&m.ManufacturerName),
                product: decode(&m.ProductCodeID),
                serial: decode(&m.SerialNumberID),
                name: decode(&m.UserFriendlyName),
            })
            .collect(),
        Err(e) => {
            tracing::warn!("WmiMonitorID query failed: {e}");
            Vec::new()
        }
    }
}

/// Panels whose brightness Windows controls itself (laptop screens): (instance, current %).
pub fn internal_panels(con: &WMIConnection) -> Vec<(String, u8)> {
    match con.query::<WmiMonitorBrightness>() {
        Ok(list) => list
            .into_iter()
            .filter(|m| m.Active)
            .map(|m| (m.InstanceName, m.CurrentBrightness))
            .collect(),
        Err(e) => {
            tracing::debug!("WmiMonitorBrightness not available: {e}");
            Vec::new()
        }
    }
}

/// `WMIConnection` is `!Send`; it is created lazily inside the worker thread
/// and never moved afterwards.
struct LazyConn(Option<WMIConnection>);
unsafe impl Send for LazyConn {}

pub struct InternalBackend {
    instance: String,
    conn: LazyConn,
}

impl InternalBackend {
    pub fn new(instance: String) -> Self {
        Self { instance, conn: LazyConn(None) }
    }

    fn con(&mut self) -> anyhow::Result<&WMIConnection> {
        if self.conn.0.is_none() {
            self.conn.0 = Some(connect()?);
        }
        Ok(self.conn.0.as_ref().unwrap())
    }
}

impl MonitorBackend for InternalBackend {
    fn read(&mut self, code: u8) -> anyhow::Result<(u32, u32)> {
        anyhow::ensure!(code == crate::vcp::BRIGHTNESS, "internal panel only supports brightness");
        let instance = self.instance.clone();
        // Always a fresh query: cached WMI objects keep stale property values
        // (this was the root of the "slider jumps back" bug in the C# version).
        let res = self.con()?.query::<WmiMonitorBrightness>();
        match res {
            Ok(list) => list
                .into_iter()
                .find(|m| m.InstanceName.eq_ignore_ascii_case(&instance))
                .map(|m| (m.CurrentBrightness as u32, 100))
                .ok_or_else(|| anyhow::anyhow!("internal panel disappeared")),
            Err(e) => {
                self.conn.0 = None;
                Err(e.into())
            }
        }
    }

    fn write(&mut self, code: u8, value: u32) -> anyhow::Result<()> {
        anyhow::ensure!(code == crate::vcp::BRIGHTNESS, "internal panel only supports brightness");
        let instance = self.instance.clone();
        let res = (|| -> anyhow::Result<()> {
            let con = self.con()?;
            let methods: Vec<WmiMonitorBrightnessMethods> = con.query()?;
            let m = methods
                .into_iter()
                .find(|m| m.Active && m.InstanceName.eq_ignore_ascii_case(&instance))
                .ok_or_else(|| anyhow::anyhow!("brightness methods not found"))?;
            con.exec_instance_method::<WmiMonitorBrightnessMethods, ()>(
                &m.__Path,
                "WmiSetBrightness",
                SetBrightnessIn { Timeout: 0, Brightness: value.min(100) as u8 },
            )?;
            Ok(())
        })();
        if res.is_err() {
            self.conn.0 = None;
        }
        res
    }

    fn attempts(&self, _code: u8) -> u32 {
        2
    }
}
