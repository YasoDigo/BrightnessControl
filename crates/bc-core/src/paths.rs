use std::path::PathBuf;

/// `%LOCALAPPDATA%\BrightnessControl` (shared with the old app; file names differ).
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("BrightnessControl")
}

pub fn settings_file() -> PathBuf {
    data_dir().join("settings.toml")
}

pub fn legacy_settings_file() -> PathBuf {
    data_dir().join("Settings_v2.xml")
}

pub fn log_dir() -> PathBuf {
    data_dir().join("logs")
}
