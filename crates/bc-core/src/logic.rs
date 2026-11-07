//! Small pure rules carried over from the original app (and unit tested).

use std::collections::HashMap;
use std::time::{Duration, Instant};

pub fn clamp_pct(v: i32) -> u8 {
    v.clamp(0, 100) as u8
}

/// Magnetic snap: within ±2 of a multiple of 10 the value jumps to it.
/// `precise` (Ctrl held) disables snapping.
pub fn snap(v: i32, precise: bool) -> u8 {
    let mut v = v.clamp(0, 100);
    if !precise {
        let r = v % 10;
        if r <= 2 {
            v -= r;
        } else if r >= 8 {
            v += 10 - r;
        }
    }
    clamp_pct(v)
}

pub fn pct_to_raw(pct: u8, max: u32) -> u32 {
    if max == 0 {
        return pct as u32;
    }
    ((pct as u64 * max as u64 + 50) / 100) as u32
}

pub fn raw_to_pct(raw: u32, max: u32) -> u8 {
    if max == 0 {
        return clamp_pct(raw as i32);
    }
    clamp_pct(((raw as u64 * 100 + max as u64 / 2) / max as u64) as i32)
}

/// Mouse-wheel acceleration: fast scrolling moves in bigger steps.
#[derive(Debug, Default)]
pub struct WheelAccel {
    last: Option<Instant>,
}

impl WheelAccel {
    pub fn step(&mut self, now: Instant, ctrl: bool) -> i32 {
        let dt = self.last.map(|l| now.saturating_duration_since(l));
        self.last = Some(now);
        if ctrl {
            return 5;
        }
        match dt {
            Some(d) if d < Duration::from_millis(50) => 3,
            Some(d) if d < Duration::from_millis(150) => 2,
            _ => 1,
        }
    }
}

/// Hotkey auto-repeat acceleration, tracked per hotkey (the C# version shared
/// one counter between all hotkeys).
#[derive(Debug, Default)]
pub struct RepeatAccel {
    state: HashMap<String, (Instant, u32)>,
}

impl RepeatAccel {
    pub fn step(&mut self, key: &str, now: Instant) -> i32 {
        let entry = self.state.entry(key.to_string()).or_insert((now - Duration::from_secs(10), 0));
        if now.saturating_duration_since(entry.0) < Duration::from_millis(250) {
            entry.1 += 1;
        } else {
            entry.1 = 0;
        }
        entry.0 = now;
        match entry.1 {
            c if c > 15 => 3,
            c if c > 4 => 2,
            _ => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_matches_original() {
        assert_eq!(snap(41, false), 40);
        assert_eq!(snap(42, false), 40);
        assert_eq!(snap(43, false), 43);
        assert_eq!(snap(47, false), 47);
        assert_eq!(snap(48, false), 50);
        assert_eq!(snap(99, false), 100);
        assert_eq!(snap(42, true), 42);
        assert_eq!(snap(-5, false), 0);
        assert_eq!(snap(150, true), 100);
    }

    #[test]
    fn raw_conversion_respects_max() {
        assert_eq!(pct_to_raw(50, 100), 50);
        assert_eq!(pct_to_raw(50, 255), 128);
        assert_eq!(raw_to_pct(128, 255), 50);
        assert_eq!(raw_to_pct(255, 255), 100);
        assert_eq!(raw_to_pct(30, 0), 30);
    }

    #[test]
    fn wheel_accel() {
        let mut w = WheelAccel::default();
        let t = Instant::now();
        assert_eq!(w.step(t, false), 1);
        assert_eq!(w.step(t + Duration::from_millis(20), false), 3);
        assert_eq!(w.step(t + Duration::from_millis(120), false), 2);
        assert_eq!(w.step(t + Duration::from_millis(900), false), 1);
        assert_eq!(w.step(t + Duration::from_millis(901), true), 5);
    }

    #[test]
    fn repeat_accel_is_per_key() {
        let mut r = RepeatAccel::default();
        let t = Instant::now();
        for i in 0..6 {
            r.step("a", t + Duration::from_millis(i * 30));
        }
        assert_eq!(r.step("a", t + Duration::from_millis(200)), 2);
        assert_eq!(r.step("b", t + Duration::from_millis(210)), 1);
        let mut last = 0;
        for i in 0..20 {
            last = r.step("a", t + Duration::from_millis(230 + i * 30));
        }
        assert_eq!(last, 3);
    }
}
