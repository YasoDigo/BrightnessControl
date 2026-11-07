//! Hidden message window on its own thread: tray icon, global hotkeys,
//! tray mouse-wheel, display / power / theme notifications.
//!
//! Events are delivered through a callback (which posts them to the Slint
//! event loop); commands are queued and the window is poked with a message.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, VK_CONTROL,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconGetRect, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NIM_SETVERSION, NIN_SELECT, NOTIFYICONDATAW, NOTIFYICONIDENTIFIER, NOTIFYICON_VERSION_4,
};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const CLASS_NAME: PCWSTR = w!("BrightnessControlRsShell");

const WM_APP_TRAY: u32 = WM_APP + 1;
const WM_APP_CMD: u32 = WM_APP + 2;
const WM_APP_WHEEL: u32 = WM_APP + 3;
/// Sent by a second instance: "show yourself".
pub const WM_APP_SHOW: u32 = WM_APP + 4;
const TRAY_ID: u32 = 1;
const NIN_KEYSELECT: u32 = NIN_SELECT | 0x1; // NINF_KEY
const TIMER_HOVER: usize = 1;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ShellEvent {
    TrayLeft,
    TrayRight,
    TrayWheel { up: bool, ctrl: bool },
    Hotkey(i32),
    DisplayChanged,
    Resumed,
    ThemeChanged,
    /// 0 = panel, 1 = quick menu, 2 = settings.
    ShowRequested(usize),
}

pub enum Cmd {
    Tooltip(String),
    /// (id, modifiers, vk). Replies with the ids that failed to register.
    Hotkeys(Vec<(i32, u32, u32)>, mpsc::Sender<Vec<i32>>),
    TrayScroll(bool),
    Quit,
}

type Callback = Box<dyn Fn(ShellEvent) + Send + Sync>;

struct State {
    callback: Callback,
    queue: Mutex<VecDeque<Cmd>>,
    hotkeys: Mutex<Vec<i32>>,
    tray_scroll: std::sync::atomic::AtomicBool,
    taskbar_created: u32,
    icon: AtomicIsize,
    tip: Mutex<String>,
}

static STATE: OnceLock<State> = OnceLock::new();
static HWND_VAL: AtomicIsize = AtomicIsize::new(0);
static HOOK: AtomicIsize = AtomicIsize::new(0);
static TRAY_RECT: Mutex<RECT> = Mutex::new(RECT { left: 0, top: 0, right: 0, bottom: 0 });

fn hwnd() -> HWND {
    HWND(HWND_VAL.load(Ordering::SeqCst) as *mut _)
}

#[derive(Clone)]
pub struct Shell;

impl Shell {
    pub fn start(callback: impl Fn(ShellEvent) + Send + Sync + 'static) -> Shell {
        let (ready_tx, ready_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("shell".into())
            .spawn(move || run(Box::new(callback), ready_tx))
            .expect("spawn shell thread");
        let _ = ready_rx.recv();
        Shell
    }

    fn send(&self, cmd: Cmd) {
        if let Some(s) = STATE.get() {
            s.queue.lock().unwrap().push_back(cmd);
            unsafe {
                let _ = PostMessageW(Some(hwnd()), WM_APP_CMD, WPARAM(0), LPARAM(0));
            }
        }
    }

    pub fn set_tooltip(&self, text: &str) {
        self.send(Cmd::Tooltip(text.to_string()));
    }

    pub fn set_tray_scroll(&self, on: bool) {
        self.send(Cmd::TrayScroll(on));
    }

    /// Registers the given hotkeys (replacing all previous ones). Returns failed ids.
    pub fn register_hotkeys(&self, keys: Vec<(i32, u32, u32)>) -> Vec<i32> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Hotkeys(keys, tx));
        rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap_or_default()
    }

    pub fn quit(&self) {
        self.send(Cmd::Quit);
    }
}

fn emit(ev: ShellEvent) {
    if let Some(s) = STATE.get() {
        (s.callback)(ev);
    }
}

fn hinstance() -> HINSTANCE {
    unsafe { GetModuleHandleW(None).map(|h| HINSTANCE(h.0)).unwrap_or_default() }
}

fn load_icon() -> HICON {
    unsafe {
        let cx = GetSystemMetrics(SM_CXSMICON);
        let cy = GetSystemMetrics(SM_CYSMICON);
        // Resource id 1 is embedded by build.rs (winresource).
        match LoadImageW(Some(hinstance()), PCWSTR(1 as _), IMAGE_ICON, cx, cy, LR_DEFAULTCOLOR) {
            Ok(h) => HICON(h.0),
            Err(_) => LoadIconW(None, IDI_APPLICATION).unwrap_or_default(),
        }
    }
}

fn fill_tip(nid: &mut NOTIFYICONDATAW, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().take(nid.szTip.len() - 1).collect();
    nid.szTip = [0; 128];
    nid.szTip[..wide.len()].copy_from_slice(&wide);
}

fn tray_data() -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd(),
        uID: TRAY_ID,
        ..Default::default()
    }
}

fn add_tray_icon() {
    let s = STATE.get().unwrap();
    let mut nid = tray_data();
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
    nid.uCallbackMessage = WM_APP_TRAY;
    nid.hIcon = HICON(s.icon.load(Ordering::SeqCst) as *mut _);
    fill_tip(&mut nid, &s.tip.lock().unwrap());
    unsafe {
        if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
            tracing::warn!("Shell_NotifyIcon(NIM_ADD) failed");
        }
        nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
    }
}

fn tray_icon_rect() -> Option<RECT> {
    let id = NOTIFYICONIDENTIFIER {
        cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
        hWnd: hwnd(),
        uID: TRAY_ID,
        ..Default::default()
    };
    unsafe { Shell_NotifyIconGetRect(&id).ok() }
}

/// Public for the UI: where the tray icon is (to anchor popups).
pub fn tray_rect() -> Option<RECT> {
    tray_icon_rect()
}

fn inside(r: &RECT, p: POINT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && wparam.0 as u32 == WM_MOUSEWHEEL {
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let rect = *TRAY_RECT.lock().unwrap();
        if inside(&rect, info.pt) {
            let delta = ((info.mouseData >> 16) & 0xFFFF) as u16 as i16;
            unsafe {
                let _ = PostMessageW(Some(hwnd()), WM_APP_WHEEL, WPARAM(delta as u16 as usize), LPARAM(0));
            }
            return LRESULT(1); // swallow: the taskbar must not scroll
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// The low-level hook is only installed while the cursor is over the tray
/// icon (the C# version kept a global hook alive all the time).
fn start_hover_tracking() {
    let s = STATE.get().unwrap();
    if !s.tray_scroll.load(Ordering::SeqCst) {
        return;
    }
    if let Some(r) = tray_icon_rect() {
        *TRAY_RECT.lock().unwrap() = r;
    }
    if HOOK.load(Ordering::SeqCst) == 0 {
        unsafe {
            if let Ok(h) = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), Some(hinstance()), 0) {
                HOOK.store(h.0 as isize, Ordering::SeqCst);
                SetTimer(Some(hwnd()), TIMER_HOVER, 150, None);
            }
        }
    }
}

fn stop_hover_tracking() {
    let h = HOOK.swap(0, Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(HHOOK(h as *mut _));
            let _ = KillTimer(Some(hwnd()), TIMER_HOVER);
        }
    }
}

fn process_commands() -> bool {
    let s = STATE.get().unwrap();
    loop {
        let cmd = s.queue.lock().unwrap().pop_front();
        let Some(cmd) = cmd else { return true };
        match cmd {
            Cmd::Tooltip(t) => {
                *s.tip.lock().unwrap() = t.clone();
                let mut nid = tray_data();
                nid.uFlags = NIF_TIP | NIF_SHOWTIP;
                fill_tip(&mut nid, &t);
                unsafe {
                    let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
                }
            }
            Cmd::Hotkeys(keys, reply) => {
                let mut registered = s.hotkeys.lock().unwrap();
                for id in registered.drain(..) {
                    unsafe {
                        let _ = UnregisterHotKey(Some(hwnd()), id);
                    }
                }
                let mut failed = Vec::new();
                for (id, mods, vk) in keys {
                    match unsafe { RegisterHotKey(Some(hwnd()), id, HOT_KEY_MODIFIERS(mods), vk) } {
                        Ok(()) => registered.push(id),
                        Err(e) => {
                            tracing::warn!("RegisterHotKey id={id} mods={mods:#x} vk={vk:#x} failed: {e}");
                            failed.push(id);
                        }
                    }
                }
                let _ = reply.send(failed);
            }
            Cmd::TrayScroll(on) => {
                s.tray_scroll.store(on, Ordering::SeqCst);
                if !on {
                    stop_hover_tracking();
                }
            }
            Cmd::Quit => return false,
        }
    }
}

unsafe extern "system" fn wndproc(hwnd_: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let s = STATE.get();
    match msg {
        WM_APP_TRAY => {
            let event = (lparam.0 & 0xFFFF) as u32;
            match event {
                NIN_SELECT | NIN_KEYSELECT => emit(ShellEvent::TrayLeft),
                WM_CONTEXTMENU => emit(ShellEvent::TrayRight),
                WM_MOUSEMOVE => start_hover_tracking(),
                _ => {}
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_HOVER => {
            let mut p = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut p);
            }
            let rect = tray_icon_rect();
            if let Some(r) = rect {
                *TRAY_RECT.lock().unwrap() = r;
            }
            if !rect.map(|r| inside(&r, p)).unwrap_or(false) {
                stop_hover_tracking();
            }
            LRESULT(0)
        }
        WM_APP_WHEEL => {
            let delta = wparam.0 as u16 as i16;
            let ctrl = unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) } as u16 & 0x8000 != 0;
            emit(ShellEvent::TrayWheel { up: delta > 0, ctrl });
            LRESULT(0)
        }
        WM_APP_CMD => {
            if !process_commands() {
                stop_hover_tracking();
                let nid = tray_data();
                unsafe {
                    let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
                    let _ = DestroyWindow(hwnd_);
                }
            }
            LRESULT(0)
        }
        WM_APP_SHOW => {
            emit(ShellEvent::ShowRequested(wparam.0));
            LRESULT(0)
        }
        WM_HOTKEY => {
            emit(ShellEvent::Hotkey(wparam.0 as i32));
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            emit(ShellEvent::DisplayChanged);
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            // PBT_APMRESUMEAUTOMATIC / PBT_APMRESUMESUSPEND
            if wparam.0 == 0x12 || wparam.0 == 0x7 {
                emit(ShellEvent::Resumed);
            }
            LRESULT(1)
        }
        WM_SETTINGCHANGE => {
            if lparam.0 != 0 {
                let s = unsafe { PCWSTR(lparam.0 as *const u16).to_string() }.unwrap_or_default();
                if s == "ImmersiveColorSet" {
                    emit(ShellEvent::ThemeChanged);
                }
            }
            LRESULT(0)
        }
        WM_DWMCOLORIZATIONCOLORCHANGED => {
            emit(ShellEvent::ThemeChanged);
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => {
            if let Some(s) = s {
                if msg == s.taskbar_created && msg != 0 {
                    // Explorer restarted: the icon is gone, add it again.
                    add_tray_icon();
                    return LRESULT(0);
                }
            }
            unsafe { DefWindowProcW(hwnd_, msg, wparam, lparam) }
        }
    }
}

fn run(callback: Callback, ready: mpsc::Sender<()>) {
    unsafe {
        let hinst = hinstance();
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst, lpszClassName: CLASS_NAME, ..Default::default() };
        RegisterClassW(&wc);
        let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));

        let _ = STATE.set(State {
            callback,
            queue: Mutex::new(VecDeque::new()),
            hotkeys: Mutex::new(Vec::new()),
            tray_scroll: std::sync::atomic::AtomicBool::new(true),
            taskbar_created,
            icon: AtomicIsize::new(0),
            tip: Mutex::new("Brightness Control".into()),
        });

        // A top-level (not message-only) window: it needs broadcasts such as
        // WM_DISPLAYCHANGE, WM_SETTINGCHANGE and TaskbarCreated.
        let h = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            CLASS_NAME,
            w!("Brightness Control"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst),
            None,
        )
        .expect("create shell window");
        HWND_VAL.store(h.0 as isize, Ordering::SeqCst);
        STATE.get().unwrap().icon.store(load_icon().0 as isize, Ordering::SeqCst);
        add_tray_icon();
        let _ = ready.send(());

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Finds a running instance and asks it to show its flyout.
pub fn signal_existing_instance(what: usize) -> bool {
    unsafe {
        match FindWindowW(CLASS_NAME, None) {
            Ok(h) if !h.is_invalid() => {
                // We are the foreground process (the user just launched us):
                // let the running instance bring its panel to the front.
                let mut pid = 0u32;
                GetWindowThreadProcessId(h, Some(&mut pid));
                let _ = AllowSetForegroundWindow(pid);
                let _ = PostMessageW(Some(h), WM_APP_SHOW, WPARAM(what), LPARAM(0));
                true
            }
            _ => false,
        }
    }
}
