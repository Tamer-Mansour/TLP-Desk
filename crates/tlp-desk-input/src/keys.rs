//! Scancode helpers shared by the injector and the keyboard grab.

/// A physical key: set-1 make code plus the `E0` "extended" prefix flag, exactly as Windows
/// reports it (`KBDLLHOOKSTRUCT`, `WM_KEYDOWN` lParam bits 16..24 and 24).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScanKey {
    pub scancode: u16,
    pub extended: bool,
}

impl ScanKey {
    pub const fn new(scancode: u16, extended: bool) -> Self {
        Self { scancode, extended }
    }

    pub const ESCAPE: Self = Self::new(0x01, false);
    pub const TAB: Self = Self::new(0x0F, false);
    pub const ENTER: Self = Self::new(0x1C, false);
    pub const LEFT_CTRL: Self = Self::new(0x1D, false);
    pub const RIGHT_CTRL: Self = Self::new(0x1D, true);
    pub const LEFT_SHIFT: Self = Self::new(0x2A, false);
    pub const RIGHT_SHIFT: Self = Self::new(0x36, false);
    pub const LEFT_ALT: Self = Self::new(0x38, false);
    /// AltGr on layouts that have it.
    pub const RIGHT_ALT: Self = Self::new(0x38, true);
    pub const SPACE: Self = Self::new(0x39, false);
    pub const LEFT_WIN: Self = Self::new(0x5B, true);
    pub const RIGHT_WIN: Self = Self::new(0x5C, true);
    pub const MENU: Self = Self::new(0x5D, true);
    pub const DELETE: Self = Self::new(0x53, true);
    pub const LEFT: Self = Self::new(0x4B, true);
    pub const RIGHT: Self = Self::new(0x4D, true);
    pub const UP: Self = Self::new(0x48, true);
    pub const DOWN: Self = Self::new(0x50, true);
}

/// Normalizes a scancode coming off the wire or from an OS API.
///
/// Accepts both the plain form (`0x1D`, `extended = true`) and the prefixed form
/// (`0xE01D`). Returns `None` for scancodes that cannot be injected as one key: `0`, and
/// `E1`-prefixed sequences (Pause/Break) or any other high byte.
pub fn normalize_scancode(scancode: u16, extended: bool) -> Option<ScanKey> {
    let low = scancode & 0x00FF;
    let key = match scancode >> 8 {
        0x00 => ScanKey::new(low, extended),
        0xE0 => ScanKey::new(low, true),
        _ => return None,
    };
    if key.scancode == 0 {
        return None;
    }
    // Some keyboards/drivers flag the shift keys as extended; Windows treats shift as never
    // extended, and an extended 0x2A is the "fake shift" of PrintScreen sequences.
    if key.scancode == 0x2A || key.scancode == 0x36 {
        return Some(ScanKey::new(key.scancode, false));
    }
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_scancode_forms() {
        assert_eq!(
            normalize_scancode(0x1E, false),
            Some(ScanKey::new(0x1E, false))
        );
        assert_eq!(normalize_scancode(0x1D, true), Some(ScanKey::RIGHT_CTRL));
        assert_eq!(normalize_scancode(0xE01D, false), Some(ScanKey::RIGHT_CTRL));
        assert_eq!(normalize_scancode(0xE05B, false), Some(ScanKey::LEFT_WIN));
        assert_eq!(normalize_scancode(0x36, true), Some(ScanKey::RIGHT_SHIFT));
        assert_eq!(normalize_scancode(0, false), None);
        assert_eq!(normalize_scancode(0xE000, false), None);
        assert_eq!(normalize_scancode(0xE11D, false), None);
        assert_eq!(normalize_scancode(0x0145, false), None);
    }
}
