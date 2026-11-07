//! DDC/CI access through dxva2 (Monitor Configuration API).

use crate::backend::MonitorBackend;
use crate::vcp;
use std::time::Duration;
use windows::core::{BOOL, PCWSTR};
use windows::Win32::Devices::Display::{
    DestroyPhysicalMonitor, GetNumberOfPhysicalMonitorsFromHMONITOR, GetPhysicalMonitorsFromHMONITOR,
    GetVCPFeatureAndVCPFeatureReply, SetVCPFeature, PHYSICAL_MONITOR,
};
use windows::Win32::Foundation::{HANDLE, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, DISPLAY_DEVICEW, HDC, HMONITOR, MONITORINFO,
    MONITORINFOEXW,
};

const EDD_GET_DEVICE_INTERFACE_NAME: u32 = 0x1;
const DISPLAY_DEVICE_ACTIVE: u32 = 0x1;

/// An owned physical monitor handle. Destroyed on drop.
pub struct PhysicalMonitor {
    handle: HANDLE,
    pub description: String,
    /// `\\?\DISPLAY#GBT2404#5&...#{guid}` when it could be resolved.
    pub device_path: Option<String>,
    /// `\\.\DISPLAY1`
    pub gdi_device: String,
}

// The handle is only ever used by one thread at a time (the owning worker).
unsafe impl Send for PhysicalMonitor {}

impl Drop for PhysicalMonitor {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            unsafe {
                let _ = DestroyPhysicalMonitor(self.handle);
            }
        }
    }
}

impl std::fmt::Debug for PhysicalMonitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhysicalMonitor")
            .field("description", &self.description)
            .field("device_path", &self.device_path)
            .field("gdi_device", &self.gdi_device)
            .finish()
    }
}

impl PhysicalMonitor {
    pub fn get(&self, code: u8) -> anyhow::Result<(u32, u32)> {
        let mut cur = 0u32;
        let mut max = 0u32;
        let ok = unsafe { GetVCPFeatureAndVCPFeatureReply(self.handle, code, None, &mut cur, Some(&mut max)) };
        if ok != 0 {
            Ok((cur, max))
        } else {
            Err(anyhow::anyhow!(
                "GetVCPFeature({code:#04x}) failed: {}",
                windows::core::Error::from_thread()
            ))
        }
    }

    pub fn set(&self, code: u8, value: u32) -> anyhow::Result<()> {
        let ok = unsafe { SetVCPFeature(self.handle, code, value) };
        if ok != 0 {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "SetVCPFeature({code:#04x}, {value}) failed: {}",
                windows::core::Error::from_thread()
            ))
        }
    }
}

pub(crate) fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

unsafe extern "system" fn collect_monitor(h: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let list = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
    list.push(h);
    BOOL(1)
}

fn device_paths_for(gdi_device: &str) -> Vec<String> {
    let dev: Vec<u16> = gdi_device.encode_utf16().chain(Some(0)).collect();
    let mut out = Vec::new();
    for i in 0.. {
        let mut dd = DISPLAY_DEVICEW { cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
        let ok = unsafe { EnumDisplayDevicesW(PCWSTR(dev.as_ptr()), i, &mut dd, EDD_GET_DEVICE_INTERFACE_NAME) };
        if !ok.as_bool() {
            break;
        }
        if dd.StateFlags.0 & DISPLAY_DEVICE_ACTIVE != 0 {
            out.push(wide_to_string(&dd.DeviceID));
        }
    }
    out
}

/// Enumerates every physical monitor attached to the desktop.
pub fn enumerate() -> Vec<PhysicalMonitor> {
    let mut hmons: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect_monitor), LPARAM(&mut hmons as *mut _ as isize));
    }

    let mut result = Vec::new();
    for h in hmons {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        let gdi_device = if unsafe { GetMonitorInfoW(h, &mut info as *mut _ as *mut MONITORINFO) }.as_bool() {
            wide_to_string(&info.szDevice)
        } else {
            String::new()
        };
        let paths = if gdi_device.is_empty() { Vec::new() } else { device_paths_for(&gdi_device) };

        let mut count = 0u32;
        if unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(h, &mut count) }.is_err() || count == 0 {
            continue;
        }
        let mut phys = vec![PHYSICAL_MONITOR::default(); count as usize];
        if unsafe { GetPhysicalMonitorsFromHMONITOR(h, &mut phys) }.is_err() {
            continue;
        }
        for (i, p) in phys.iter().enumerate() {
            result.push(PhysicalMonitor {
                handle: p.hPhysicalMonitor,
                description: { let d = p.szPhysicalMonitorDescription; wide_to_string(&d).trim().to_string() },
                device_path: paths.get(i).cloned(),
                gdi_device: gdi_device.clone(),
            });
        }
    }
    result
}

/// `\\?\DISPLAY#GBT2404#5&308dec65&0&UID4352#{guid}` -> `DISPLAY\GBT2404\5&308DEC65&0&UID4352`
pub fn instance_key_from_device_path(path: &str) -> Option<String> {
    let p = path.strip_prefix(r"\\?\").unwrap_or(path);
    let parts: Vec<&str> = p.split('#').collect();
    if parts.len() < 3 {
        return None;
    }
    Some(format!("{}\\{}\\{}", parts[0], parts[1], parts[2]).to_uppercase())
}

/// `DISPLAY\GBT2404\5&308dec65&0&UID4352_0` -> `DISPLAY\GBT2404\5&308DEC65&0&UID4352`
pub fn instance_key_from_wmi(instance: &str) -> String {
    let s = match instance.rfind('_') {
        Some(i) if !instance[i + 1..].is_empty() && instance[i + 1..].chars().all(|c| c.is_ascii_digit()) => {
            &instance[..i]
        }
        _ => instance,
    };
    s.to_uppercase()
}

pub struct DdcBackend {
    pub monitor: PhysicalMonitor,
}

impl MonitorBackend for DdcBackend {
    fn read(&mut self, code: u8) -> anyhow::Result<(u32, u32)> {
        self.monitor.get(code)
    }
    fn write(&mut self, code: u8, value: u32) -> anyhow::Result<()> {
        self.monitor.set(code, value)
    }
    fn settle_time(&self, code: u8) -> Duration {
        // DDC/CI is a slow I2C protocol; monitors drop commands sent back to back.
        if code == vcp::INPUT_SOURCE {
            Duration::from_millis(150)
        } else {
            Duration::from_millis(30)
        }
    }
    fn attempts(&self, code: u8) -> u32 {
        if code == vcp::BRIGHTNESS {
            3
        } else {
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_match_between_device_path_and_wmi() {
        let a = instance_key_from_device_path(
            r"\\?\DISPLAY#GBT2404#5&308dec65&0&UID4352#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}",
        )
        .unwrap();
        let b = instance_key_from_wmi(r"DISPLAY\GBT2404\5&308dec65&0&UID4352_0");
        assert_eq!(a, b);
    }
}
