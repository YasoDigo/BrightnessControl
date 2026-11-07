#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

slint::include_modules!();

mod app;
mod keys;
mod shell;
mod theme;
mod winutil;

use bc_core::{legacy, paths, settings::Settings};
use windows::core::w;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Threading::CreateMutexW;

fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let dir = paths::log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let appender = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("bc")
        .filename_suffix("log")
        .max_log_files(7)
        .build(&dir)
        .ok()?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_env("BC_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(writer).with_ansi(false))
        .with(fmt::layer().with_writer(std::io::stderr))
        .init();
    std::panic::set_hook(Box::new(|info| {
        tracing::error!("panic: {info}");
    }));
    Some(guard)
}

/// settings.toml, or a one-time import of the old app's Settings_v2.xml.
fn load_settings() -> Settings {
    let path = paths::settings_file();
    if path.exists() {
        match Settings::load(&path) {
            Ok(s) => return s,
            Err(e) => {
                tracing::error!("settings.toml is invalid ({e:#}); keeping a copy and using defaults");
                let _ = std::fs::copy(&path, path.with_extension("toml.bad"));
            }
        }
    }
    let legacy_path = paths::legacy_settings_file();
    if let Ok(xml) = std::fs::read_to_string(&legacy_path) {
        match legacy::import(&xml) {
            Ok(s) => {
                tracing::info!("imported settings from {}", legacy_path.display());
                let _ = s.save(&path);
                return s;
            }
            Err(e) => tracing::warn!("could not import {}: {e:#}", legacy_path.display()),
        }
    }
    Settings::default()
}

fn main() {
    let _guard = init_logging();
    tracing::info!("Brightness Control {} starting", env!("CARGO_PKG_VERSION"));

    // Single instance: a second launch just opens the running one's panel.
    unsafe {
        let _mutex = CreateMutexW(None, true, w!("Local\\BrightnessControlRs-3c8f1e")).ok();
        if GetLastError() == ERROR_ALREADY_EXISTS {
            // `--settings` / `--quick` open those instead of the panel.
            let args: Vec<String> = std::env::args().collect();
            let what = match args.get(1).map(String::as_str) {
                Some("--quick") => 1,
                Some("--up") => 10,
                Some("--down") => 11,
                Some("--settings") => 2 + args.get(2).and_then(|p| p.parse::<usize>().ok()).unwrap_or(0),
                Some("--tray") => return,
                _ => 0,
            };
            shell::signal_existing_instance(what);
            return;
        }
        let _ = _mutex; // the handle stays open for the process lifetime
    }

    let selected = slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name(std::env::var("BC_RENDERER").unwrap_or_else(|_| "software".into()))
        // Never steal focus when a window is first created (OSD, prewarm).
        .with_winit_window_attributes_hook(|attrs| attrs.with_active(false))
        .select();
    if let Err(e) = selected {
        tracing::error!("UI backend: {e}");
        return;
    }

    let settings = load_settings();
    if let Err(e) = app::run(settings) {
        tracing::error!("fatal: {e:#}");
    }
    tracing::info!("bye");
}
