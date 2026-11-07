//! Finds every display we can control and gives it a stable identity.

use crate::backend::MonitorBackend;
use crate::ddc::{self, DdcBackend, PhysicalMonitor};
use crate::internal::{self, EdidInfo, InternalBackend};
use crate::vcp;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorKind {
    Internal,
    External,
}

/// What a monitor answered during probing: (current, max) per feature.
#[derive(Debug, Clone, Default)]
pub struct Caps {
    pub brightness: Option<(u32, u32)>,
    pub volume: Option<(u32, u32)>,
    pub input: Option<u32>,
}

pub struct Discovered {
    /// Stable id (`internal`, `GBT2404-0000A1B2C3D4`, ...).
    pub id: String,
    /// Name from EDID (`GS25F2`) or the driver description.
    pub name: String,
    /// Raw driver description, e.g. `Generic PnP Monitor` (used for legacy id mapping).
    pub description: String,
    pub kind: MonitorKind,
    pub caps: Caps,
    pub backend: Box<dyn MonitorBackend>,
}

impl std::fmt::Debug for Discovered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Discovered")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("description", &self.description)
            .field("kind", &self.kind)
            .field("caps", &self.caps)
            .finish()
    }
}

fn read_retry(m: &PhysicalMonitor, code: u8, attempts: u32) -> Option<(u32, u32)> {
    for i in 0..attempts {
        if let Ok(v) = m.get(code) {
            return Some(v);
        }
        if i + 1 < attempts {
            std::thread::sleep(Duration::from_millis(40));
        }
    }
    None
}

fn probe(m: &PhysicalMonitor) -> Caps {
    let brightness = read_retry(m, vcp::BRIGHTNESS, 3);
    if brightness.is_none() {
        return Caps::default();
    }
    std::thread::sleep(Duration::from_millis(30));
    let volume = read_retry(m, vcp::VOLUME, 2).filter(|v| v.1 > 0);
    std::thread::sleep(Duration::from_millis(30));
    let input = read_retry(m, vcp::INPUT_SOURCE, 2).map(|v| v.0);
    Caps { brightness, volume, input }
}

fn sanitize(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).collect()
}

/// Full scan. Blocking (DDC probing takes ~50-200 ms per monitor); call it off the UI thread.
pub fn discover() -> Vec<Discovered> {
    let (edids, panels) = match internal::connect() {
        Ok(con) => (internal::edid_list(&con), internal::internal_panels(&con)),
        Err(e) => {
            tracing::warn!("WMI unavailable: {e}");
            (Vec::new(), Vec::new())
        }
    };
    let edid_for = |key: &str| -> Option<&EdidInfo> {
        edids.iter().find(|e| ddc::instance_key_from_wmi(&e.instance) == key)
    };
    let internal_keys: Vec<String> = panels.iter().map(|(i, _)| ddc::instance_key_from_wmi(i)).collect();

    let mut out = Vec::new();

    // Internal panels first, as in the original layout.
    for (n, (instance, current)) in panels.iter().enumerate() {
        let key = ddc::instance_key_from_wmi(instance);
        let edid = edid_for(&key);
        let id = if n == 0 { "internal".to_string() } else { format!("internal-{}", n + 1) };
        let name = edid.map(|e| e.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| "Laptop Display".into());
        out.push(Discovered {
            id,
            name,
            description: "Internal Screen".into(),
            kind: MonitorKind::Internal,
            caps: Caps { brightness: Some((*current as u32, 100)), volume: None, input: None },
            backend: Box::new(InternalBackend::new(instance.clone())),
        });
    }

    // External monitors: probe in parallel, each on its own thread.
    let physical: Vec<PhysicalMonitor> = ddc::enumerate()
        .into_iter()
        .filter(|m| {
            let key = m.device_path.as_deref().and_then(ddc::instance_key_from_device_path);
            // The laptop panel also shows up as a physical monitor; WMI owns it.
            !matches!(key, Some(k) if internal_keys.contains(&k))
        })
        .collect();

    let probed: Vec<(PhysicalMonitor, Caps)> = std::thread::scope(|s| {
        let handles: Vec<_> = physical
            .into_iter()
            .map(|m| {
                s.spawn(move || {
                    let caps = probe(&m);
                    (m, caps)
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });

    for (m, caps) in probed {
        if caps.brightness.is_none() {
            tracing::info!("skipping {} ({:?}): no DDC/CI response", m.description, m.device_path);
            continue;
        }
        let key = m.device_path.as_deref().and_then(ddc::instance_key_from_device_path);
        let edid = key.as_deref().and_then(edid_for);
        let name = edid
            .map(|e| e.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| if m.description.is_empty() { "Monitor".into() } else { m.description.clone() });
        let mut id = edid
            .and_then(|e| e.stable_id())
            .or_else(|| key.as_ref().map(|k| k.split('\\').skip(1).collect::<Vec<_>>().join("-")))
            .unwrap_or_else(|| format!("{}-{}", sanitize(&m.description), sanitize(&m.gdi_device)));
        // Two identical monitors without serials: keep ids unique.
        let mut n = 2;
        while out.iter().any(|d: &Discovered| d.id == id) {
            id = format!("{id}#{n}");
            n += 1;
        }
        out.push(Discovered {
            id,
            name,
            description: m.description.clone(),
            kind: MonitorKind::External,
            caps,
            backend: Box::new(DdcBackend { monitor: m }),
        });
    }

    tracing::info!("discovered {} monitor(s): {:?}", out.len(), out.iter().map(|d| &d.id).collect::<Vec<_>>());
    out
}
