//! Pure key and button tables for the Linux backends (no OS calls, so they are unit-tested on
//! every platform).
//!
//! Wire keys are Windows set-1 scancodes plus an `E0` flag ([`ScanKey`]). Linux numbers keys
//! with evdev codes (`linux/input-event-codes.h`), and the X server numbers them `evdev + 8`
//! (the `evdev` XKB rules every modern server uses). For the main block the evdev code is
//! the set-1 make code; the `E0` cluster (arrows, navigation, right Ctrl/Alt, Win keys,
//! keypad Enter and divide, PrintScreen) was renumbered, so it needs the explicit table
//! below.

// The X11 sink and the tests are the only users on non-Linux hosts.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use crate::keys::ScanKey;
use crate::MouseButton;

/// X keycodes are evdev codes shifted by this offset.
pub(crate) const X_KEYCODE_OFFSET: u8 = 8;

/// `E0`-prefixed set-1 make code to evdev code.
const EXTENDED: &[(u16, u16)] = &[
    (0x1C, 96),  // KEY_KPENTER
    (0x1D, 97),  // KEY_RIGHTCTRL
    (0x35, 98),  // KEY_KPSLASH
    (0x37, 99),  // KEY_SYSRQ (PrintScreen)
    (0x38, 100), // KEY_RIGHTALT (AltGr)
    (0x47, 102), // KEY_HOME
    (0x48, 103), // KEY_UP
    (0x49, 104), // KEY_PAGEUP
    (0x4B, 105), // KEY_LEFT
    (0x4D, 106), // KEY_RIGHT
    (0x4F, 107), // KEY_END
    (0x50, 108), // KEY_DOWN
    (0x51, 109), // KEY_PAGEDOWN
    (0x52, 110), // KEY_INSERT
    (0x53, 111), // KEY_DELETE
    (0x5B, 125), // KEY_LEFTMETA
    (0x5C, 126), // KEY_RIGHTMETA
    (0x5D, 127), // KEY_COMPOSE (Menu)
];

/// Highest set-1 make code that maps one-to-one onto the evdev code (F12 = 0x58 = 88).
const IDENTITY_MAX: u16 = 0x58;

/// The evdev key code for a wire key, or `None` for keys with no Linux equivalent.
pub(crate) fn scan_to_evdev(key: ScanKey) -> Option<u16> {
    if key.extended {
        return EXTENDED
            .iter()
            .find(|(sc, _)| *sc == key.scancode)
            .map(|&(_, ev)| ev);
    }
    // 0x54..=0x55 are unused on set 1; 0x56 is the ISO extra key (evdev 86); 0x57/0x58 are
    // F11/F12 and equal evdev 87/88.
    (1..=IDENTITY_MAX)
        .contains(&key.scancode)
        .then_some(key.scancode)
        .filter(|sc| !(0x54..=0x55).contains(sc))
}

/// Inverse of [`scan_to_evdev`], used by the viewer keyboard grab.
pub(crate) fn evdev_to_scan(code: u16) -> Option<ScanKey> {
    if let Some(&(sc, _)) = EXTENDED.iter().find(|(_, ev)| *ev == code) {
        return Some(ScanKey::new(sc, true));
    }
    let key = ScanKey::new(code, false);
    (scan_to_evdev(key) == Some(code)).then_some(key)
}

/// The X keycode for a wire key.
pub(crate) fn scan_to_x_keycode(key: ScanKey) -> Option<u8> {
    let ev = scan_to_evdev(key)?;
    u8::try_from(ev.checked_add(u16::from(X_KEYCODE_OFFSET))?).ok()
}

/// The wire key for an X keycode.
pub(crate) fn x_keycode_to_scan(keycode: u8) -> Option<ScanKey> {
    evdev_to_scan(u16::from(keycode.checked_sub(X_KEYCODE_OFFSET)?))
}

/// X core protocol pointer button number (`ButtonPress` detail).
pub(crate) fn x_button(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Middle => 2,
        MouseButton::Right => 3,
        MouseButton::Back => 8,
        MouseButton::Forward => 9,
    }
}

/// One wheel notch is 120 units on the wire.
const NOTCH: i32 = 120;

/// Converts a wheel delta into X scroll-button clicks, carrying the sub-notch remainder in
/// `carry` so smooth-scroll devices (deltas below 120) still scroll eventually.
///
/// Returns `(button, count)`; buttons 4/5 are up/down and 6/7 left/right. `count` is 0 while
/// the carry has not reached a full notch.
pub(crate) fn wheel_clicks(horizontal: bool, delta: i32, carry: &mut i32) -> (u8, u32) {
    let total = carry.saturating_add(delta);
    let notches = total / NOTCH;
    *carry = total % NOTCH;
    let button = match (horizontal, notches >= 0) {
        // Positive vertical delta = wheel forward = scroll up; positive horizontal = right.
        (false, true) => 4,
        (false, false) => 5,
        (true, true) => 7,
        (true, false) => 6,
    };
    (button, notches.unsigned_abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_block_is_identity() {
        // KEY_A (evdev 30) is set-1 0x1E; X keycode 38.
        assert_eq!(scan_to_evdev(ScanKey::new(0x1E, false)), Some(30));
        assert_eq!(scan_to_x_keycode(ScanKey::new(0x1E, false)), Some(38));
        assert_eq!(scan_to_x_keycode(ScanKey::ESCAPE), Some(9));
        assert_eq!(scan_to_x_keycode(ScanKey::SPACE), Some(65));
        assert_eq!(scan_to_evdev(ScanKey::new(0x58, false)), Some(88)); // F12
    }

    #[test]
    fn extended_cluster_is_renumbered() {
        assert_eq!(scan_to_evdev(ScanKey::RIGHT_CTRL), Some(97));
        assert_eq!(scan_to_evdev(ScanKey::RIGHT_ALT), Some(100));
        assert_eq!(scan_to_evdev(ScanKey::LEFT_WIN), Some(125));
        assert_eq!(scan_to_evdev(ScanKey::UP), Some(103));
        assert_eq!(scan_to_evdev(ScanKey::DELETE), Some(111));
        assert_eq!(scan_to_x_keycode(ScanKey::LEFT), Some(113));
        // Keypad 7 shares set-1 0x47 with Home but is not extended.
        assert_eq!(scan_to_evdev(ScanKey::new(0x47, false)), Some(71));
    }

    #[test]
    fn unmapped_keys_are_none() {
        assert_eq!(scan_to_evdev(ScanKey::new(0x00, false)), None);
        assert_eq!(scan_to_evdev(ScanKey::new(0x59, false)), None);
        assert_eq!(scan_to_evdev(ScanKey::new(0x54, false)), None);
        assert_eq!(scan_to_evdev(ScanKey::new(0x7F, true)), None);
        assert_eq!(x_keycode_to_scan(3), None);
        assert_eq!(x_keycode_to_scan(255), None);
    }

    #[test]
    fn tables_round_trip() {
        for sc in 0..=0x7Fu16 {
            for extended in [false, true] {
                let key = ScanKey::new(sc, extended);
                if let Some(code) = scan_to_x_keycode(key) {
                    assert_eq!(x_keycode_to_scan(code), Some(key), "{key:?}");
                }
            }
        }
    }

    #[test]
    fn buttons_follow_x_numbering() {
        assert_eq!(x_button(MouseButton::Left), 1);
        assert_eq!(x_button(MouseButton::Middle), 2);
        assert_eq!(x_button(MouseButton::Right), 3);
        assert_eq!(x_button(MouseButton::Back), 8);
        assert_eq!(x_button(MouseButton::Forward), 9);
    }

    #[test]
    fn wheel_notches_and_carry() {
        let mut carry = 0;
        assert_eq!(wheel_clicks(false, 120, &mut carry), (4, 1));
        assert_eq!(wheel_clicks(false, -240, &mut carry), (5, 2));
        assert_eq!(wheel_clicks(true, 120, &mut carry), (7, 1));
        assert_eq!(wheel_clicks(true, -120, &mut carry), (6, 1));
        // Smooth scrolling: 40 + 40 + 40 makes one notch.
        let mut carry = 0;
        assert_eq!(wheel_clicks(false, 40, &mut carry).1, 0);
        assert_eq!(wheel_clicks(false, 40, &mut carry).1, 0);
        assert_eq!(wheel_clicks(false, 40, &mut carry), (4, 1));
        assert_eq!(carry, 0);
        // Extreme values do not overflow.
        let mut carry = i32::MAX;
        let _ = wheel_clicks(false, i32::MAX, &mut carry);
    }
}
