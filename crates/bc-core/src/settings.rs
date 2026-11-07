//! Persistent settings (`settings.toml`).

use serde::{Deserialize, Serialize};
use std::path::Path;

pub const VERSION: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    System,
    /// `custom` was the old fourth mode; it now falls back to dark.
    #[serde(alias = "custom")]
    Dark,
    Light,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    pub theme: ThemeMode,
    pub start_with_windows: bool,
    pub show_osd: bool,
    pub snap_to_ten: bool,
    pub tray_scroll: bool,
}

impl Default for General {
    fn default() -> Self {
        Self { theme: ThemeMode::System, start_with_windows: false, show_osd: true, snap_to_ten: true, tray_scroll: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MonitorConfig {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    pub show_volume: bool,
    pub show_input: bool,
    /// Input codes offered in the UI. Empty = all.
    pub inputs: Vec<u8>,
    /// Index into the UI palette. None = picked automatically.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    ToggleFlyout,
    QuickMenu,
    MasterUp,
    MasterDown,
    BrightnessUp,
    BrightnessDown,
    VolumeUp,
    VolumeDown,
    ApplyProfile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub struct Action {
    pub kind: ActionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<u32>,
}

impl Action {
    pub fn simple(kind: ActionKind) -> Self {
        Self { kind, monitor: None, profile: None }
    }
    pub fn monitor(kind: ActionKind, id: &str) -> Self {
        Self { kind, monitor: Some(id.to_string()), profile: None }
    }
    pub fn profile(id: u32) -> Self {
        Self { kind: ActionKind::ApplyProfile, monitor: None, profile: Some(id) }
    }
    /// Key used for repeat acceleration.
    pub fn key(&self) -> String {
        format!("{:?}/{:?}/{:?}", self.kind, self.monitor, self.profile)
    }
}

pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Hash)]
#[serde(into = "String", try_from = "String")]
pub struct Chord {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    /// Win32 virtual-key code.
    pub key: u32,
}

impl Chord {
    pub fn from_win32(modifiers: u32, key: u32) -> Self {
        Self {
            alt: modifiers & MOD_ALT != 0,
            ctrl: modifiers & MOD_CONTROL != 0,
            shift: modifiers & MOD_SHIFT != 0,
            win: modifiers & MOD_WIN != 0,
            key,
        }
    }
    pub fn modifiers(&self) -> u32 {
        (if self.alt { MOD_ALT } else { 0 })
            | (if self.ctrl { MOD_CONTROL } else { 0 })
            | (if self.shift { MOD_SHIFT } else { 0 })
            | (if self.win { MOD_WIN } else { 0 })
    }
    pub fn label(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".into());
        }
        if self.alt {
            parts.push("Alt".into());
        }
        if self.shift {
            parts.push("Shift".into());
        }
        if self.win {
            parts.push("Win".into());
        }
        parts.push(vk_name(self.key));
        parts.join(" + ")
    }
}

impl From<Chord> for String {
    fn from(c: Chord) -> String {
        c.label()
    }
}

impl TryFrom<String> for Chord {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        Chord::parse(&s).ok_or_else(|| format!("invalid hotkey '{s}'"))
    }
}

impl Chord {
    /// Parses what [`Chord::label`] produces, e.g. `Ctrl + Shift + F2`.
    pub fn parse(s: &str) -> Option<Chord> {
        let mut c = Chord::default();
        let parts: Vec<&str> = s.split(" + ").map(str::trim).collect();
        let (key, mods) = parts.split_last()?;
        for m in mods {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" => c.ctrl = true,
                "alt" => c.alt = true,
                "shift" => c.shift = true,
                "win" => c.win = true,
                _ => return None,
            }
        }
        c.key = (1..=0xFE).find(|&vk| vk_name(vk).eq_ignore_ascii_case(key))?;
        Some(c)
    }
}

pub fn vk_name(vk: u32) -> String {
    match vk {
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        0x30..=0x39 | 0x41..=0x5A => char::from_u32(vk).unwrap().to_string(),
        0x60..=0x69 => format!("Num {}", vk - 0x60),
        0x6A => "Num *".into(),
        0x6B => "Num +".into(),
        0x6D => "Num -".into(),
        0x6E => "Num .".into(),
        0x6F => "Num /".into(),
        0x08 => "Backspace".into(),
        0x09 => "Tab".into(),
        0x0D => "Enter".into(),
        0x13 => "Pause".into(),
        0x1B => "Esc".into(),
        0x20 => "Space".into(),
        0x21 => "Page Up".into(),
        0x22 => "Page Down".into(),
        0x23 => "End".into(),
        0x24 => "Home".into(),
        0x25 => "Left".into(),
        0x26 => "Up".into(),
        0x27 => "Right".into(),
        0x28 => "Down".into(),
        0x2C => "Print Screen".into(),
        0x2D => "Insert".into(),
        0x2E => "Delete".into(),
        0xBA => ";".into(),
        0xBB => "=".into(),
        0xBC => ",".into(),
        0xBD => "-".into(),
        0xBE => ".".into(),
        0xBF => "/".into(),
        0xC0 => "`".into(),
        0xDB => "[".into(),
        0xDC => "\\".into(),
        0xDD => "]".into(),
        0xDE => "'".into(),
        0xE2 => "<".into(),
        0xAD => "Mute".into(),
        0xAE => "Volume Down".into(),
        0xAF => "Volume Up".into(),
        0xB0 => "Next Track".into(),
        0xB1 => "Prev Track".into(),
        0xB3 => "Play/Pause".into(),
        _ => format!("Key {vk:#04X}"),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hotkey {
    pub action: Action,
    pub chord: Chord,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProfileValue {
    pub monitor: String,
    pub brightness: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Profile {
    pub id: u32,
    pub name: String,
    pub values: Vec<ProfileValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    /// Ids still in the old `MON_<name>_<n>` form; resolved after the first scan.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub legacy_ids_pending: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings_window: Option<[i32; 2]>,
    pub general: General,
    pub monitors: Vec<MonitorConfig>,
    pub hotkeys: Vec<Hotkey>,
    pub profiles: Vec<Profile>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: VERSION,
            legacy_ids_pending: false,
            settings_window: None,
            general: General::default(),
            monitors: Vec::new(),
            hotkeys: vec![
                Hotkey { action: Action::simple(ActionKind::MasterUp), chord: Chord::from_win32(MOD_CONTROL, 0x71) },
                Hotkey { action: Action::simple(ActionKind::MasterDown), chord: Chord::from_win32(MOD_CONTROL, 0x70) },
            ],
            profiles: Vec::new(),
        }
    }
}

impl Settings {
    pub fn monitor(&self, id: &str) -> Option<&MonitorConfig> {
        self.monitors.iter().find(|m| m.id == id)
    }

    pub fn monitor_mut(&mut self, id: &str) -> &mut MonitorConfig {
        if let Some(i) = self.monitors.iter().position(|m| m.id == id) {
            &mut self.monitors[i]
        } else {
            self.monitors.push(MonitorConfig { id: id.to_string(), ..Default::default() });
            self.monitors.last_mut().unwrap()
        }
    }

    pub fn hotkey_for(&self, action: &Action) -> Option<Chord> {
        self.hotkeys.iter().find(|h| &h.action == action).map(|h| h.chord)
    }

    /// Sets (or clears with `None`) the chord bound to `action`.
    pub fn set_hotkey(&mut self, action: Action, chord: Option<Chord>) {
        self.hotkeys.retain(|h| h.action != action);
        if let Some(chord) = chord.filter(|c| c.key != 0) {
            self.hotkeys.push(Hotkey { action, chord });
        }
    }

    /// Hotkeys bound to the same chord as another one.
    pub fn conflicts(&self) -> Vec<Chord> {
        let mut seen = Vec::new();
        let mut dup = Vec::new();
        for h in &self.hotkeys {
            if seen.contains(&h.chord) {
                if !dup.contains(&h.chord) {
                    dup.push(h.chord);
                }
            } else {
                seen.push(h.chord);
            }
        }
        dup
    }

    pub fn next_profile_id(&self) -> u32 {
        self.profiles.iter().map(|p| p.id).max().unwrap_or(0) + 1
    }

    pub fn remove_profile(&mut self, id: u32) {
        self.profiles.retain(|p| p.id != id);
        self.hotkeys.retain(|h| h.action.profile != Some(id));
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let mut s: Settings = toml::from_str(&text)?;
        s.version = VERSION;
        Ok(s)
    }

    /// Atomic save: write a temp file, then rename over the real one.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_toml() {
        let mut s = Settings::default();
        s.monitors.push(MonitorConfig { id: "GBT2404-1".into(), alias: Some("Desk".into()), show_volume: true, inputs: vec![15, 17], ..Default::default() });
        s.profiles.push(Profile { id: 1, name: "night".into(), values: vec![ProfileValue { monitor: "internal".into(), brightness: 20, volume: None }] });
        s.set_hotkey(Action::profile(1), Some(Chord::from_win32(MOD_CONTROL, 0x77)));
        let text = toml::to_string_pretty(&s).unwrap();
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn chord_label() {
        assert_eq!(Chord::from_win32(MOD_CONTROL | MOD_SHIFT, 0x71).label(), "Ctrl + Shift + F2");
        assert_eq!(Chord::from_win32(MOD_WIN, 0x42).label(), "Win + B");
        for c in [Chord::from_win32(MOD_CONTROL | MOD_ALT, 0x70), Chord::from_win32(0, 0xBB), Chord::from_win32(MOD_SHIFT, 0x21)] {
            assert_eq!(Chord::parse(&c.label()), Some(c));
        }
    }

    #[test]
    fn conflicts_and_profile_removal() {
        let mut s = Settings::default();
        s.set_hotkey(Action::profile(7), Some(Chord::from_win32(MOD_CONTROL, 0x71)));
        assert_eq!(s.conflicts(), vec![Chord::from_win32(MOD_CONTROL, 0x71)]);
        s.remove_profile(7);
        assert!(s.conflicts().is_empty());
    }
}
