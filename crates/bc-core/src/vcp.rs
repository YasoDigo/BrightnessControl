//! MCCS / DDC-CI VCP codes and input source table.

pub const BRIGHTNESS: u8 = 0x10;
pub const CONTRAST: u8 = 0x12;
pub const INPUT_SOURCE: u8 = 0x60;
pub const VOLUME: u8 = 0x62;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputSource {
    pub code: u8,
    pub name: &'static str,
    pub short: &'static str,
}

/// Same set (and codes) the original app offered.
pub const INPUTS: &[InputSource] = &[
    InputSource { code: 15, name: "DisplayPort 1", short: "DP 1" },
    InputSource { code: 16, name: "DisplayPort 2", short: "DP 2" },
    InputSource { code: 17, name: "HDMI 1", short: "HDMI 1" },
    InputSource { code: 18, name: "HDMI 2", short: "HDMI 2" },
    InputSource { code: 27, name: "USB-C", short: "USB-C" },
    InputSource { code: 3, name: "DVI 1", short: "DVI 1" },
    InputSource { code: 4, name: "DVI 2", short: "DVI 2" },
    InputSource { code: 1, name: "VGA", short: "VGA" },
];

pub fn all_input_codes() -> Vec<u8> {
    INPUTS.iter().map(|i| i.code).collect()
}

pub fn input(code: u8) -> Option<&'static InputSource> {
    INPUTS.iter().find(|i| i.code == code)
}

/// Monitors report the input in the low byte; some set high bits.
pub fn normalize_input(raw: u32) -> u8 {
    (raw & 0xFF) as u8
}
