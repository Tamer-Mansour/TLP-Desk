//! Testing helper for `connect --type`: turns text into the scancode events a US keyboard
//! would send, so the whole viewer-to-host keyboard path can be tested without a GUI.

use tlp_desk_proto::InputEvent;

const LSHIFT: u16 = 0x2A;
const LCTRL: u16 = 0x1D;

/// Set-1 scancode of `c` on a US layout and whether Shift is needed. `None` for characters
/// the helper cannot type (the host's layout decides everything else anyway).
fn us_key(c: char) -> Option<(u16, bool)> {
    const ROWS: [(&str, &str, u16); 4] = [
        ("1234567890-=", "!@#$%^&*()_+", 0x02),
        ("qwertyuiop[]", "QWERTYUIOP{}", 0x10),
        ("asdfghjkl;'`", "ASDFGHJKL:\"~", 0x1E),
        ("\\zxcvbnm,./", "|ZXCVBNM<>?", 0x2B),
    ];
    for (plain, shifted, first) in ROWS {
        // The home row's backtick and the bottom row's backslash are out of sequence.
        let code_of = |i: usize| -> u16 {
            let i = u16::try_from(i).unwrap_or(0);
            match (first, i) {
                (0x1E, 11) => 0x29,
                (0x2B, 0) => 0x2B,
                (0x2B, n) => 0x2B + n,
                (f, n) => f + n,
            }
        };
        if let Some(i) = plain.chars().position(|p| p == c) {
            return Some((code_of(i), false));
        }
        if let Some(i) = shifted.chars().position(|p| p == c) {
            return Some((code_of(i), true));
        }
    }
    match c {
        ' ' => Some((0x39, false)),
        '\n' => Some((0x1C, false)),
        '\t' => Some((0x0F, false)),
        '\u{8}' => Some((0x0E, false)),
        _ => None,
    }
}

fn key(scancode: u16, down: bool) -> InputEvent {
    InputEvent::Key {
        scancode,
        extended: false,
        down,
    }
}

/// Key events that type `text` (`\n` = Enter, `\t` = Tab, `\x08` = Backspace). Characters
/// without a US key are skipped.
pub fn typing_events(text: &str) -> Vec<InputEvent> {
    let mut out = Vec::new();
    for c in text.chars() {
        let Some((code, shift)) = us_key(c) else {
            continue;
        };
        if shift {
            out.push(key(LSHIFT, true));
        }
        out.push(key(code, true));
        out.push(key(code, false));
        if shift {
            out.push(key(LSHIFT, false));
        }
    }
    out
}

/// Ctrl+`letter` (e.g. Ctrl+S); empty when `letter` is not a letter.
pub fn ctrl_chord(letter: char) -> Vec<InputEvent> {
    match us_key(letter.to_ascii_lowercase()) {
        Some((code, false)) if letter.is_ascii_alphabetic() => vec![
            key(LCTRL, true),
            key(code, true),
            key(code, false),
            key(LCTRL, false),
        ],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_digits_and_punctuation_use_us_scancodes() {
        for (c, code, shift) in [
            ('a', 0x1E, false),
            ('z', 0x2C, false),
            ('m', 0x32, false),
            ('q', 0x10, false),
            ('1', 0x02, false),
            ('0', 0x0B, false),
            ('-', 0x0C, false),
            ('=', 0x0D, false),
            (';', 0x27, false),
            ('\'', 0x28, false),
            ('`', 0x29, false),
            ('\\', 0x2B, false),
            (',', 0x33, false),
            ('.', 0x34, false),
            ('/', 0x35, false),
            ('A', 0x1E, true),
            ('!', 0x02, true),
            ('?', 0x35, true),
            ('_', 0x0C, true),
            (' ', 0x39, false),
            ('\n', 0x1C, false),
            ('\u{8}', 0x0E, false),
        ] {
            assert_eq!(us_key(c), Some((code, shift)), "{c:?}");
        }
        assert_eq!(us_key('م'), None);
    }

    #[test]
    fn shifted_characters_are_wrapped_in_shift() {
        let ev = typing_events("A");
        assert_eq!(
            ev,
            vec![
                key(LSHIFT, true),
                key(0x1E, true),
                key(0x1E, false),
                key(LSHIFT, false)
            ]
        );
        // Arabic is skipped (typing is by scancode; the host layout decides the character).
        assert_eq!(typing_events("م"), vec![]);
    }

    #[test]
    fn ctrl_chord_presses_and_releases_in_order() {
        let ev = ctrl_chord('S');
        assert_eq!(ev.first(), Some(&key(LCTRL, true)));
        assert_eq!(ev.get(1), Some(&key(0x1F, true)));
        assert_eq!(ev.last(), Some(&key(LCTRL, false)));
        assert!(ctrl_chord('1').is_empty());
    }
}
