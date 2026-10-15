//! Translation of egui input into protocol [`InputEvent`]s.
//!
//! The keyboard table maps `egui::Key` (physical position when egui provides it) to PC/AT
//! **scancode set 1** plus the `E0` "extended" flag, which is what `SendInput` with
//! `KEYEVENTF_SCANCODE` expects on the host. egui cannot see every key (Win key, Alt+Tab,
//! PrintScreen, CapsLock, numpad specifics are swallowed by the OS or not exposed); Lane C's
//! low-level keyboard hook replaces this path for those on Windows.

use eframe::egui::{self, Key, Pos2, Rect, Vec2};
use tlp_desk_proto::{InputEvent, MouseButton};

/// Set-1 scancode for a key: `(scancode, extended)`.
pub fn scancode(key: Key) -> Option<(u16, bool)> {
    use Key::*;
    let (code, ext) = match key {
        Escape => (0x01, false),
        Num1 | Exclamationmark => (0x02, false),
        Num2 => (0x03, false),
        Num3 => (0x04, false),
        Num4 => (0x05, false),
        Num5 => (0x06, false),
        Num6 => (0x07, false),
        Num7 => (0x08, false),
        Num8 => (0x09, false),
        Num9 => (0x0A, false),
        Num0 => (0x0B, false),
        Minus => (0x0C, false),
        Equals | Plus => (0x0D, false),
        Backspace => (0x0E, false),
        Tab => (0x0F, false),
        Q => (0x10, false),
        W => (0x11, false),
        E => (0x12, false),
        R => (0x13, false),
        T => (0x14, false),
        Y => (0x15, false),
        U => (0x16, false),
        I => (0x17, false),
        O => (0x18, false),
        P => (0x19, false),
        OpenBracket | OpenCurlyBracket => (0x1A, false),
        CloseBracket | CloseCurlyBracket => (0x1B, false),
        Enter => (0x1C, false),
        ControlLeft => (0x1D, false),
        A => (0x1E, false),
        S => (0x1F, false),
        D => (0x20, false),
        F => (0x21, false),
        G => (0x22, false),
        H => (0x23, false),
        J => (0x24, false),
        K => (0x25, false),
        L => (0x26, false),
        Semicolon | Colon => (0x27, false),
        Quote => (0x28, false),
        Backtick => (0x29, false),
        ShiftLeft => (0x2A, false),
        Backslash | Pipe => (0x2B, false),
        Z => (0x2C, false),
        X => (0x2D, false),
        C => (0x2E, false),
        V => (0x2F, false),
        B => (0x30, false),
        N => (0x31, false),
        M => (0x32, false),
        Comma => (0x33, false),
        Period => (0x34, false),
        Slash | Questionmark => (0x35, false),
        ShiftRight => (0x36, false),
        AltLeft => (0x38, false),
        Space => (0x39, false),
        F1 => (0x3B, false),
        F2 => (0x3C, false),
        F3 => (0x3D, false),
        F4 => (0x3E, false),
        F5 => (0x3F, false),
        F6 => (0x40, false),
        F7 => (0x41, false),
        F8 => (0x42, false),
        F9 => (0x43, false),
        F10 => (0x44, false),
        IntlBackslash => (0x56, false),
        F11 => (0x57, false),
        F12 => (0x58, false),
        F13 => (0x64, false),
        F14 => (0x65, false),
        F15 => (0x66, false),
        F16 => (0x67, false),
        F17 => (0x68, false),
        F18 => (0x69, false),
        F19 => (0x6A, false),
        F20 => (0x6B, false),
        F21 => (0x6C, false),
        F22 => (0x6D, false),
        F23 => (0x6E, false),
        F24 => (0x76, false),
        // Extended (E0-prefixed) keys.
        ControlRight => (0x1D, true),
        AltRight => (0x38, true),
        Home => (0x47, true),
        ArrowUp => (0x48, true),
        PageUp => (0x49, true),
        ArrowLeft => (0x4B, true),
        ArrowRight => (0x4D, true),
        End => (0x4F, true),
        ArrowDown => (0x50, true),
        PageDown => (0x51, true),
        Insert => (0x52, true),
        Delete => (0x53, true),
        SuperLeft => (0x5B, true),
        SuperRight => (0x5C, true),
        BrowserBack => (0x6A, true),
        // No set-1 equivalent / logical-only keys.
        Copy | Cut | Paste | F25 | F26 | F27 | F28 | F29 | F30 | F31 | F32 | F33 | F34 | F35 => {
            return None
        }
    };
    Some((code, ext))
}

/// Convert an egui key event to a protocol key event. Prefers the physical key (layout
/// independent) when egui reports it.
pub fn key_event(key: Key, physical_key: Option<Key>, pressed: bool) -> Option<InputEvent> {
    let (scancode, extended) = scancode(physical_key.unwrap_or(key))?;
    Some(InputEvent::Key {
        scancode,
        extended,
        down: pressed,
    })
}

/// Map a pointer position inside the displayed image rectangle to normalized `0..=65535`
/// coordinates of the remote surface. Returns `None` if the position is outside the image.
pub fn normalize_pointer(pos: Pos2, image_rect: Rect) -> Option<(u16, u16)> {
    if !image_rect.contains(pos) || image_rect.width() <= 0.0 || image_rect.height() <= 0.0 {
        return None;
    }
    Some(normalize_clamped(pos, image_rect))
}

/// Like [`normalize_pointer`] but clamps positions outside the rectangle to its edge (used
/// while a button is held and the drag leaves the image).
pub fn normalize_clamped(pos: Pos2, image_rect: Rect) -> (u16, u16) {
    let to_u16 = |v: f32, min: f32, len: f32| -> u16 {
        if len <= 0.0 {
            return 0;
        }
        let f = ((v - min) / len).clamp(0.0, 1.0);
        (f * f32::from(u16::MAX)).round() as u16
    };
    (
        to_u16(pos.x, image_rect.min.x, image_rect.width()),
        to_u16(pos.y, image_rect.min.y, image_rect.height()),
    )
}

pub fn mouse_button(button: egui::PointerButton) -> MouseButton {
    match button {
        egui::PointerButton::Primary => MouseButton::Left,
        egui::PointerButton::Secondary => MouseButton::Right,
        egui::PointerButton::Middle => MouseButton::Middle,
        egui::PointerButton::Extra1 => MouseButton::Back,
        egui::PointerButton::Extra2 => MouseButton::Forward,
    }
}

/// Points per wheel notch assumed for pixel-precise (touchpad) scrolling.
const POINTS_PER_NOTCH: f32 = 40.0;
/// `WHEEL_DELTA` on Windows.
const WHEEL_DELTA: f32 = 120.0;

/// Convert an egui wheel delta to Windows wheel units (1/120 notch).
///
/// egui/winit: positive `y` = content moves down = wheel rotated away from the user, which is a
/// positive Windows `WHEEL_DELTA`. Positive `x` = content moves right = scroll left, which is
/// a *negative* Windows horizontal wheel value.
pub fn wheel_event(unit: egui::MouseWheelUnit, delta: Vec2) -> Option<InputEvent> {
    let notches = match unit {
        egui::MouseWheelUnit::Line => delta,
        egui::MouseWheelUnit::Point => delta / POINTS_PER_NOTCH,
        egui::MouseWheelUnit::Page => delta * 3.0,
    };
    let to_i16 = |v: f32| (v * WHEEL_DELTA).round().clamp(-32768.0, 32767.0) as i16;
    let (dx, dy) = (-to_i16(notches.x), to_i16(notches.y));
    (dx != 0 || dy != 0).then_some(InputEvent::MouseWheel { dx, dy })
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{pos2, vec2};

    /// The pointer is sent as a fraction of the displayed image, not in video pixels, so a
    /// 1920x1200 video of a 3840x2400 host monitor still clicks the right physical pixel.
    #[test]
    fn downscaled_video_maps_to_physical_monitor_pixels() {
        use tlp_desk_input::MonitorRect;
        let monitor = MonitorRect::new(0, 0, 3840, 2400);
        // The 1920x1200 frame shown at 960x600 points inside the window.
        let image = Rect::from_min_size(pos2(20.0, 30.0), vec2(960.0, 600.0));
        for &(fx, fy) in &[(0.0f32, 0.0f32), (0.5, 0.5), (0.25, 0.8), (1.0, 1.0)] {
            let pos = pos2(20.0 + fx * 960.0, 30.0 + fy * 600.0);
            let (nx, ny) = normalize_clamped(pos, image);
            let (px, py) = monitor.normalized_to_pixel(nx, ny);
            let (wx, wy) = (fx * 3839.0, fy * 2399.0);
            assert!(
                (px as f32 - wx).abs() <= 1.0 && (py as f32 - wy).abs() <= 1.0,
                "({fx},{fy}) -> ({px},{py}), want ({wx},{wy})"
            );
        }
    }

    #[test]
    fn scancode_spot_checks() {
        assert_eq!(scancode(Key::Escape), Some((0x01, false)));
        assert_eq!(scancode(Key::A), Some((0x1E, false)));
        assert_eq!(scancode(Key::Z), Some((0x2C, false)));
        assert_eq!(scancode(Key::Num0), Some((0x0B, false)));
        assert_eq!(scancode(Key::Enter), Some((0x1C, false)));
        assert_eq!(scancode(Key::Space), Some((0x39, false)));
        assert_eq!(scancode(Key::F1), Some((0x3B, false)));
        assert_eq!(scancode(Key::F12), Some((0x58, false)));
        assert_eq!(scancode(Key::ControlLeft), Some((0x1D, false)));
        assert_eq!(scancode(Key::ControlRight), Some((0x1D, true)));
        assert_eq!(scancode(Key::AltRight), Some((0x38, true)));
        assert_eq!(scancode(Key::ArrowUp), Some((0x48, true)));
        assert_eq!(scancode(Key::Delete), Some((0x53, true)));
        assert_eq!(scancode(Key::SuperLeft), Some((0x5B, true)));
        assert_eq!(scancode(Key::Copy), None);
    }

    /// Every key a typist needs maps to its PC/AT set-1 position (typing is by scancode, so
    /// the host's layout, Arabic included, decides the character).
    #[test]
    fn typing_keys_have_the_standard_scancodes() {
        let rows: [(&[Key], u16); 5] = [
            (
                &[
                    Key::Num1,
                    Key::Num2,
                    Key::Num3,
                    Key::Num4,
                    Key::Num5,
                    Key::Num6,
                    Key::Num7,
                    Key::Num8,
                    Key::Num9,
                    Key::Num0,
                    Key::Minus,
                    Key::Equals,
                    Key::Backspace,
                    Key::Tab,
                ],
                0x02,
            ),
            (
                &[
                    Key::Q,
                    Key::W,
                    Key::E,
                    Key::R,
                    Key::T,
                    Key::Y,
                    Key::U,
                    Key::I,
                    Key::O,
                    Key::P,
                    Key::OpenBracket,
                    Key::CloseBracket,
                    Key::Enter,
                ],
                0x10,
            ),
            (
                &[
                    Key::A,
                    Key::S,
                    Key::D,
                    Key::F,
                    Key::G,
                    Key::H,
                    Key::J,
                    Key::K,
                    Key::L,
                    Key::Semicolon,
                    Key::Quote,
                    Key::Backtick,
                ],
                0x1E,
            ),
            (
                &[
                    Key::Backslash,
                    Key::Z,
                    Key::X,
                    Key::C,
                    Key::V,
                    Key::B,
                    Key::N,
                    Key::M,
                    Key::Comma,
                    Key::Period,
                    Key::Slash,
                ],
                0x2B,
            ),
            (
                &[
                    Key::F1,
                    Key::F2,
                    Key::F3,
                    Key::F4,
                    Key::F5,
                    Key::F6,
                    Key::F7,
                ],
                0x3B,
            ),
        ];
        // The rows are contiguous runs, except where noted by the gaps below.
        for (keys, first) in rows {
            let mut expect = first;
            for key in keys {
                // Enter ends the QWERTY row at 0x1C (after the brackets at 0x1A/0x1B), the
                // home row starts at 0x1E, and the bottom row skips left Shift (0x2A).
                let code = scancode(*key).expect("mapped");
                match key {
                    Key::Enter => assert_eq!(code, (0x1C, false)),
                    Key::A => assert_eq!(code, (0x1E, false)),
                    Key::Backtick => assert_eq!(code, (0x29, false)),
                    Key::Backslash => assert_eq!(code, (0x2B, false)),
                    Key::Z => assert_eq!(code, (0x2C, false)),
                    _ => assert_eq!(code, (expect, false), "{key:?}"),
                }
                expect = match key {
                    Key::Enter | Key::Backtick => expect,
                    Key::Backslash => 0x2C,
                    _ => expect + 1,
                };
            }
        }
        for (key, code) in [
            (Key::Space, 0x39),
            (Key::ShiftLeft, 0x2A),
            (Key::ShiftRight, 0x36),
            (Key::IntlBackslash, 0x56),
            (Key::F10, 0x44),
            (Key::F11, 0x57),
            (Key::F12, 0x58),
        ] {
            assert_eq!(scancode(key), Some((code, false)), "{key:?}");
        }
        // Navigation cluster and arrows are extended (E0).
        for (key, code) in [
            (Key::Insert, 0x52),
            (Key::Home, 0x47),
            (Key::PageUp, 0x49),
            (Key::End, 0x4F),
            (Key::PageDown, 0x51),
            (Key::ArrowLeft, 0x4B),
            (Key::ArrowRight, 0x4D),
        ] {
            assert_eq!(scancode(key), Some((code, true)), "{key:?}");
        }
    }

    /// Every egui key is either mapped or deliberately unmapped (logical-only keys that no
    /// physical key sends), so a new egui version cannot silently drop a key.
    #[test]
    fn every_key_is_mapped_or_known_unmapped() {
        let unmapped: Vec<Key> = Key::ALL
            .iter()
            .copied()
            .filter(|k| scancode(*k).is_none())
            .collect();
        for k in &unmapped {
            assert!(
                matches!(
                    k,
                    Key::Copy
                        | Key::Cut
                        | Key::Paste
                        | Key::F25
                        | Key::F26
                        | Key::F27
                        | Key::F28
                        | Key::F29
                        | Key::F30
                        | Key::F31
                        | Key::F32
                        | Key::F33
                        | Key::F34
                        | Key::F35
                ),
                "{k:?} has no scancode"
            );
        }
    }

    #[test]
    fn every_mapped_key_has_unique_scancode() {
        use std::collections::HashMap;
        // Logical aliases intentionally share the scancode of their base key.
        let aliases = [
            Key::Exclamationmark,
            Key::Plus,
            Key::OpenCurlyBracket,
            Key::CloseCurlyBracket,
            Key::Colon,
            Key::Pipe,
            Key::Questionmark,
        ];
        let mut seen: HashMap<(u16, bool), Key> = HashMap::new();
        for key in Key::ALL.iter().copied().filter(|k| !aliases.contains(k)) {
            if let Some(code) = scancode(key) {
                if let Some(prev) = seen.insert(code, key) {
                    panic!("{key:?} and {prev:?} share scancode {code:?}");
                }
            }
        }
        assert!(seen.len() > 90);
    }

    #[test]
    fn key_event_prefers_physical_key() {
        // French AZERTY: logical A at the physical Q position.
        assert_eq!(
            key_event(Key::A, Some(Key::Q), true),
            Some(InputEvent::Key {
                scancode: 0x10,
                extended: false,
                down: true
            })
        );
        assert_eq!(
            key_event(Key::Enter, None, false),
            Some(InputEvent::Key {
                scancode: 0x1C,
                extended: false,
                down: false
            })
        );
    }

    #[test]
    fn pointer_normalization() {
        let rect = Rect::from_min_size(pos2(100.0, 50.0), vec2(200.0, 100.0));
        assert_eq!(normalize_pointer(pos2(100.0, 50.0), rect), Some((0, 0)));
        assert_eq!(
            normalize_pointer(pos2(300.0, 150.0), rect),
            Some((65535, 65535))
        );
        assert_eq!(
            normalize_pointer(pos2(200.0, 100.0), rect),
            Some((32768, 32768))
        );
        assert_eq!(
            normalize_pointer(pos2(150.0, 75.0), rect),
            Some((16384, 16384))
        );
        assert_eq!(normalize_pointer(pos2(99.0, 75.0), rect), None);
        assert_eq!(normalize_pointer(pos2(150.0, 151.0), rect), None);
        assert_eq!(normalize_clamped(pos2(-50.0, 500.0), rect), (0, 65535));
        let empty = Rect::from_min_size(pos2(0.0, 0.0), vec2(0.0, 0.0));
        assert_eq!(normalize_pointer(pos2(0.0, 0.0), empty), None);
    }

    #[test]
    fn wheel_conversion() {
        assert_eq!(
            wheel_event(egui::MouseWheelUnit::Line, vec2(0.0, 1.0)),
            Some(InputEvent::MouseWheel { dx: 0, dy: 120 })
        );
        assert_eq!(
            wheel_event(egui::MouseWheelUnit::Line, vec2(1.0, -2.0)),
            Some(InputEvent::MouseWheel { dx: -120, dy: -240 })
        );
        assert_eq!(
            wheel_event(egui::MouseWheelUnit::Point, vec2(0.0, 40.0)),
            Some(InputEvent::MouseWheel { dx: 0, dy: 120 })
        );
        assert_eq!(
            wheel_event(egui::MouseWheelUnit::Point, vec2(0.0, 0.0)),
            None
        );
    }
}
