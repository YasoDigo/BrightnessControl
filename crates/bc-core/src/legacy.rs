//! One-time import of the C# app's `Settings_v2.xml`.
//!
//! Old monitor ids (`MON_INTERNAL`, `MON_GenericPnPMonitor_0`) are kept as-is and
//! `legacy_ids_pending` is set; [`resolve_ids`] maps them to the new stable ids
//! once real monitors have been discovered.

use crate::settings::*;
use serde::Deserialize;

#[derive(Deserialize, Default, Debug)]
#[serde(default, rename_all = "PascalCase")]
struct Container {
    current_theme: Option<XTheme>,
    monitor_configs: XMonitorConfigs,
    hotkeys: XHotkeys,
    profiles: XProfiles,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default, rename_all = "PascalCase")]
struct XTheme {
    theme_name: String,
    settings_location: Option<XPoint>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
struct XPoint {
    #[serde(rename = "X")]
    x: i32,
    #[serde(rename = "Y")]
    y: i32,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
struct XMonitorConfigs {
    #[serde(rename = "MonitorConfigData")]
    items: Vec<XMonitorConfig>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default, rename_all = "PascalCase")]
struct XMonitorConfig {
    #[serde(rename = "ID")]
    id: String,
    alias: Option<String>,
    show_audio: bool,
    show_input_selector: bool,
    enabled_inputs: XInts,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
struct XInts {
    #[serde(rename = "int")]
    items: Vec<u8>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
struct XHotkeys {
    #[serde(rename = "HotkeyItem")]
    items: Vec<XHotkey>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default, rename_all = "PascalCase")]
struct XHotkey {
    monitor_id: String,
    is_audio: bool,
    is_up: bool,
    modifier: u32,
    key: u32,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
struct XProfiles {
    #[serde(rename = "ProfileData")]
    items: Vec<XProfile>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default, rename_all = "PascalCase")]
struct XProfile {
    name: String,
    monitor_states: XStates,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
struct XStates {
    #[serde(rename = "MonitorState")]
    items: Vec<XState>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default, rename_all = "PascalCase")]
struct XState {
    #[serde(rename = "MonitorID")]
    monitor_id: String,
    brightness: i32,
    volume: i32,
}

pub fn import(xml: &str) -> anyhow::Result<Settings> {
    let xml = xml.trim_start_matches('\u{feff}');
    let c: Container = quick_xml::de::from_str(xml)?;
    let mut s = Settings { legacy_ids_pending: true, hotkeys: Vec::new(), ..Default::default() };

    let theme_name = c.current_theme.as_ref().map(|t| t.theme_name.as_str()).unwrap_or("Dark");
    s.general.theme = match theme_name {
        "Light" => ThemeMode::Light,
        // Custom colours are gone; the old Custom theme maps to dark.
        _ => ThemeMode::Dark,
    };
    if let Some(p) = c.current_theme.as_ref().and_then(|t| t.settings_location.as_ref()) {
        if p.x != 0 || p.y != 0 {
            s.settings_window = Some([p.x, p.y]);
        }
    }

    for m in &c.monitor_configs.items {
        let cfg = s.monitor_mut(&m.id);
        cfg.alias = m.alias.clone().filter(|a| !a.trim().is_empty());
        cfg.show_volume = m.show_audio;
        cfg.show_input = m.show_input_selector;
        cfg.inputs = m.enabled_inputs.items.clone();
    }

    for (i, p) in c.profiles.items.iter().enumerate() {
        let values = p
            .monitor_states
            .items
            .iter()
            .map(|st| {
                let volume_on = s.monitor(&st.monitor_id).map(|m| m.show_volume).unwrap_or(false);
                ProfileValue {
                    monitor: st.monitor_id.clone(),
                    brightness: st.brightness.clamp(0, 100) as u8,
                    volume: volume_on.then_some(st.volume.clamp(0, 100) as u8),
                }
            })
            .collect();
        s.profiles.push(Profile { id: i as u32 + 1, name: p.name.clone(), values });
    }

    for h in &c.hotkeys.items {
        if h.key == 0 {
            continue;
        }
        let action = match h.monitor_id.as_str() {
            "SYSTEM" => Action::simple(ActionKind::ToggleFlyout),
            "SYSTEM_POPUP" => Action::simple(ActionKind::QuickMenu),
            "MASTER" => Action::simple(if h.is_up { ActionKind::MasterUp } else { ActionKind::MasterDown }),
            id if id.starts_with("PROFILE_") => {
                let name = &id["PROFILE_".len()..];
                match s.profiles.iter().find(|p| p.name == name) {
                    Some(p) => Action::profile(p.id),
                    None => continue,
                }
            }
            id => {
                let kind = match (h.is_audio, h.is_up) {
                    (false, true) => ActionKind::BrightnessUp,
                    (false, false) => ActionKind::BrightnessDown,
                    (true, true) => ActionKind::VolumeUp,
                    (true, false) => ActionKind::VolumeDown,
                };
                Action::monitor(kind, id)
            }
        };
        s.set_hotkey(action, Some(Chord::from_win32(h.modifier, h.key)));
    }

    Ok(s)
}

/// A monitor as seen by the id resolver.
pub struct Known<'a> {
    pub id: &'a str,
    pub internal: bool,
    /// Driver description (`Generic PnP Monitor`) – what the old ids were built from.
    pub description: &'a str,
}

fn sanitize(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).collect()
}

/// Maps old ids to discovered ones and rewrites every reference.
/// Returns false (and changes nothing) when nothing could be resolved yet.
pub fn resolve_ids(s: &mut Settings, monitors: &[Known]) -> bool {
    if !s.legacy_ids_pending || monitors.is_empty() {
        return false;
    }
    let mut legacy: Vec<String> = s.monitors.iter().map(|m| m.id.clone()).collect();
    for p in &s.profiles {
        legacy.extend(p.values.iter().map(|v| v.monitor.clone()));
    }
    legacy.extend(s.hotkeys.iter().filter_map(|h| h.action.monitor.clone()));
    legacy.sort();
    legacy.dedup();

    let externals: Vec<&Known> = monitors.iter().filter(|m| !m.internal).collect();
    let mut map: Vec<(String, String)> = Vec::new();
    let mut used: Vec<&str> = Vec::new();

    // Old external ids: MON_<sanitized description>_<counter>, ordered by counter.
    let mut ext_legacy: Vec<(String, String, u32)> = legacy
        .iter()
        .filter(|id| id.starts_with("MON_") && id.as_str() != "MON_INTERNAL")
        .filter_map(|id| {
            let rest = &id[4..];
            let (name, n) = rest.rsplit_once('_')?;
            Some((id.clone(), name.to_string(), n.parse().ok()?))
        })
        .collect();
    ext_legacy.sort_by_key(|e| e.2);

    for (old, name, _) in &ext_legacy {
        let by_name = externals.iter().find(|m| !used.contains(&m.id) && sanitize(m.description) == *name);
        let pick = by_name.or_else(|| externals.iter().find(|m| !used.contains(&m.id)));
        if let Some(m) = pick {
            used.push(m.id);
            map.push((old.clone(), m.id.to_string()));
        }
    }
    if let Some(m) = monitors.iter().find(|m| m.internal) {
        map.push(("MON_INTERNAL".into(), m.id.to_string()));
    }

    let lookup = |id: &str| map.iter().find(|(o, _)| o == id).map(|(_, n)| n.clone());
    for m in &mut s.monitors {
        if let Some(n) = lookup(&m.id) {
            m.id = n;
        }
    }
    for p in &mut s.profiles {
        for v in &mut p.values {
            if let Some(n) = lookup(&v.monitor) {
                v.monitor = n;
            }
        }
    }
    for h in &mut s.hotkeys {
        if let Some(n) = h.action.monitor.as_deref().and_then(lookup) {
            h.action.monitor = Some(n);
        }
    }
    tracing::info!("legacy ids resolved: {map:?}");
    s.legacy_ids_pending = false;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<SettingsContainer xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xsd="http://www.w3.org/2001/XMLSchema">
  <CurrentTheme>
    <ThemeName>Dark</ThemeName>
    <SettingsLocation><X>678</X><Y>139</Y></SettingsLocation>
    <MonitorColorsList />
    <MonitorAudioColorsList />
  </CurrentTheme>
  <CustomProfile>
    <ThemeName>Custom</ThemeName>
    <BackColorHtml>#FF80FF</BackColorHtml>
    <InputBackColorHtml>Yellow</InputBackColorHtml>
    <MonitorColorsList>
      <ColorMapItem><ID>MON_GenericPnPMonitor_0</ID><ColorHtml>#800040</ColorHtml></ColorMapItem>
    </MonitorColorsList>
    <MonitorAudioColorsList />
  </CustomProfile>
  <MonitorConfigs>
    <MonitorConfigData><ID>MON_INTERNAL</ID><Alias>Laptop Display</Alias><ShowAudio>false</ShowAudio><ShowInputSelector>false</ShowInputSelector><EnabledInputs /></MonitorConfigData>
    <MonitorConfigData><ID>MON_GenericPnPMonitor_0</ID><Alias>GS25F2</Alias><ShowAudio>true</ShowAudio><ShowInputSelector>true</ShowInputSelector>
      <EnabledInputs><int>15</int><int>17</int><int>18</int></EnabledInputs></MonitorConfigData>
  </MonitorConfigs>
  <Hotkeys>
    <HotkeyItem><Id>0</Id><MonitorId>SYSTEM</MonitorId><IsAudio>false</IsAudio><IsUp>true</IsUp><Modifier>2</Modifier><Key>113</Key><DisplayName>Show/Hide Menu</DisplayName></HotkeyItem>
    <HotkeyItem><Id>1</Id><MonitorId>MASTER</MonitorId><IsAudio>false</IsAudio><IsUp>true</IsUp><Modifier>2</Modifier><Key>114</Key><DisplayName>x</DisplayName></HotkeyItem>
    <HotkeyItem><Id>5</Id><MonitorId>MON_GenericPnPMonitor_0</MonitorId><IsAudio>false</IsAudio><IsUp>true</IsUp><Modifier>0</Modifier><Key>0</Key><DisplayName>x</DisplayName></HotkeyItem>
    <HotkeyItem><Id>10</Id><MonitorId>PROFILE_night</MonitorId><IsAudio>false</IsAudio><IsUp>true</IsUp><Modifier>2</Modifier><Key>120</Key><DisplayName>x</DisplayName></HotkeyItem>
    <HotkeyItem><Id>13</Id><MonitorId>SYSTEM_POPUP</MonitorId><IsAudio>false</IsAudio><IsUp>true</IsUp><Modifier>2</Modifier><Key>112</Key><DisplayName>x</DisplayName></HotkeyItem>
  </Hotkeys>
  <Profiles>
    <ProfileData><Name>night</Name><MonitorStates>
      <MonitorState><MonitorID>MON_INTERNAL</MonitorID><Brightness>20</Brightness><Volume>0</Volume></MonitorState>
      <MonitorState><MonitorID>MON_GenericPnPMonitor_0</MonitorID><Brightness>0</Brightness><Volume>100</Volume></MonitorState>
    </MonitorStates></ProfileData>
  </Profiles>
</SettingsContainer>"#;

    #[test]
    fn imports_real_file_shape() {
        let s = import(SAMPLE).unwrap();
        assert_eq!(s.general.theme, ThemeMode::Dark);
        assert_eq!(s.settings_window, Some([678, 139]));
        let ext = s.monitor("MON_GenericPnPMonitor_0").unwrap();
        assert_eq!(ext.alias.as_deref(), Some("GS25F2"));
        assert!(ext.show_volume && ext.show_input);
        assert_eq!(ext.inputs, vec![15, 17, 18]);
        assert_eq!(s.profiles.len(), 1);
        assert_eq!(s.profiles[0].values[0].volume, None);
        assert_eq!(s.profiles[0].values[1].volume, Some(100));
        assert_eq!(s.hotkey_for(&Action::simple(ActionKind::ToggleFlyout)), Some(Chord::from_win32(2, 113)));
        assert_eq!(s.hotkey_for(&Action::simple(ActionKind::QuickMenu)), Some(Chord::from_win32(2, 112)));
        assert_eq!(s.hotkey_for(&Action::profile(1)), Some(Chord::from_win32(2, 120)));
        assert_eq!(s.hotkeys.len(), 4, "unbound (key 0) hotkeys are dropped");
    }

    #[test]
    fn resolves_old_ids_to_discovered_monitors() {
        let mut s = import(SAMPLE).unwrap();
        let known = [
            Known { id: "internal", internal: true, description: "Internal Screen" },
            Known { id: "GBT2404-0000A1B2C3D4", internal: false, description: "Generic PnP Monitor" },
        ];
        assert!(resolve_ids(&mut s, &known));
        assert!(!s.legacy_ids_pending);
        assert!(s.monitor("GBT2404-0000A1B2C3D4").unwrap().show_volume);
        assert_eq!(s.monitor("internal").unwrap().alias.as_deref(), Some("Laptop Display"));
        assert_eq!(s.profiles[0].values[1].monitor, "GBT2404-0000A1B2C3D4");
        // Second call is a no-op.
        assert!(!resolve_ids(&mut s, &known));
    }
}
