//! Soft-keyboard text to host input (pure logic).
//!
//! The wire carries physical keys (scancodes), not characters, so the host's own keyboard layout
//! decides what appears. Latin letters, digits and punctuation of a **US layout** are sent as key
//! presses (with Shift where needed). Any other character (Arabic, accented letters, emoji) has
//! no scancode: [`plan_text`] groups such runs so the caller can send them through the clipboard
//! and press Ctrl+V on the host instead.

use tlp_desk_proto::InputEvent;

const SHIFT: (u16, bool) = (0x2A, false);
const CTRL: (u16, bool) = (0x1D, false);
const KEY_V: (u16, bool) = (0x2F, false);

/// A special key on the soft keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecialKey {
    /// Backspace.
    Backspace,
    /// Enter / Return.
    Enter,
    /// Tab.
    Tab,
    /// Escape.
    Escape,
    /// Delete (forward).
    Delete,
    /// Arrow left.
    Left,
    /// Arrow right.
    Right,
    /// Arrow up.
    Up,
    /// Arrow down.
    Down,
    /// Home.
    Home,
    /// End.
    End,
}

fn key(code: (u16, bool), down: bool) -> InputEvent {
    InputEvent::Key {
        scancode: code.0,
        extended: code.1,
        down,
    }
}

fn tap(code: (u16, bool)) -> [InputEvent; 2] {
    [key(code, true), key(code, false)]
}

/// Scancode of a special key.
pub fn special_scancode(k: SpecialKey) -> (u16, bool) {
    match k {
        SpecialKey::Backspace => (0x0E, false),
        SpecialKey::Enter => (0x1C, false),
        SpecialKey::Tab => (0x0F, false),
        SpecialKey::Escape => (0x01, false),
        SpecialKey::Delete => (0x53, true),
        SpecialKey::Left => (0x4B, true),
        SpecialKey::Right => (0x4D, true),
        SpecialKey::Up => (0x48, true),
        SpecialKey::Down => (0x50, true),
        SpecialKey::Home => (0x47, true),
        SpecialKey::End => (0x4F, true),
    }
}

/// Press and release a special key.
pub fn special_key(k: SpecialKey) -> Vec<InputEvent> {
    tap(special_scancode(k)).to_vec()
}

/// US-layout scancode of an ASCII character and whether Shift is needed.
pub fn ascii_scancode(c: char) -> Option<((u16, bool), bool)> {
    const ROW_DIGITS: [char; 10] = ['1', '2', '3', '4', '5', '6', '7', '8', '9', '0'];
    const SHIFTED_DIGITS: [char; 10] = ['!', '@', '#', '$', '%', '^', '&', '*', '(', ')'];
    const ROW_Q: &str = "qwertyuiop";
    const ROW_A: &str = "asdfghjkl";
    const ROW_Z: &str = "zxcvbnm";
    let lower = c.to_ascii_lowercase();
    let shift_letter = c.is_ascii_uppercase();
    if let Some(i) = ROW_Q.find(lower) {
        return Some(((0x10 + i as u16, false), shift_letter));
    }
    if let Some(i) = ROW_A.find(lower) {
        return Some(((0x1E + i as u16, false), shift_letter));
    }
    if let Some(i) = ROW_Z.find(lower) {
        return Some(((0x2C + i as u16, false), shift_letter));
    }
    if let Some(i) = ROW_DIGITS.iter().position(|&d| d == c) {
        return Some(((0x02 + i as u16, false), false));
    }
    if let Some(i) = SHIFTED_DIGITS.iter().position(|&d| d == c) {
        return Some(((0x02 + i as u16, false), true));
    }
    let (code, shift) = match c {
        ' ' => (0x39, false),
        '-' => (0x0C, false),
        '_' => (0x0C, true),
        '=' => (0x0D, false),
        '+' => (0x0D, true),
        '[' => (0x1A, false),
        '{' => (0x1A, true),
        ']' => (0x1B, false),
        '}' => (0x1B, true),
        '\\' => (0x2B, false),
        '|' => (0x2B, true),
        ';' => (0x27, false),
        ':' => (0x27, true),
        '\'' => (0x28, false),
        '"' => (0x28, true),
        '`' => (0x29, false),
        '~' => (0x29, true),
        ',' => (0x33, false),
        '<' => (0x33, true),
        '.' => (0x34, false),
        '>' => (0x34, true),
        '/' => (0x35, false),
        '?' => (0x35, true),
        '\n' => (0x1C, false),
        '\t' => (0x0F, false),
        _ => return None,
    };
    Some(((code, false), shift))
}

/// Key events that type one ASCII character, or `None` if it has no US-layout key.
pub fn type_char(c: char) -> Option<Vec<InputEvent>> {
    let (code, shift) = ascii_scancode(c)?;
    let mut out = Vec::with_capacity(4);
    if shift {
        out.push(key(SHIFT, true));
    }
    out.extend(tap(code));
    if shift {
        out.push(key(SHIFT, false));
    }
    Some(out)
}

/// One step of typing a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextStep {
    /// Send these key events.
    Keys(Vec<InputEvent>),
    /// Put this text on the host clipboard, then send Ctrl+V ([`paste_keys`]).
    Paste(String),
}

/// Splits `text` into key presses for characters a US layout has and clipboard pastes for the
/// rest, keeping the original order. Control characters other than newline and tab are dropped.
pub fn plan_text(text: &str) -> Vec<TextStep> {
    let mut steps: Vec<TextStep> = Vec::new();
    for c in text.chars() {
        if let Some(keys) = type_char(c) {
            match steps.last_mut() {
                Some(TextStep::Keys(k)) => k.extend(keys),
                _ => steps.push(TextStep::Keys(keys)),
            }
        } else if !c.is_control() {
            match steps.last_mut() {
                Some(TextStep::Paste(s)) => s.push(c),
                _ => steps.push(TextStep::Paste(c.to_string())),
            }
        }
    }
    steps
}

/// Ctrl+V.
pub fn paste_keys() -> Vec<InputEvent> {
    vec![
        key(CTRL, true),
        key(KEY_V, true),
        key(KEY_V, false),
        key(CTRL, false),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lower_and_upper_letters() {
        let a = type_char('a').unwrap_or_default();
        assert_eq!(a, tap((0x1E, false)).to_vec());
        let upper = type_char('A').unwrap_or_default();
        assert_eq!(upper.len(), 4);
        assert_eq!(upper[0], key(SHIFT, true));
        assert_eq!(upper[3], key(SHIFT, false));
        assert_eq!(type_char('z').map(|v| v.len()), Some(2));
        assert_eq!(
            type_char('m').unwrap_or_default()[0],
            key((0x32, false), true)
        );
    }

    #[test]
    fn digits_symbols_and_whitespace() {
        assert_eq!(
            type_char('1').unwrap_or_default()[0],
            key((0x02, false), true)
        );
        assert_eq!(
            type_char('0').unwrap_or_default()[0],
            key((0x0B, false), true)
        );
        assert_eq!(type_char('!').unwrap_or_default().len(), 4);
        assert_eq!(
            type_char(' ').unwrap_or_default()[0],
            key((0x39, false), true)
        );
        assert_eq!(
            type_char('\n').unwrap_or_default()[0],
            key((0x1C, false), true)
        );
        assert_eq!(
            type_char('?').unwrap_or_default()[1],
            key((0x35, false), true)
        );
    }

    #[test]
    fn every_printable_ascii_char_is_typeable_and_balanced() {
        for c in (0x20u8..0x7F).map(char::from) {
            let keys = type_char(c).unwrap_or_default();
            assert!(!keys.is_empty(), "{c:?}");
            let downs = keys
                .iter()
                .filter(|e| matches!(e, InputEvent::Key { down: true, .. }))
                .count();
            assert_eq!(downs * 2, keys.len(), "{c:?} leaves a key held");
        }
    }

    #[test]
    fn arabic_goes_through_the_clipboard_in_order() {
        let plan = plan_text("ID \u{645}\u{631}\u{62d}\u{628}\u{627} 7");
        assert_eq!(plan.len(), 3);
        assert!(matches!(&plan[0], TextStep::Keys(k) if k.len() == 10));
        assert_eq!(
            plan[1],
            TextStep::Paste("\u{645}\u{631}\u{62d}\u{628}\u{627}".to_owned())
        );
        assert!(matches!(&plan[2], TextStep::Keys(_)));
    }

    #[test]
    fn control_characters_are_dropped() {
        assert!(plan_text("\u{0}\u{7}\u{1b}").is_empty());
        assert!(plan_text("").is_empty());
    }

    #[test]
    fn paste_is_ctrl_v_balanced() {
        let p = paste_keys();
        assert_eq!(p.first(), Some(&key(CTRL, true)));
        assert_eq!(p.last(), Some(&key(CTRL, false)));
    }

    #[test]
    fn special_keys_use_extended_codes_for_navigation() {
        assert_eq!(special_scancode(SpecialKey::Left), (0x4B, true));
        assert_eq!(special_scancode(SpecialKey::Backspace), (0x0E, false));
        assert_eq!(special_key(SpecialKey::Enter).len(), 2);
    }
}
