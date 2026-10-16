//! Wire scancode (Windows set-1, as sent by the viewer) to macOS virtual key code.
//!
//! The macOS codes are the `kVK_*` constants of `Carbon.HIToolbox/Events.h`
//! (`CGKeyCode`). They name *physical* keys on an ANSI/ISO keyboard, so, exactly like the
//! Windows injector, the host's own layout decides which character a key produces.
//!
//! Pure table, compiled on every platform so it is unit-tested on the Windows dev machines.

use crate::keys::ScanKey;

/// `CGEventFlags` mask bits for the modifier families. Plain numbers (not the `objc2` type)
/// so this file builds without the Apple crates.
pub mod flags {
    /// `kCGEventFlagMaskShift`.
    pub const SHIFT: u64 = 0x0002_0000;
    /// `kCGEventFlagMaskControl`.
    pub const CONTROL: u64 = 0x0004_0000;
    /// `kCGEventFlagMaskAlternate` (Option).
    pub const ALTERNATE: u64 = 0x0008_0000;
    /// `kCGEventFlagMaskCommand`.
    pub const COMMAND: u64 = 0x0010_0000;
    /// `kCGEventFlagMaskAlphaShift` (Caps Lock).
    pub const CAPS_LOCK: u64 = 0x0001_0000;
}

/// macOS virtual key code for a physical key, or `None` when macOS has no such key
/// (Pause, Scroll Lock, Print Screen, unknown scancodes).
///
/// The Windows key maps to Command, Alt to Option, Menu to the context-menu key.
pub fn mac_keycode(key: ScanKey) -> Option<u16> {
    if key.extended {
        return extended(key.scancode);
    }
    plain(key.scancode)
}

fn plain(sc: u16) -> Option<u16> {
    Some(match sc {
        0x01 => 0x35, // Escape
        0x02 => 0x12, // 1
        0x03 => 0x13,
        0x04 => 0x14,
        0x05 => 0x15,
        0x06 => 0x17, // 5
        0x07 => 0x16, // 6
        0x08 => 0x1A,
        0x09 => 0x1C,
        0x0A => 0x19, // 9
        0x0B => 0x1D, // 0
        0x0C => 0x1B, // minus
        0x0D => 0x18, // equals
        0x0E => 0x33, // Backspace (kVK_Delete)
        0x0F => 0x30, // Tab
        0x10 => 0x0C, // Q
        0x11 => 0x0D, // W
        0x12 => 0x0E, // E
        0x13 => 0x0F, // R
        0x14 => 0x11, // T
        0x15 => 0x10, // Y
        0x16 => 0x20, // U
        0x17 => 0x22, // I
        0x18 => 0x1F, // O
        0x19 => 0x23, // P
        0x1A => 0x21, // left bracket
        0x1B => 0x1E, // right bracket
        0x1C => 0x24, // Return
        0x1D => 0x3B, // Left Control
        0x1E => 0x00, // A
        0x1F => 0x01, // S
        0x20 => 0x02, // D
        0x21 => 0x03, // F
        0x22 => 0x05, // G
        0x23 => 0x04, // H
        0x24 => 0x26, // J
        0x25 => 0x28, // K
        0x26 => 0x25, // L
        0x27 => 0x29, // semicolon
        0x28 => 0x27, // quote
        0x29 => 0x32, // grave
        0x2A => 0x38, // Left Shift
        0x2B => 0x2A, // backslash
        0x2C => 0x06, // Z
        0x2D => 0x07, // X
        0x2E => 0x08, // C
        0x2F => 0x09, // V
        0x30 => 0x0B, // B
        0x31 => 0x2D, // N
        0x32 => 0x2E, // M
        0x33 => 0x2B, // comma
        0x34 => 0x2F, // period
        0x35 => 0x2C, // slash
        0x36 => 0x3C, // Right Shift
        0x37 => 0x43, // keypad *
        0x38 => 0x3A, // Left Option
        0x39 => 0x31, // Space
        0x3A => 0x39, // Caps Lock
        0x3B => 0x7A, // F1
        0x3C => 0x78,
        0x3D => 0x63,
        0x3E => 0x76,
        0x3F => 0x60,
        0x40 => 0x61,
        0x41 => 0x62,
        0x42 => 0x64,
        0x43 => 0x65,
        0x44 => 0x6D, // F10
        0x45 => 0x47, // Num Lock -> keypad Clear
        0x47 => 0x59, // keypad 7
        0x48 => 0x5B,
        0x49 => 0x5C,
        0x4A => 0x4E, // keypad minus
        0x4B => 0x56,
        0x4C => 0x57,
        0x4D => 0x58,
        0x4E => 0x45, // keypad plus
        0x4F => 0x53,
        0x50 => 0x54,
        0x51 => 0x55,
        0x52 => 0x52, // keypad 0
        0x53 => 0x41, // keypad period
        0x56 => 0x0A, // ISO extra key (kVK_ISO_Section)
        0x57 => 0x67, // F11
        0x58 => 0x6F, // F12
        0x64 => 0x69, // F13
        0x65 => 0x6B, // F14
        0x66 => 0x71, // F15
        _ => return None,
    })
}

fn extended(sc: u16) -> Option<u16> {
    Some(match sc {
        0x1C => 0x4C, // keypad Enter
        0x1D => 0x3E, // Right Control
        0x35 => 0x4B, // keypad slash
        0x38 => 0x3D, // Right Option
        0x47 => 0x73, // Home
        0x48 => 0x7E, // Up
        0x49 => 0x74, // Page Up
        0x4B => 0x7B, // Left
        0x4D => 0x7C, // Right
        0x4F => 0x77, // End
        0x50 => 0x7D, // Down
        0x51 => 0x79, // Page Down
        0x52 => 0x72, // Insert -> Help
        0x53 => 0x75, // Delete (forward)
        0x5B => 0x37, // Left Windows -> Left Command
        0x5C => 0x36, // Right Windows -> Right Command
        0x5D => 0x6E, // Menu (context menu)
        _ => return None,
    })
}

/// The `CGEventFlags` modifier bit a modifier key controls, or `None` for ordinary keys.
///
/// macOS applications read modifier state from the event flags, not from the key itself, so the
/// injector keeps these in sync on every key event.
pub fn modifier_flag(keycode: u16) -> Option<u64> {
    match keycode {
        0x38 | 0x3C => Some(flags::SHIFT),
        0x3B | 0x3E => Some(flags::CONTROL),
        0x3A | 0x3D => Some(flags::ALTERNATE),
        0x37 | 0x36 => Some(flags::COMMAND),
        0x39 => Some(flags::CAPS_LOCK),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn letters_digits_and_arrows() {
        assert_eq!(mac_keycode(ScanKey::new(0x1E, false)), Some(0x00)); // A
        assert_eq!(mac_keycode(ScanKey::new(0x2C, false)), Some(0x06)); // Z
        assert_eq!(mac_keycode(ScanKey::new(0x0B, false)), Some(0x1D)); // 0
        assert_eq!(mac_keycode(ScanKey::ENTER), Some(0x24));
        assert_eq!(mac_keycode(ScanKey::SPACE), Some(0x31));
        assert_eq!(mac_keycode(ScanKey::LEFT), Some(0x7B));
        assert_eq!(mac_keycode(ScanKey::UP), Some(0x7E));
        assert_eq!(mac_keycode(ScanKey::DELETE), Some(0x75));
    }

    #[test]
    fn extended_flag_selects_the_other_key() {
        // 0x1C: Enter vs keypad Enter; 0x47: keypad 7 vs Home.
        assert_eq!(mac_keycode(ScanKey::new(0x1C, true)), Some(0x4C));
        assert_eq!(mac_keycode(ScanKey::new(0x47, false)), Some(0x59));
        assert_eq!(mac_keycode(ScanKey::new(0x47, true)), Some(0x73));
    }

    #[test]
    fn arabic_layout_keys_use_the_same_physical_codes() {
        // The Arabic letter alef sits on the physical "H" key of the Windows Arabic layout
        // (scancode 0x23); the host layout, not this table, produces the glyph.
        assert_eq!(mac_keycode(ScanKey::new(0x23, false)), Some(0x04));
    }

    #[test]
    fn windows_key_is_command_alt_is_option() {
        assert_eq!(mac_keycode(ScanKey::LEFT_WIN), Some(0x37));
        assert_eq!(mac_keycode(ScanKey::RIGHT_WIN), Some(0x36));
        assert_eq!(mac_keycode(ScanKey::LEFT_ALT), Some(0x3A));
        assert_eq!(mac_keycode(ScanKey::RIGHT_ALT), Some(0x3D));
    }

    #[test]
    fn unmappable_keys_are_none() {
        assert_eq!(mac_keycode(ScanKey::new(0x46, false)), None); // Scroll Lock
        assert_eq!(mac_keycode(ScanKey::new(0x37, true)), None); // Print Screen
        assert_eq!(mac_keycode(ScanKey::new(0x7F, false)), None);
        assert_eq!(mac_keycode(ScanKey::new(0xFFFF, true)), None);
    }

    #[test]
    fn no_two_physical_keys_share_a_mac_keycode() {
        let mut seen = HashSet::new();
        for ext in [false, true] {
            for sc in 0..=0xFFu16 {
                if let Some(code) = mac_keycode(ScanKey::new(sc, ext)) {
                    assert!(seen.insert(code), "duplicate mac keycode {code:#x}");
                }
            }
        }
        assert!(seen.len() > 90);
    }

    #[test]
    fn modifiers_have_flags() {
        for (key, flag) in [
            (ScanKey::LEFT_SHIFT, flags::SHIFT),
            (ScanKey::RIGHT_SHIFT, flags::SHIFT),
            (ScanKey::LEFT_CTRL, flags::CONTROL),
            (ScanKey::RIGHT_CTRL, flags::CONTROL),
            (ScanKey::LEFT_ALT, flags::ALTERNATE),
            (ScanKey::RIGHT_ALT, flags::ALTERNATE),
            (ScanKey::LEFT_WIN, flags::COMMAND),
            (ScanKey::RIGHT_WIN, flags::COMMAND),
        ] {
            let code = mac_keycode(key).unwrap_or(u16::MAX);
            assert_eq!(modifier_flag(code), Some(flag), "{key:?}");
        }
        assert_eq!(modifier_flag(0x00), None);
    }
}
