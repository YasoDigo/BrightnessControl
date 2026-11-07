//! Win32 helpers for the Slint windows: styles, DWM, placement, registry.

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ,
    RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::*;

pub fn hwnd_of(window: &slint::Window) -> Option<HWND> {
    let handle = window.window_handle();
    let raw = handle.window_handle().ok()?.as_raw();
    match raw {
        RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut _)),
        _ => None,
    }
}

const DWMWA_USE_IMMERSIVE_DARK_MODE: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(20);
const DWMWA_WINDOW_CORNER_PREFERENCE: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(33);
const DWMWA_BORDER_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(34);

/// Windows 11 rounded corners + shadow, dark title bar, subtle border.
pub fn dwm_style(hwnd: HWND, dark: bool, round: bool, border: Option<(u8, u8, u8)>) {
    unsafe {
        let d: i32 = dark as i32;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &d as *const _ as _, 4);
        if round {
            let pref: i32 = 2; // DWMWCP_ROUND
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &pref as *const _ as _, 4);
        }
        if let Some((r, g, b)) = border {
            let c: u32 = r as u32 | (g as u32) << 8 | (b as u32) << 16;
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &c as *const _ as _, 4);
        }
    }
}

pub fn cursor_pos() -> POINT {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    p
}

pub struct MonitorArea {
    pub monitor: RECT,
    pub work: RECT,
    pub scale: f32,
}

pub fn monitor_at(p: POINT) -> MonitorArea {
    unsafe {
        let hm: HMONITOR = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(hm, &mut mi);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(hm, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        MonitorArea { monitor: mi.rcMonitor, work: mi.rcWork, scale: dx as f32 / 96.0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Edge {
    Bottom,
    Top,
    Left,
    Right,
}

/// Which side the taskbar is on, judged by the gap between monitor and work area.
pub fn taskbar_edge(a: &MonitorArea) -> Edge {
    if a.work.bottom < a.monitor.bottom {
        Edge::Bottom
    } else if a.work.top > a.monitor.top {
        Edge::Top
    } else if a.work.left > a.monitor.left {
        Edge::Left
    } else if a.work.right < a.monitor.right {
        Edge::Right
    } else {
        Edge::Bottom
    }
}

/// Corner position for a flyout of `size` (physical px) next to the taskbar.
pub fn flyout_position(a: &MonitorArea, w: i32, h: i32) -> (i32, i32) {
    let m = (12.0 * a.scale) as i32;
    let wa = a.work;
    match taskbar_edge(a) {
        Edge::Bottom => (wa.right - w - m, wa.bottom - h - m),
        Edge::Top => (wa.right - w - m, wa.top + m),
        Edge::Left => (wa.left + m, wa.bottom - h - m),
        Edge::Right => (wa.right - w - m, wa.bottom - h - m),
    }
}

pub fn window_rect(hwnd: HWND) -> RECT {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    r
}

pub fn move_window(hwnd: HWND, x: i32, y: i32) {
    unsafe {
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

/// Brings our window to the front. Windows refuses this when another
/// process got the last input; briefly sharing the foreground thread's
/// input state lifts that lock.
pub fn focus(hwnd: HWND) {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    unsafe {
        if SetForegroundWindow(hwnd).as_bool() && GetForegroundWindow() == hwnd {
            return;
        }
        let fg = GetWindowThreadProcessId(GetForegroundWindow(), None);
        let me = GetCurrentThreadId();
        if fg != 0 && fg != me && AttachThreadInput(me, fg, true).as_bool() {
            let _ = BringWindowToTop(hwnd);
            let _ = SetForegroundWindow(hwnd);
            let _ = AttachThreadInput(me, fg, false);
        }
    }
}

pub fn is_foreground(hwnd: HWND) -> bool {
    unsafe { GetForegroundWindow() == hwnd }
}

// ---------------------------------------------------------------- registry

fn reg_dword(key: PCWSTR, value: PCWSTR) -> Option<u32> {
    let mut data = 0u32;
    let mut size = 4u32;
    let r = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key, value, RRF_RT_REG_DWORD, None, Some(&mut data as *mut _ as _), Some(&mut size))
    };
    r.is_ok().then_some(data)
}

pub fn system_uses_light_theme() -> bool {
    reg_dword(w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"), w!("AppsUseLightTheme"))
        .map(|v| v != 0)
        .unwrap_or(false)
}

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("BrightnessControl");

pub fn autostart_enabled() -> bool {
    let mut size = 0u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, Some(&mut size)) }.is_ok()
}

pub fn set_autostart(on: bool) -> anyhow::Result<()> {
    unsafe {
        if on {
            let exe = std::env::current_exe()?;
            let cmd = format!("\"{}\" --tray", exe.display());
            let wide: Vec<u16> = cmd.encode_utf16().chain(Some(0)).collect();
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                RUN_VALUE,
                REG_SZ.0,
                Some(wide.as_ptr() as _),
                (wide.len() * 2) as u32,
            )
            .ok()?;
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
        }
    }
    Ok(())
}

pub fn open_folder(path: &std::path::Path) {
    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
}
