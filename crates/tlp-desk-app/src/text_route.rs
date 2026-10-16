//! Decides how typed characters of the session view travel to the host.
//!
//! Two paths exist: **scancodes** (`InputEvent::Key`, the host's own layout decides the
//! character; right for shortcuts, games and normal typing) and **Unicode text**
//! (`InputEvent::Text`, protocol v6; the character arrives as typed, whatever the host layout).
//!
//! The rule, in one place so a character is never sent both ways:
//!
//! * Default ([`TextRoute::Scancodes`]): every key goes as a scancode. An egui `Text` event that
//!   follows a text-producing key press in the same frame is that key's own character and is
//!   dropped (the host already produces it). `Text` or an IME commit with *no* such key press
//!   (composed text, IME, emoji, characters without a key on this layout) is sent as `Text`.
//! * "Type text using my keyboard layout" ([`TextRoute::Layout`]): text-producing keys pressed
//!   without Ctrl/Alt/Cmd are *not* sent as scancodes (their release is swallowed too); the
//!   character from the `Text` event is sent instead, so Arabic typed on the viewer appears
//!   as Arabic even if the host is set to English. Everything else (Enter, Backspace, arrows,
//!   function keys, modifiers, any shortcut) stays a scancode.

use std::collections::BTreeSet;

use eframe::egui::{Key, Modifiers};
use tlp_desk_proto::{InputEvent, MAX_INPUT_TEXT_CHARS};

use crate::input_map;

/// Which path ordinary characters take (see the module docs).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TextRoute {
    /// Scancodes for keys, `Text` only for input without a key.
    #[default]
    Scancodes,
    /// Characters typed as `Text`, so the viewer's layout wins.
    Layout,
}

impl TextRoute {
    /// The path chosen by the "Type text using my keyboard layout" setting.
    pub fn from_layout_setting(type_with_my_layout: bool) -> Self {
        if type_with_my_layout {
            Self::Layout
        } else {
            Self::Scancodes
        }
    }
}

/// Keys that produce a printable character (letters, digits, punctuation, space).
pub fn is_text_key(key: Key) -> bool {
    use Key::*;
    matches!(
        key,
        A | B
            | C
            | D
            | E
            | F
            | G
            | H
            | I
            | J
            | K
            | L
            | M
            | N
            | O
            | P
            | Q
            | R
            | S
            | T
            | U
            | V
            | W
            | X
            | Y
            | Z
            | Num0
            | Num1
            | Num2
            | Num3
            | Num4
            | Num5
            | Num6
            | Num7
            | Num8
            | Num9
            | Space
            | Minus
            | Equals
            | Plus
            | Exclamationmark
            | OpenBracket
            | CloseBracket
            | OpenCurlyBracket
            | CloseCurlyBracket
            | Semicolon
            | Colon
            | Quote
            | Backtick
            | Backslash
            | Pipe
            | Comma
            | Period
            | Slash
            | Questionmark
            | IntlBackslash
    )
}

/// Per-session key/text router.
#[derive(Debug, Default)]
pub struct TextRouter {
    route: TextRoute,
    /// Text-producing key presses of this frame whose `Text` event has not arrived yet.
    pending: u32,
    /// Keys whose press was turned into `Text`; their release is swallowed.
    suppressed: BTreeSet<(u16, bool)>,
    /// A Windows key is held (egui has no modifier flag for it): Win+Space switches the
    /// keyboard layout and Win+letter are shortcuts, so they are never turned into text.
    win_held: BTreeSet<Key>,
}

impl TextRouter {
    /// Switches the path (takes effect for the next key press).
    pub fn set_route(&mut self, route: TextRoute) {
        self.route = route;
    }

    /// The current path.
    pub fn route(&self) -> TextRoute {
        self.route
    }

    /// Call once per frame before the events: a `Text` only pairs with a key of its own frame.
    pub fn begin_frame(&mut self) {
        self.pending = 0;
    }

    /// Forget swallowed keys (the caller sends `ReleaseAll`).
    pub fn clear(&mut self) {
        self.pending = 0;
        self.suppressed.clear();
        self.win_held.clear();
    }

    /// A key event: the scancode event to send, or `None` if there is nothing to send (no
    /// scancode, or the character travels as text).
    pub fn key(
        &mut self,
        key: Key,
        physical_key: Option<Key>,
        modifiers: Modifiers,
        pressed: bool,
    ) -> Option<InputEvent> {
        let ev = input_map::key_event(key, physical_key, pressed)?;
        let InputEvent::Key {
            scancode, extended, ..
        } = ev
        else {
            return Some(ev);
        };
        let id = (scancode, extended);
        if matches!(key, Key::SuperLeft | Key::SuperRight) {
            if pressed {
                self.win_held.insert(key);
            } else {
                self.win_held.remove(&key);
            }
        }
        if !pressed {
            return if self.suppressed.remove(&id) {
                None
            } else {
                Some(ev)
            };
        }
        let shortcut = modifiers.ctrl
            || modifiers.alt
            || modifiers.command
            || modifiers.mac_cmd
            || !self.win_held.is_empty();
        if is_text_key(physical_key.unwrap_or(key)) && !shortcut {
            self.pending = self.pending.saturating_add(1);
            if self.route == TextRoute::Layout {
                self.suppressed.insert(id);
                return None;
            }
        }
        Some(ev)
    }

    /// An egui `Text` event: the `Text` events to send (none when it is the character of a key
    /// already sent as a scancode).
    pub fn text(&mut self, text: &str) -> Vec<InputEvent> {
        if self.pending > 0 {
            self.pending -= 1;
            if self.route == TextRoute::Scancodes {
                return Vec::new();
            }
        }
        text_events(text)
    }

    /// An IME commit: always text (there is no key to pair with).
    pub fn commit(&mut self, text: &str) -> Vec<InputEvent> {
        text_events(text)
    }
}

/// Splits `text` into `InputEvent::Text` events of at most [`MAX_INPUT_TEXT_CHARS`] characters,
/// dropping control characters (the host would refuse them).
pub fn text_events(text: &str) -> Vec<InputEvent> {
    let clean: Vec<char> = text
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\t' | '\n'))
        .collect();
    clean
        .chunks(MAX_INPUT_TEXT_CHARS)
        .map(|chunk| InputEvent::Text {
            text: chunk.iter().collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router(route: TextRoute) -> TextRouter {
        let mut r = TextRouter::default();
        r.set_route(route);
        r
    }

    fn text(s: &str) -> InputEvent {
        InputEvent::Text { text: s.into() }
    }

    fn down(r: &mut TextRouter, key: Key, m: Modifiers) -> Option<InputEvent> {
        r.key(key, None, m, true)
    }

    #[test]
    fn default_route_sends_the_scancode_and_drops_its_own_character() {
        let mut r = router(TextRoute::Scancodes);
        r.begin_frame();
        assert!(matches!(
            down(&mut r, Key::A, Modifiers::NONE),
            Some(InputEvent::Key {
                scancode: 0x1E,
                down: true,
                ..
            })
        ));
        assert!(
            r.text("ش").is_empty(),
            "the host makes this character itself"
        );
        assert!(r.key(Key::A, None, Modifiers::NONE, false).is_some());
    }

    #[test]
    fn default_route_sends_text_without_a_key_press() {
        let mut r = router(TextRoute::Scancodes);
        r.begin_frame();
        // IME commit, emoji picker, composed text: no key of their own.
        assert_eq!(r.text("😀"), vec![text("😀")]);
        assert_eq!(r.commit("مرحبا"), vec![text("مرحبا")]);
    }

    #[test]
    fn pending_key_does_not_leak_into_the_next_frame() {
        let mut r = router(TextRoute::Scancodes);
        r.begin_frame();
        down(&mut r, Key::A, Modifiers::NONE);
        r.begin_frame();
        assert_eq!(r.text("é"), vec![text("é")]);
    }

    #[test]
    fn layout_route_sends_text_instead_of_the_text_key() {
        let mut r = router(TextRoute::Layout);
        r.begin_frame();
        assert_eq!(down(&mut r, Key::A, Modifiers::NONE), None);
        assert_eq!(r.text("ش"), vec![text("ش")]);
        // The release of a suppressed key is swallowed too (nothing was pressed on the host).
        assert_eq!(r.key(Key::A, None, Modifiers::NONE, false), None);
        // A later plain release is not swallowed.
        assert!(r.key(Key::A, None, Modifiers::NONE, false).is_some());
    }

    #[test]
    fn layout_route_keeps_non_text_keys_and_shortcuts_as_scancodes() {
        let mut r = router(TextRoute::Layout);
        r.begin_frame();
        for key in [
            Key::Enter,
            Key::Backspace,
            Key::ArrowLeft,
            Key::F5,
            Key::ShiftLeft,
        ] {
            assert!(down(&mut r, key, Modifiers::NONE).is_some(), "{key:?}");
        }
        assert!(down(&mut r, Key::C, Modifiers::CTRL).is_some());
        assert!(down(&mut r, Key::A, Modifiers::ALT).is_some());
    }

    #[test]
    fn layout_switch_shortcuts_stay_scancodes_in_layout_mode() {
        let mut r = router(TextRoute::Layout);
        r.begin_frame();
        // Alt+Shift: both are modifiers.
        assert!(down(&mut r, Key::AltLeft, Modifiers::NONE).is_some());
        assert!(down(&mut r, Key::ShiftLeft, Modifiers::ALT).is_some());
        // Win+Space.
        assert!(down(&mut r, Key::SuperLeft, Modifiers::NONE).is_some());
        assert!(down(&mut r, Key::Space, Modifiers::NONE).is_some());
        // Releasing the Windows key makes Space a text key again.
        r.key(Key::SuperLeft, None, Modifiers::NONE, false);
        assert_eq!(down(&mut r, Key::Space, Modifiers::NONE), None);
    }

    #[test]
    fn layout_route_uses_the_physical_key_for_the_decision() {
        let mut r = router(TextRoute::Layout);
        r.begin_frame();
        // Arabic layout: the logical key is not a Latin one, the physical position is "F".
        assert_eq!(r.key(Key::Num0, Some(Key::F), Modifiers::NONE, true), None);
    }

    #[test]
    fn layout_text_without_key_is_still_sent() {
        let mut r = router(TextRoute::Layout);
        r.begin_frame();
        assert_eq!(r.text("ب"), vec![text("ب")]);
    }

    #[test]
    fn long_text_is_chunked_and_control_characters_are_dropped() {
        let long = "ب".repeat(MAX_INPUT_TEXT_CHARS * 2 + 5);
        let events = text_events(&long);
        assert_eq!(events.len(), 3);
        assert!(events.iter().all(|e| matches!(e,
            InputEvent::Text { text } if text.chars().count() <= MAX_INPUT_TEXT_CHARS)));
        assert_eq!(text_events("a\u{1b}b"), vec![text("ab")]);
        assert!(text_events("\u{7f}").is_empty());
        assert_eq!(text_events("x\ny"), vec![text("x\ny")]);
    }

    #[test]
    fn clear_forgets_swallowed_keys() {
        let mut r = router(TextRoute::Layout);
        r.begin_frame();
        down(&mut r, Key::A, Modifiers::NONE);
        r.clear();
        assert!(r.key(Key::A, None, Modifiers::NONE, false).is_some());
    }

    #[test]
    fn text_key_table_matches_the_scancode_table() {
        for key in Key::ALL {
            if is_text_key(*key) {
                assert!(input_map::scancode(*key).is_some(), "{key:?}");
            }
        }
        assert!(!is_text_key(Key::Enter) && !is_text_key(Key::Tab));
    }
}
