//! Maps a Slint key event to a Win32 virtual-key code (layout aware).

use slint::platform::Key;
use slint::SharedString;
use windows::Win32::UI::Input::KeyboardAndMouse::VkKeyScanW;

fn k(key: Key) -> SharedString {
    key.into()
}

pub enum Captured {
    /// Only a modifier was pressed; keep listening.
    Modifier,
    Clear,
    Vk(u32),
    Unknown,
}

pub fn capture(text: &str, any_modifier: bool) -> Captured {
    for m in [Key::Control, Key::ControlR, Key::Shift, Key::ShiftR, Key::Alt, Key::AltGr, Key::Meta, Key::MetaR] {
        if text == k(m).as_str() {
            return Captured::Modifier;
        }
    }
    if !any_modifier && (text == k(Key::Backspace).as_str() || text == k(Key::Delete).as_str()) {
        return Captured::Clear;
    }
    let table: &[(Key, u32)] = &[
        (Key::F1, 0x70),
        (Key::F2, 0x71),
        (Key::F3, 0x72),
        (Key::F4, 0x73),
        (Key::F5, 0x74),
        (Key::F6, 0x75),
        (Key::F7, 0x76),
        (Key::F8, 0x77),
        (Key::F9, 0x78),
        (Key::F10, 0x79),
        (Key::F11, 0x7A),
        (Key::F12, 0x7B),
        (Key::F13, 0x7C),
        (Key::F14, 0x7D),
        (Key::F15, 0x7E),
        (Key::F16, 0x7F),
        (Key::F17, 0x80),
        (Key::F18, 0x81),
        (Key::F19, 0x82),
        (Key::F20, 0x83),
        (Key::F21, 0x84),
        (Key::F22, 0x85),
        (Key::F23, 0x86),
        (Key::F24, 0x87),
        (Key::LeftArrow, 0x25),
        (Key::UpArrow, 0x26),
        (Key::RightArrow, 0x27),
        (Key::DownArrow, 0x28),
        (Key::PageUp, 0x21),
        (Key::PageDown, 0x22),
        (Key::End, 0x23),
        (Key::Home, 0x24),
        (Key::Insert, 0x2D),
        (Key::Delete, 0x2E),
        (Key::Backspace, 0x08),
        (Key::Tab, 0x09),
        (Key::Return, 0x0D),
        (Key::Space, 0x20),
        (Key::Pause, 0x13),
        (Key::ScrollLock, 0x91),
        (Key::SysReq, 0x2C),
    ];
    for (key, vk) in table {
        if text == k(*key).as_str() {
            return Captured::Vk(*vk);
        }
    }
    let mut chars = text.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphanumeric() {
            return Captured::Vk(c.to_ascii_uppercase() as u32);
        }
        // Punctuation / national letters: ask the active keyboard layout.
        let mut buf = [0u16; 2];
        if c.encode_utf16(&mut buf).len() == 1 {
            let r = unsafe { VkKeyScanW(buf[0]) };
            if r != -1 {
                let vk = (r as u16 & 0xFF) as u32;
                if vk != 0xFF {
                    return Captured::Vk(vk);
                }
            }
        }
        // Ctrl+letter may arrive as a control character.
        if (c as u32) >= 1 && (c as u32) <= 26 {
            return Captured::Vk(0x40 + c as u32);
        }
    }
    Captured::Unknown
}
