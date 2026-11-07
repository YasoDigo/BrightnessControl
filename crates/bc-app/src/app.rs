//! Glue between the core [`Session`], the Win32 shell and the Slint windows.
//!
//! One popup (the light columns), a themed tray menu, and a settings page
//! whose changes apply and save immediately. There is no on-screen indicator.

use crate::shell::{Shell, ShellEvent};
use crate::theme::{self, Colors};
use crate::{apply_theme, keys, winutil};
use crate::{
    DisplayCfg, Flyout, InputToggle, InputToggleRow, MenuInput, MenuInputGroup, MenuInputRow, MenuProfile, MenuProfileRow, MonitorRow, ProfileCfg,
    SettingsWindow, ShortcutRow, TrayMenu,
};
use bc_core::discovery::{self, Discovered};
use bc_core::session::{Outcome, Session};
use bc_core::settings::*;
use bc_core::worker::Event;
use bc_core::{paths, vcp};
use slint::{ComponentHandle, LogicalSize, Model, ModelRc, PhysicalPosition, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::WM_ACTIVATE;

thread_local! {
    static APP: RefCell<Option<Rc<RefCell<App>>>> = const { RefCell::new(None) };
}

/// Runs `f` with the app. If the app is already borrowed (re-entrant call
/// from inside a Slint/Win32 callback), the call is deferred to the next
/// event-loop turn instead of panicking.
pub fn with_app(f: impl FnOnce(&mut App) + 'static) {
    let app = APP.with(|a| a.borrow().clone());
    let Some(app) = app else { return };
    let mut f = Some(f);
    if let Ok(mut a) = app.try_borrow_mut() {
        (f.take().unwrap())(&mut a);
    }
    if let Some(f) = f {
        Timer::single_shot(Duration::ZERO, move || {
            if let Ok(mut a) = app.try_borrow_mut() {
                f(&mut a)
            }
        });
    }
}

const VERSION: &str = concat!("Version ", env!("CARGO_PKG_VERSION"));

/// Settings-window state that is not part of the saved settings.
#[derive(Default)]
struct Editing {
    actions: Vec<Action>,
    recording: Option<usize>,
}

pub struct App {
    session: Session,
    shell: Shell,
    flyout: Flyout,
    menu: TrayMenu,
    menu_hwnd: Option<HWND>,
    menu_profiles: Vec<u32>,
    menu_last_hide: Instant,
    menu_opened: Instant,
    menu_watch: Timer,
    settings_win: Option<SettingsWindow>,
    editing: Editing,
    monitors_model: Rc<VecModel<MonitorRow>>,
    hotkey_actions: Vec<Action>,
    failed_hotkeys: Vec<Action>,
    hwnd: Option<HWND>,
    last_hide: Instant,
    rescan_timer: Timer,
    scanning: bool,
    rescan_pending: bool,
    colors: Colors,
}

// ------------------------------------------------------------------ startup

pub fn run(settings: Settings) -> anyhow::Result<()> {
    let flyout = Flyout::new()?;
    let menu = TrayMenu::new()?;

    let shell = Shell::start(|ev| {
        let _ = slint::invoke_from_event_loop(move || with_app(move |a| a.on_shell(ev)));
    });

    let sink: bc_core::worker::Sink = std::sync::Arc::new(|ev: Event| {
        let _ = slint::invoke_from_event_loop(move || with_app(move |a| a.on_worker(ev)));
    });

    let monitors_model = Rc::new(VecModel::default());
    flyout.set_monitors(ModelRc::from(monitors_model.clone()));
    let colors = theme::resolve(&settings);
    shell.set_tray_scroll(settings.general.tray_scroll);

    let app = App {
        session: Session::new(settings, sink),
        shell,
        flyout,
        menu,
        menu_hwnd: None,
        menu_profiles: Vec::new(),
        menu_last_hide: Instant::now() - Duration::from_secs(5),
        menu_opened: Instant::now(),
        menu_watch: Timer::default(),
        settings_win: None,
        editing: Editing::default(),
        monitors_model,
        hotkey_actions: Vec::new(),
        failed_hotkeys: Vec::new(),
        hwnd: None,
        last_hide: Instant::now() - Duration::from_secs(5),
        rescan_timer: Timer::default(),
        scanning: false,
        rescan_pending: false,
        colors,
    };
    APP.with(|a| *a.borrow_mut() = Some(Rc::new(RefCell::new(app))));
    with_app(|a| {
        a.wire_callbacks();
        a.apply_theme();
        a.register_hotkeys();
        a.start_scan();
    });
    // Windows can only be created once the event loop runs.
    Timer::single_shot(Duration::from_millis(1), || with_app(|a| a.prewarm()));

    slint::run_event_loop_until_quit()?;
    with_app(|a| {
        a.session.shutdown();
        a.shell.quit();
    });
    Ok(())
}

fn save_settings(s: &Settings) {
    if let Err(e) = s.save(&paths::settings_file()) {
        tracing::error!("saving settings failed: {e:#}");
    }
}

/// The saved profile with this name (case-insensitive, ignoring spaces around).
fn profile_named<'a>(s: &'a Settings, name: &str) -> Option<&'a Profile> {
    let name = name.trim();
    s.profiles.iter().find(|p| p.name.trim().eq_ignore_ascii_case(name))
}

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(v)))
}

unsafe extern "system" fn popup_subclass(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _data: usize,
) -> LRESULT {
    if msg == WM_ACTIVATE && (wparam.0 & 0xFFFF) == 0 {
        if id == 2 {
            Timer::single_shot(Duration::ZERO, || with_app(|a| a.hide_menu()));
        } else {
            Timer::single_shot(Duration::ZERO, || with_app(|a| a.hide_flyout()));
        }
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

impl App {
    fn wire_callbacks(&mut self) {
        let f = &self.flyout;
        f.on_brightness(|i, v| with_app(move |a| a.ui_brightness(i as usize, v)));
        f.on_volume(|i, v| with_app(move |a| a.ui_volume(i as usize, v)));
        f.on_master_changed(|v| with_app(move |a| a.ui_master(v)));
        f.on_wheel(|i, vol, up, ctrl| with_app(move |a| a.ui_wheel(i, vol, up, ctrl)));
        f.on_settings(|| {
            with_app(|a| {
                a.hide_flyout();
                a.open_settings();
            })
        });
        f.on_dismiss(|| with_app(|a| a.hide_flyout()));
        f.on_content_resized(|| with_app(|a| a.resize_flyout()));

        let m = &self.menu;
        m.on_profile(|i| {
            with_app(move |a| {
                a.hide_menu();
                if let Some(id) = a.menu_profiles.get(i as usize).copied() {
                    let o = a.session.run_action(&Action::profile(id));
                    a.handle_outcome(o);
                }
            })
        });
        m.on_input(|monitor, code| {
            with_app(move |a| {
                a.hide_menu();
                a.ui_input(monitor as usize, code as u8);
            })
        });
        m.on_settings(|| {
            with_app(|a| {
                // Open first so the focus stays with us when the menu goes.
                a.open_settings();
            })
        });
        m.on_quit(|| {
            let _ = slint::quit_event_loop();
        });
        m.on_dismiss(|| with_app(|a| a.hide_menu()));
    }

    /// Creates the popup once (off-screen) and applies its styles, so later
    /// shows never get a taskbar button.
    fn prewarm(&mut self) {
        let w = self.flyout.window();
        w.set_position(PhysicalPosition::new(-32000, -32000));
        let _ = self.flyout.show();
        self.ensure_styles();
        let _ = self.flyout.hide();
        let w = self.menu.window();
        w.set_position(PhysicalPosition::new(-32000, -32000));
        let _ = self.menu.show();
        self.ensure_menu_styles();
        let _ = self.menu.hide();
        tracing::info!("window ready");
    }

    fn ensure_styles(&mut self) {
        let w = self.flyout.window();
        let Some(h) = winutil::hwnd_of(w) else { return };
        if self.hwnd == Some(h) {
            return;
        }
        self.hwnd = Some(h);
        {
            use slint::winit_030::winit::platform::windows::WindowExtWindows;
            use slint::winit_030::WinitWindowAccessor;
            w.with_winit_window(|ww| ww.set_skip_taskbar(true));
        }
        winutil::dwm_style(h, self.colors.dark, true, None);
        unsafe {
            let _ = SetWindowSubclass(h, Some(popup_subclass), 1, 0);
        }
    }

    fn ensure_menu_styles(&mut self) {
        let w = self.menu.window();
        let Some(h) = winutil::hwnd_of(w) else { return };
        if self.menu_hwnd == Some(h) {
            return;
        }
        self.menu_hwnd = Some(h);
        {
            use slint::winit_030::winit::platform::windows::WindowExtWindows;
            use slint::winit_030::WinitWindowAccessor;
            w.with_winit_window(|ww| ww.set_skip_taskbar(true));
        }
        winutil::dwm_style(h, self.colors.dark, true, None);
        unsafe {
            let _ = SetWindowSubclass(h, Some(popup_subclass), 2, 0);
        }
    }

    // -------------------------------------------------------------- theme

    fn apply_theme(&mut self) {
        self.colors = theme::resolve(&self.session.settings);
        apply_theme!(self.flyout, self.colors);
        apply_theme!(self.menu, self.colors);
        for h in [self.hwnd, self.menu_hwnd].into_iter().flatten() {
            winutil::dwm_style(h, self.colors.dark, true, None);
        }
        if let Some(w) = &self.settings_win {
            apply_theme!(w, self.colors);
            w.set_dark(self.colors.dark);
            if let Some(h) = winutil::hwnd_of(w.window()) {
                winutil::dwm_style(h, self.colors.dark, false, None);
            }
        }
        self.rebuild_flyout();
    }

    // -------------------------------------------------------------- flyout model

    fn display_choice(&self, i: usize) -> Option<u8> {
        let id = &self.session.monitors.get(i)?.id;
        self.session.settings.monitor(id).and_then(|c| c.color)
    }

    fn display_hue(&self, i: usize) -> usize {
        self.display_choice(i).map(usize::from).unwrap_or(i) % theme::HUE_COUNT
    }

    fn monitor_row(&self, i: usize) -> MonitorRow {
        let m = &self.session.monitors[i];
        MonitorRow {
            name: self.session.display_name(i).into(),
            internal: m.is_internal(),
            brightness: m.brightness as i32,
            volume: m.volume as i32,
            show_volume: self.session.shows_volume(i),
            tint: self.colors.display(self.display_choice(i), i),
        }
    }

    /// Full rebuild (structure changed: monitors, settings, theme).
    fn rebuild_flyout(&mut self) {
        let rows: Vec<MonitorRow> = (0..self.session.monitors.len()).map(|i| self.monitor_row(i)).collect();
        self.monitors_model.set_vec(rows);
        self.flyout.set_show_master(self.session.has_master());
        self.flyout.set_master(self.session.master() as i32);
        self.flyout.set_snap(self.session.settings.general.snap_to_ten);
        self.flyout.set_scanning(self.scanning);
        self.resize_flyout();
    }

    /// Cheap update of the live values only (drag, wheel, hardware read-back).
    fn update_values(&mut self) {
        for i in 0..self.monitors_model.row_count() {
            let Some(mut row) = self.monitors_model.row_data(i) else { continue };
            let Some(m) = self.session.monitors.get(i) else { continue };
            if row.brightness != m.brightness as i32 || row.volume != m.volume as i32 {
                row.brightness = m.brightness as i32;
                row.volume = m.volume as i32;
                self.monitors_model.set_row_data(i, row);
            }
        }
        self.flyout.set_master(self.session.master() as i32);
    }

    fn update_tooltip(&self, text: &str) {
        self.shell.set_tooltip(&format!("Brightness Control — {text}"));
    }

    // -------------------------------------------------------------- UI intents

    fn ui_brightness(&mut self, i: usize, v: i32) {
        if i < self.session.monitors.len() {
            self.session.set_brightness(i, v.clamp(0, 100) as u8);
            self.update_values();
            self.update_tooltip(&format!("{} {}%", self.session.display_name(i), v.clamp(0, 100)));
        }
    }

    fn ui_volume(&mut self, i: usize, v: i32) {
        if i < self.session.monitors.len() {
            self.session.set_volume(i, v.clamp(0, 100) as u8);
            self.update_values();
        }
    }

    fn ui_input(&mut self, i: usize, code: u8) {
        if i < self.session.monitors.len() {
            self.session.set_input(i, code);
            let name = vcp::input(code).map(|x| x.name).unwrap_or("input");
            self.update_tooltip(&format!("{} → {name}", self.session.display_name(i)));
        }
    }

    fn ui_master(&mut self, v: i32) {
        let v = v.clamp(0, 100);
        self.session.set_master(v as u8);
        self.update_values();
        self.update_tooltip(&format!("All displays {v}%"));
    }

    fn ui_wheel(&mut self, i: i32, volume: bool, up: bool, ctrl: bool) {
        let d = self.session.wheel_step(up, ctrl);
        if i < 0 {
            let v = self.session.master() as i32 + d;
            self.ui_master(v);
        } else if let Some(m) = self.session.monitors.get(i as usize) {
            let (b, vol) = (m.brightness as i32, m.volume as i32);
            if volume {
                self.ui_volume(i as usize, vol + d);
            } else {
                self.ui_brightness(i as usize, b + d);
            }
        }
    }

    fn handle_outcome(&mut self, o: Outcome) {
        match o {
            Outcome::Nothing => {}
            Outcome::ToggleFlyout => self.toggle_flyout(),
            Outcome::QuickMenu => self.show_menu(),
            Outcome::Osd { title, value, .. } => {
                self.update_values();
                self.update_tooltip(&format!("{title} {value}%"));
            }
            Outcome::Profile(name) => {
                self.update_values();
                self.update_tooltip(&format!("profile “{name}”"));
            }
        }
    }

    // -------------------------------------------------------------- tray menu

    fn show_menu(&mut self) {
        self.hide_flyout();
        if self.menu.window().is_visible() {
            self.hide_menu();
            return;
        }
        // The tray click that brought us here also deactivated the menu.
        if self.menu_last_hide.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.fill_menu();
        // Values may have been changed elsewhere (Fn keys, the monitor's own
        // buttons); fresh read-backs update the open menu.
        self.session.refresh();

        // Repeaters are laid out lazily and a window that grows after its
        // first frame keeps the new area unpainted. So: lay it out once off
        // screen, then show it again at its real size and place.
        // Anchored to the tray icon's screen, like the panel.
        let anchor = crate::shell::tray_rect()
            .map(|r| POINT { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 })
            .unwrap_or_else(winutil::cursor_pos);
        self.menu_opened = Instant::now();
        let opened = self.menu_opened;
        self.menu.window().set_position(PhysicalPosition::new(-32000, -32000));
        let _ = self.menu.show();
        self.ensure_menu_styles();
        Timer::single_shot(Duration::from_millis(20), move || {
            with_app(move |a| {
                // Closed (or reopened) in the meantime: leave it alone.
                if a.menu_opened != opened || !a.menu.window().is_visible() {
                    return;
                }
                let _ = a.menu.hide();
                a.place_menu(anchor);
                let _ = a.menu.show();
                if let Some(h) = a.menu_hwnd {
                    winutil::focus(h);
                }
                // If focus was refused, deactivation never comes: watch for it.
                a.menu_watch.start(TimerMode::Repeated, Duration::from_millis(250), || {
                    with_app(|a| {
                        let front = a.menu_hwnd.is_some_and(winutil::is_foreground)
                            || std::env::var_os("BC_KEEP_MENU").is_some(); // screenshots
                        if !front {
                            a.hide_menu();
                        }
                    })
                });
            })
        });
    }

    /// Fills the menu's tiles from the current settings and values.
    fn fill_menu(&mut self) {
        let s = &self.session.settings;
        self.menu_profiles = s.profiles.iter().map(|p| p.id).collect();
        let tiles: Vec<MenuProfile> = s
            .profiles
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let n = p.values.len().max(1) as u32;
                let level = p.values.iter().map(|v| v.brightness as u32).sum::<u32>() / n;
                MenuProfile {
                    index: i as i32,
                    name: p.name.as_str().into(),
                    level: level as i32,
                    active: self.profile_in_effect(p),
                }
            })
            .collect();
        let rows: Vec<MenuProfileRow> = tiles.chunks(3).map(|c| MenuProfileRow { items: model(c.to_vec()) }).collect();
        let groups: Vec<MenuInputGroup> = (0..self.session.monitors.len())
            .filter(|&i| self.session.shows_input(i))
            .map(|i| {
                let current = self.session.monitors[i].input;
                let items: Vec<MenuInput> = self
                    .session
                    .visible_inputs(i)
                    .into_iter()
                    .filter_map(vcp::input)
                    .map(|inp| MenuInput {
                        code: inp.code as i32,
                        label: inp.short.into(),
                        current: current == Some(inp.code),
                    })
                    .collect();
                MenuInputGroup {
                    monitor: i as i32,
                    title: self.session.display_name(i).into(),
                    tint: self.colors.display(self.display_choice(i), i),
                    rows: model(items.chunks(3).map(|c| MenuInputRow { items: model(c.to_vec()) }).collect()),
                }
            })
            .collect();
        self.menu.set_profiles(model(rows));
        self.menu.set_inputs(model(groups));
    }

    /// True when every connected display in the profile is at its saved
    /// level (and volume, where saved).
    fn profile_in_effect(&self, p: &Profile) -> bool {
        let mut any = false;
        for v in &p.values {
            let Some(i) = self.session.index_of(&v.monitor) else { continue };
            let m = &self.session.monitors[i];
            if m.brightness != v.brightness || v.volume.is_some_and(|vol| self.session.shows_volume(i) && m.volume != vol) {
                return false;
            }
            any = true;
        }
        any
    }

    /// Puts the menu where the panel opens: the tray corner of that screen.
    fn place_menu(&mut self, at: POINT) {
        let area = winutil::monitor_at(at);
        let (w, h) = (self.menu.get_wanted_width(), self.menu.get_wanted_height());
        self.menu.window().set_size(LogicalSize::new(w, h));
        let (pw, ph) = ((w * area.scale).round() as i32, (h * area.scale).round() as i32);
        // Same corner as the panel, whether opened by click or shortcut.
        let (x, y) = winutil::flyout_position(&area, pw, ph);
        self.menu.window().set_position(PhysicalPosition::new(x, y));
        self.menu.window().request_redraw();
    }

    fn hide_menu(&mut self) {
        // Deactivation while it is still being laid out off screen.
        if self.menu_opened.elapsed() < Duration::from_millis(150) {
            return;
        }
        self.close_menu();
    }

    /// Closes the menu now, even while it is still opening.
    fn close_menu(&mut self) {
        self.menu_watch.stop();
        if self.menu.window().is_visible() {
            let _ = self.menu.hide();
            self.menu_last_hide = Instant::now();
        }
    }

    // -------------------------------------------------------------- shell

    fn on_shell(&mut self, ev: ShellEvent) {
        match ev {
            ShellEvent::TrayLeft => self.toggle_flyout(),
            ShellEvent::TrayRight => self.show_menu(),
            ShellEvent::ShowRequested(what) => match what {
                10 | 11 => {
                    let kind = if what == 10 { ActionKind::MasterUp } else { ActionKind::MasterDown };
                    let o = self.session.run_action(&Action::simple(kind));
                    self.handle_outcome(o);
                }
                1 => self.show_menu(),
                2..=6 => {
                    self.open_settings();
                    if let Some(w) = &self.settings_win {
                        w.set_tab((what - 2).min(3) as i32);
                    }
                }
                _ => {
                    if !self.flyout.window().is_visible() {
                        self.toggle_flyout();
                    }
                }
            },
            ShellEvent::TrayWheel { up, ctrl } => {
                if self.session.settings.general.tray_scroll {
                    let o = self.session.tray_wheel(up, ctrl);
                    self.handle_outcome(o);
                }
            }
            ShellEvent::Hotkey(id) => {
                if let Some(action) = self.hotkey_actions.get((id - 1) as usize).cloned() {
                    let o = self.session.run_action(&action);
                    self.handle_outcome(o);
                }
            }
            ShellEvent::DisplayChanged | ShellEvent::Resumed => {
                tracing::info!("{ev:?}: rescanning soon");
                self.schedule_rescan(Duration::from_millis(1500));
            }
            ShellEvent::ThemeChanged => {
                if self.session.settings.general.theme == ThemeMode::System {
                    self.apply_theme();
                }
            }
        }
    }

    fn on_worker(&mut self, ev: Event) {
        if let Event::Failed { monitor, code, write } = &ev {
            tracing::debug!("worker failure {monitor} {code:#x} write={write}");
        }
        if self.session.on_event(ev) {
            self.update_values();
            if self.menu.window().is_visible() {
                self.fill_menu();
            }
        }
    }

    fn register_hotkeys(&mut self) {
        self.hotkey_actions = self.session.settings.hotkeys.iter().map(|h| h.action.clone()).collect();
        let keys: Vec<(i32, u32, u32)> = self
            .session
            .settings
            .hotkeys
            .iter()
            .enumerate()
            .map(|(i, h)| (i as i32 + 1, h.chord.modifiers(), h.chord.key))
            .collect();
        let failed = self.shell.register_hotkeys(keys);
        self.failed_hotkeys = failed
            .iter()
            .filter_map(|id| self.hotkey_actions.get((*id - 1) as usize).cloned())
            .collect();
        if !self.failed_hotkeys.is_empty() {
            tracing::warn!("{} hotkey(s) could not be registered", self.failed_hotkeys.len());
        }
    }

    // -------------------------------------------------------------- scanning

    fn schedule_rescan(&mut self, delay: Duration) {
        self.rescan_timer
            .start(TimerMode::SingleShot, delay, || with_app(|a| a.start_scan()));
    }

    fn start_scan(&mut self) {
        if self.scanning {
            self.rescan_pending = true;
            return;
        }
        self.scanning = true;
        self.flyout.set_scanning(true);
        std::thread::Builder::new()
            .name("discovery".into())
            .spawn(|| {
                let found = discovery::discover();
                let _ = slint::invoke_from_event_loop(move || with_app(move |a| a.finish_scan(found)));
            })
            .ok();
    }

    fn finish_scan(&mut self, found: Vec<Discovered>) {
        if self.session.install(found) {
            save_settings(&self.session.settings);
        }
        self.scanning = false;
        if self.editing.recording.is_none() {
            self.register_hotkeys();
        }
        self.rebuild_flyout();
        if self.flyout.window().is_visible() {
            self.session.refresh();
        }
        self.fill_settings();
        if std::mem::take(&mut self.rescan_pending) {
            self.schedule_rescan(Duration::from_millis(500));
        }
    }

    // -------------------------------------------------------------- popup

    fn size(&self) -> (f32, f32) {
        (self.flyout.get_wanted_width(), self.flyout.get_wanted_height())
    }

    fn toggle_flyout(&mut self) {
        self.close_menu();
        if self.flyout.window().is_visible() {
            self.hide_flyout();
            return;
        }
        // The tray click that brought us here also deactivated the popup.
        if self.last_hide.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.session.refresh();
        self.update_values();
        let anchor = crate::shell::tray_rect()
            .map(|r| POINT { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 })
            .unwrap_or_else(winutil::cursor_pos);
        let area = winutil::monitor_at(anchor);
        let (w, h) = self.size();
        let win = self.flyout.window();
        win.set_size(LogicalSize::new(w, h));
        let (pw, ph) = ((w * area.scale).round() as i32, (h * area.scale).round() as i32);
        let (x, y) = winutil::flyout_position(&area, pw, ph);
        win.set_position(PhysicalPosition::new(x, y));
        let _ = self.flyout.show();
        self.ensure_styles();
        if let Some(h) = self.hwnd {
            winutil::focus(h);
        }
        self.flyout.set_open(true);
        self.flyout.window().request_redraw();
        // Repeaters are laid out lazily; re-measure once the first frame ran.
        Timer::single_shot(Duration::from_millis(16), || with_app(|a| a.resize_flyout()));
    }

    fn hide_flyout(&mut self) {
        self.flyout.set_open(false);
        if self.flyout.window().is_visible() {
            let _ = self.flyout.hide();
            self.last_hide = Instant::now();
        }
    }

    /// Keeps the visible popup sized to its content and in its corner.
    fn resize_flyout(&mut self) {
        if !self.flyout.window().is_visible() {
            return;
        }
        let (w, h) = self.size();
        self.flyout.window().set_size(LogicalSize::new(w, h));
        if let Some(hwnd) = self.hwnd {
            let r = winutil::window_rect(hwnd);
            let area = winutil::monitor_at(POINT { x: r.left + 1, y: r.top + 1 });
            let (pw, ph) = ((w * area.scale).round() as i32, (h * area.scale).round() as i32);
            let (x, y) = winutil::flyout_position(&area, pw, ph);
            if (x, y) != (r.left, r.top) {
                winutil::move_window(hwnd, x, y);
            }
        }
    }

    // -------------------------------------------------------------- settings window

    fn open_settings(&mut self) {
        self.hide_flyout();
        self.close_menu();
        if let Some(w) = &self.settings_win {
            let _ = w.show();
            if let Some(h) = winutil::hwnd_of(w.window()) {
                winutil::focus(h);
            }
            return;
        }
        let w = match SettingsWindow::new() {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("settings window: {e}");
                return;
            }
        };
        apply_theme!(w, self.colors);
        w.set_dark(self.colors.dark);
        w.set_version(VERSION.into());
        Self::wire_settings(&w);
        if let Some([x, y]) = self.session.settings.settings_window {
            let p = POINT { x: x + 40, y: y + 40 };
            let a = winutil::monitor_at(p);
            if p.x < a.work.right && p.y < a.work.bottom && p.x > a.work.left && p.y > a.work.top {
                w.window().set_position(PhysicalPosition::new(x, y));
            }
        }
        self.editing = Editing::default();
        self.settings_win = Some(w);
        self.fill_settings();
        let w = self.settings_win.as_ref().unwrap();
        let _ = w.show();
        if let Some(h) = winutil::hwnd_of(w.window()) {
            winutil::dwm_style(h, self.colors.dark, false, None);
            winutil::focus(h);
        }
    }

    fn wire_settings(w: &SettingsWindow) {
        w.window().on_close_requested(|| {
            with_app(|a| a.close_settings());
            slint::CloseRequestResponse::KeepWindowShown
        });
        w.on_rescan(|| with_app(|a| a.start_scan()));
        w.on_open_logs(|| winutil::open_folder(&paths::log_dir()));
        w.on_theme_selected(|i| {
            with_app(move |a| {
                a.change(|s| {
                    s.general.theme = match i {
                        1 => ThemeMode::Dark,
                        2 => ThemeMode::Light,
                        _ => ThemeMode::System,
                    }
                })
            })
        });
        w.on_general_toggled(|key, on| {
            with_app(move |a| {
                if key == "start" {
                    if let Err(e) = winutil::set_autostart(on) {
                        tracing::error!("autostart: {e:#}");
                    }
                }
                a.change(|s| match key.as_str() {
                    "start" => s.general.start_with_windows = on,
                    "snap" => s.general.snap_to_ten = on,
                    "tray" => s.general.tray_scroll = on,
                    _ => {}
                })
            })
        });
        // Text edits save without refilling the page, so the field keeps focus.
        w.on_alias_edited(|i, text| {
            with_app(move |a| {
                let Some(id) = a.session.monitors.get(i as usize).map(|m| m.id.clone()) else { return };
                let t: String = text.trim().chars().take(50).collect();
                a.session.settings.monitor_mut(&id).alias = (!t.is_empty()).then_some(t);
                save_settings(&a.session.settings);
                a.rebuild_flyout();
            })
        });
        w.on_display_toggled(|i, key, on| {
            with_app(move |a| {
                let Some(id) = a.session.monitors.get(i as usize).map(|m| m.id.clone()) else { return };
                a.change(|s| {
                    let c = s.monitor_mut(&id);
                    if key == "volume" {
                        c.show_volume = on;
                    } else {
                        c.show_input = on;
                    }
                });
            })
        });
        w.on_hue_picked(|i, h| {
            with_app(move |a| {
                let Some(id) = a.session.monitors.get(i as usize).map(|m| m.id.clone()) else { return };
                a.change(|s| s.monitor_mut(&id).color = Some(h as u8));
            })
        });
        w.on_input_toggled(|i, code, on| {
            with_app(move |a| {
                let Some(id) = a.session.monitors.get(i as usize).map(|m| m.id.clone()) else { return };
                a.change(|s| {
                    let c = s.monitor_mut(&id);
                    if c.inputs.is_empty() {
                        c.inputs = vcp::all_input_codes();
                    }
                    c.inputs.retain(|&x| x != code as u8);
                    if on {
                        c.inputs.push(code as u8);
                    }
                });
            })
        });
        w.on_record(|i| with_app(move |a| a.start_recording(i as usize)));
        w.on_cancel_record(|| with_app(|a| a.stop_recording()));
        w.on_key_captured(|text, ctrl, alt, shift, meta| {
            with_app(move |a| a.key_captured(&text, ctrl, alt, shift, meta))
        });
        w.on_clear_shortcut(|i| {
            with_app(move |a| {
                if let Some(action) = a.editing.actions.get(i as usize).cloned() {
                    a.change(|s| s.set_hotkey(action, None));
                }
            })
        });
        w.on_profile_name_edited(|name| {
            with_app(move |a| {
                let taken = profile_named(&a.session.settings, &name).is_some();
                if let Some(w) = &a.settings_win {
                    w.set_name_taken(taken);
                }
            })
        });
        w.on_profile_add(|name, overwrite| {
            with_app(move |a| {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return;
                }
                // An existing profile is only replaced when the user pressed
                // the button that says "Overwrite".
                if profile_named(&a.session.settings, &name).is_some() && !overwrite {
                    if let Some(w) = &a.settings_win {
                        w.set_name_taken(true);
                    }
                    return;
                }
                let values = a.session.capture();
                a.change(|s| {
                    if let Some(p) = s.profiles.iter_mut().find(|p| p.name.eq_ignore_ascii_case(&name)) {
                        p.values = values;
                    } else {
                        let id = s.next_profile_id();
                        s.profiles.push(Profile { id, name, values });
                    }
                });
                if let Some(w) = &a.settings_win {
                    w.set_new_profile_name("".into());
                    w.set_name_taken(false);
                }
            })
        });
        w.on_profile_rename(|i, name| {
            with_app(move |a| {
                let n = name.trim().to_string();
                if let Some(p) = a.session.settings.profiles.get_mut(i as usize) {
                    if !n.is_empty() {
                        p.name = n;
                        save_settings(&a.session.settings);
                    }
                }
            })
        });
        w.on_profile_update(|i| {
            with_app(move |a| {
                let values = a.session.capture();
                a.change(|s| {
                    if let Some(p) = s.profiles.get_mut(i as usize) {
                        p.values = values;
                    }
                });
            })
        });
        w.on_profile_delete(|i| {
            with_app(move |a| {
                a.change(|s| {
                    if let Some(id) = s.profiles.get(i as usize).map(|p| p.id) {
                        s.remove_profile(id);
                    }
                });
            })
        });
    }

    /// Applies a settings change right away: saves it, re-registers hotkeys
    /// if they changed, refreshes theme, popup and the settings page.
    fn change(&mut self, f: impl FnOnce(&mut Settings)) {
        let before = self.session.settings.clone();
        f(&mut self.session.settings);
        let s = self.session.settings.clone();
        save_settings(&s);
        if before.general.tray_scroll != s.general.tray_scroll {
            self.shell.set_tray_scroll(s.general.tray_scroll);
        }
        if before.hotkeys != s.hotkeys && self.editing.recording.is_none() {
            self.register_hotkeys();
        }
        if before.general.theme != s.general.theme {
            self.apply_theme();
        } else {
            self.rebuild_flyout();
        }
        self.fill_settings();
    }

    fn fill_settings(&mut self) {
        let Some(w) = &self.settings_win else { return };
        let s = &self.session.settings;
        w.set_theme_index(match s.general.theme {
            ThemeMode::System => 0,
            ThemeMode::Dark => 1,
            ThemeMode::Light => 2,
        });
        w.set_start_with_windows(winutil::autostart_enabled());
        w.set_snap(s.general.snap_to_ten);
        w.set_tray_scroll(s.general.tray_scroll);
        w.set_recording(self.editing.recording.is_some());

        // Displays
        let displays: Vec<DisplayCfg> = self
            .session
            .monitors
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let cfg = s.monitor(&m.id).cloned().unwrap_or_default();
                let toggles: Vec<InputToggle> = vcp::INPUTS
                    .iter()
                    .map(|inp| InputToggle {
                        code: inp.code as i32,
                        label: inp.short.into(),
                        on: cfg.inputs.is_empty() || cfg.inputs.contains(&inp.code),
                    })
                    .collect();
                let subtitle = if m.is_internal() {
                    "Built-in · Windows brightness"
                } else if m.supports_volume {
                    "External · DDC/CI · speakers"
                } else {
                    "External · DDC/CI"
                };
                DisplayCfg {
                    hw_name: m.hw_name.as_str().into(),
                    title: m.hw_name.as_str().into(),
                    hue: self.display_hue(i) as i32,
                    subtitle: subtitle.into(),
                    alias: cfg.alias.clone().unwrap_or_default().into(),
                    internal: m.is_internal(),
                    show_volume: cfg.show_volume,
                    show_input: cfg.show_input,
                    input_rows: model(
                        toggles.chunks(4).map(|c| InputToggleRow { items: model(c.to_vec()) }).collect(),
                    ),
                }
            })
            .collect();
        w.set_displays(model(displays));

        // Shortcuts
        let list = self.shortcut_list();
        let conflicts = s.conflicts();
        let rows: Vec<ShortcutRow> = list
            .iter()
            .enumerate()
            .map(|(i, (header, label, action))| {
                let chord = s.hotkey_for(action);
                let mut warning = String::new();
                if let Some(c) = chord {
                    if conflicts.contains(&c) {
                        warning = "Also used by another shortcut".into();
                    } else if self.failed_hotkeys.contains(action) {
                        warning = "Taken by another app".into();
                    }
                }
                ShortcutRow {
                    header: header.as_str().into(),
                    label: label.as_str().into(),
                    chord: chord.map(|c| c.label()).unwrap_or_default().into(),
                    recording: self.editing.recording == Some(i),
                    warning: warning.into(),
                }
            })
            .collect();
        w.set_shortcuts(model(rows));

        // Profiles
        let profiles: Vec<ProfileCfg> = s
            .profiles
            .iter()
            .map(|p| {
                let summary: Vec<String> = p
                    .values
                    .iter()
                    .map(|v| {
                        let name = self
                            .session
                            .index_of(&v.monitor)
                            .map(|i| self.session.display_name(i))
                            .unwrap_or_else(|| "not connected".into());
                        match v.volume {
                            Some(vol) => format!("{name} {}% · vol {vol}%", v.brightness),
                            None => format!("{name} {}%", v.brightness),
                        }
                    })
                    .collect();
                ProfileCfg { name: p.name.as_str().into(), summary: summary.join("   ").into() }
            })
            .collect();
        w.set_profiles(model(profiles));
        self.editing.actions = list.into_iter().map(|(_, _, a)| a).collect();
    }

    fn shortcut_list(&self) -> Vec<(String, String, Action)> {
        let s = &self.session.settings;
        let mut out = vec![
            ("Panel".to_string(), "Show or hide the panel".to_string(), Action::simple(ActionKind::ToggleFlyout)),
            (String::new(), "Open the tray menu".into(), Action::simple(ActionKind::QuickMenu)),
            ("All displays".into(), "Brighter".into(), Action::simple(ActionKind::MasterUp)),
            (String::new(), "Dimmer".into(), Action::simple(ActionKind::MasterDown)),
        ];
        for (i, m) in self.session.monitors.iter().enumerate() {
            out.push((self.session.display_name(i), "Brighter".into(), Action::monitor(ActionKind::BrightnessUp, &m.id)));
            out.push((String::new(), "Dimmer".into(), Action::monitor(ActionKind::BrightnessDown, &m.id)));
            if self.session.shows_volume(i) {
                out.push((String::new(), "Louder".into(), Action::monitor(ActionKind::VolumeUp, &m.id)));
                out.push((String::new(), "Quieter".into(), Action::monitor(ActionKind::VolumeDown, &m.id)));
            }
        }
        for (n, p) in s.profiles.iter().enumerate() {
            let header = if n == 0 { "Profiles".to_string() } else { String::new() };
            out.push((header, p.name.clone(), Action::profile(p.id)));
        }
        out
    }

    fn set_status(&self, text: &str) {
        if let Some(w) = &self.settings_win {
            w.set_status(text.into());
        }
    }

    fn start_recording(&mut self, i: usize) {
        self.editing.recording = Some(i);
        self.set_status("");
        // Live hotkeys would swallow the combination we want to record.
        self.shell.register_hotkeys(Vec::new());
        self.fill_settings();
    }

    fn stop_recording(&mut self) {
        if self.editing.recording.take().is_some() {
            self.register_hotkeys();
        }
        self.fill_settings();
    }

    fn key_captured(&mut self, text: &str, ctrl: bool, alt: bool, shift: bool, win: bool) {
        let Some(i) = self.editing.recording else { return };
        let Some(action) = self.editing.actions.get(i).cloned() else { return };
        match keys::capture(text, ctrl || alt || shift || win) {
            keys::Captured::Modifier => return,
            keys::Captured::Unknown => {
                self.set_status("That key cannot be used for a shortcut");
                return;
            }
            keys::Captured::Clear => self.change(|s| s.set_hotkey(action, None)),
            keys::Captured::Vk(vk) => {
                let chord = Chord { ctrl, alt, shift, win, key: vk };
                self.change(|s| s.set_hotkey(action, Some(chord)));
            }
        }
        self.stop_recording();
    }

    fn close_settings(&mut self) {
        if self.editing.recording.is_some() {
            self.stop_recording();
        }
        if let Some(w) = self.settings_win.take() {
            let p = w.window().position();
            if self.session.settings.settings_window != Some([p.x, p.y]) {
                self.session.settings.settings_window = Some([p.x, p.y]);
                save_settings(&self.session.settings);
            }
            let _ = w.hide();
            // Drop the component after this callback has returned.
            Timer::single_shot(Duration::from_millis(50), move || drop(w));
        }
        self.session.refresh();
    }
}
