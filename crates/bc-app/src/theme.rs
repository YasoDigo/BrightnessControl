//! Dark / light palettes and the automatic per-display colours.

use crate::winutil;
use bc_core::settings::{Settings, ThemeMode};
use slint::Color;

#[derive(Debug, Clone, Copy)]
pub struct Colors {
    pub dark: bool,
    pub bg: Color,
    pub surface: Color,
    pub surface_hover: Color,
    pub surface_strong: Color,
    pub border: Color,
    pub text: Color,
    pub subtext: Color,
    pub faint: Color,
    pub track: Color,
    pub rail: Color,
    pub knob: Color,
    pub accent: Color,
    pub on_accent: Color,
    pub danger: Color,
    /// Display colours: amber, sky, mint, violet, rose, coral.
    pub hues: [Color; 6],
    /// Text and icons drawn on top of a hue.
    pub on_hue: Color,
    /// Raised buttons in the tray menu.
    pub tile: Color,
    pub tile_hover: Color,
}

pub const HUE_COUNT: usize = 6;

impl Colors {
    /// Colour of display `idx` (its own choice, or the next one in line).
    pub fn display(&self, choice: Option<u8>, idx: usize) -> Color {
        self.hues[choice.map(usize::from).unwrap_or(idx) % HUE_COUNT]
    }
}

const fn rgb(v: u32) -> Color {
    Color::from_argb_u8(0xFF, (v >> 16) as u8, (v >> 8) as u8, v as u8)
}

const fn argb(v: u32) -> Color {
    Color::from_argb_u8((v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8)
}

fn dark() -> Colors {
    Colors {
        dark: true,
        bg: rgb(0x19191C),
        surface: argb(0x0AFFFFFF),
        surface_hover: argb(0x12FFFFFF),
        surface_strong: argb(0x1CFFFFFF),
        border: argb(0x10FFFFFF),
        text: rgb(0xEDEDEF),
        subtext: rgb(0x9C9CA3),
        faint: rgb(0x68686F),
        track: argb(0x12FFFFFF),
        rail: rgb(0xBDBDC4),
        knob: rgb(0x2A2A2F),
        accent: rgb(0xF2B85B),
        on_accent: rgb(0x2B200F),
        danger: rgb(0xFF8A8A),
        hues: [rgb(0xF2B85B), rgb(0x6CB6FF), rgb(0x5FD3A6), rgb(0xB39CFF), rgb(0xFF8FB1), rgb(0xFF9A6B)],
        on_hue: rgb(0x1C1B20),
        tile: rgb(0x2A2A30),
        tile_hover: rgb(0x35353C),
    }
}

fn light() -> Colors {
    Colors {
        dark: false,
        bg: rgb(0xF6F5F2),
        surface: rgb(0xFFFFFF),
        surface_hover: argb(0x0A000000),
        surface_strong: argb(0x12000000),
        border: argb(0x12000000),
        text: rgb(0x1E1D1B),
        subtext: rgb(0x6B6963),
        faint: rgb(0xA19E97),
        track: argb(0x10000000),
        rail: rgb(0x8E8C86),
        knob: rgb(0xFFFFFF),
        accent: rgb(0xE29A2C),
        on_accent: rgb(0xFFFFFF),
        danger: rgb(0xC8423B),
        hues: [rgb(0xE29A2C), rgb(0x2F86D9), rgb(0x1E9E73), rgb(0x7D5FE0), rgb(0xD9487A), rgb(0xE2683A)],
        on_hue: rgb(0xFFFFFF),
        tile: rgb(0xE9E7E2),
        tile_hover: rgb(0xDEDCD6),
    }
}

pub fn resolve(s: &Settings) -> Colors {
    match s.general.theme {
        ThemeMode::Dark => dark(),
        ThemeMode::Light => light(),
        ThemeMode::System => {
            if winutil::system_uses_light_theme() {
                light()
            } else {
                dark()
            }
        }
    }
}

/// Every Slint window has its own instance of the `Theme` global.
#[macro_export]
macro_rules! apply_theme {
    ($ui:expr, $c:expr) => {{
        let t = $ui.global::<$crate::Theme>();
        let c = $c;
        t.set_dark(c.dark);
        t.set_bg(c.bg);
        t.set_surface(c.surface);
        t.set_surface_hover(c.surface_hover);
        t.set_surface_strong(c.surface_strong);
        t.set_border(c.border);
        t.set_text(c.text);
        t.set_subtext(c.subtext);
        t.set_faint(c.faint);
        t.set_track(c.track);
        t.set_rail(c.rail);
        t.set_knob(c.knob);
        t.set_accent(c.accent);
        t.set_on_accent(c.on_accent);
        t.set_danger(c.danger);
        t.set_amber(c.hues[0]);
        t.set_sky(c.hues[1]);
        t.set_mint(c.hues[2]);
        t.set_violet(c.hues[3]);
        t.set_rose(c.hues[4]);
        t.set_coral(c.hues[5]);
        t.set_on_hue(c.on_hue);
        t.set_tile(c.tile);
        t.set_tile_hover(c.tile_hover);
    }};
}
