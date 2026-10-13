//! Platform-independent decision logic of the viewer keyboard grab, kept pure for tests.

// Only the Windows hook and the tests drive this today.
#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::BTreeSet;

use crate::keys::{normalize_scancode, ScanKey};
use crate::InputEvent;

/// What the hook does with one OS key event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Decision {
    /// Events to send to the host, in order.
    pub emit: Vec<InputEvent>,
    /// True: swallow the key locally. False: let the local OS see it.
    pub suppress: bool,
}

impl Decision {
    fn pass() -> Self {
        Self {
            emit: Vec::new(),
            suppress: false,
        }
    }
}

/// Tracks keys whose key-down was swallowed and forwarded, so their key-up is swallowed and
/// forwarded too, while key-ups of keys pressed before the grab started still reach the local
/// OS (otherwise e.g. the Alt used to Alt+Tab into the viewer would stay stuck locally).
#[derive(Debug, Default)]
pub(crate) struct GrabLogic {
    forwarded: BTreeSet<ScanKey>,
}

impl GrabLogic {
    /// Forgets forwarded keys (after the caller emitted `ReleaseAll`).
    pub(crate) fn reset(&mut self) {
        self.forwarded.clear();
    }

    /// `grab_now`: grab enabled and the session window is in the foreground.
    /// `injected`: the event was synthesized by software (never forwarded, never swallowed,
    /// so a host and viewer on the same machine cannot feed back into each other).
    pub(crate) fn on_key(
        &mut self,
        grab_now: bool,
        injected: bool,
        scancode: u16,
        extended: bool,
        down: bool,
    ) -> Decision {
        if injected {
            return Decision::pass();
        }
        if !grab_now {
            // Focus left without set_active(false) (e.g. Win+L, a UAC prompt, another app
            // stole the foreground): release what the host holds, then behave normally.
            if self.forwarded.is_empty() {
                return Decision::pass();
            }
            self.forwarded.clear();
            return Decision {
                emit: vec![InputEvent::ReleaseAll],
                suppress: false,
            };
        }
        let Some(key) = normalize_scancode(scancode, extended) else {
            return Decision::pass();
        };
        let forward = if down {
            self.forwarded.insert(key);
            true
        } else {
            self.forwarded.remove(&key)
        };
        if !forward {
            return Decision::pass();
        }
        Decision {
            emit: vec![InputEvent::Key {
                scancode: key.scancode,
                extended: key.extended,
                down,
            }],
            suppress: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(scancode: u16, extended: bool, down: bool) -> InputEvent {
        InputEvent::Key {
            scancode,
            extended,
            down,
        }
    }

    #[test]
    fn grabbed_system_combos_are_forwarded_and_suppressed() {
        let mut g = GrabLogic::default();
        // Win, Alt+Tab, Alt+Esc, Ctrl+Esc.
        let seq = [
            (0x5B, true, true),
            (0x5B, true, false),
            (0x38, false, true),
            (0x0F, false, true),
            (0x0F, false, false),
            (0x01, false, true),
            (0x01, false, false),
            (0x38, false, false),
            (0x1D, false, true),
            (0x01, false, true),
            (0x01, false, false),
            (0x1D, false, false),
        ];
        for (sc, ext, down) in seq {
            let d = g.on_key(true, false, sc, ext, down);
            assert!(d.suppress);
            assert_eq!(d.emit, vec![k(sc, ext, down)]);
        }
    }

    #[test]
    fn not_grabbing_passes_through() {
        let mut g = GrabLogic::default();
        assert_eq!(g.on_key(false, false, 0x1E, false, true), Decision::pass());
        assert_eq!(g.on_key(false, false, 0x1E, false, false), Decision::pass());
    }

    #[test]
    fn injected_events_are_ignored() {
        let mut g = GrabLogic::default();
        assert_eq!(g.on_key(true, true, 0x1E, false, true), Decision::pass());
    }

    #[test]
    fn keyup_of_key_pressed_before_grab_reaches_local_os() {
        let mut g = GrabLogic::default();
        // Alt was down locally before the viewer got focus; its release must not be eaten.
        assert_eq!(g.on_key(true, false, 0x38, false, false), Decision::pass());
    }

    #[test]
    fn losing_foreground_releases_remote_keys_once() {
        let mut g = GrabLogic::default();
        g.on_key(true, false, 0x1D, false, true);
        let d = g.on_key(false, false, 0x26, false, true);
        assert_eq!(d.emit, vec![InputEvent::ReleaseAll]);
        assert!(!d.suppress);
        assert_eq!(g.on_key(false, false, 0x26, false, false), Decision::pass());
    }

    #[test]
    fn reset_forgets_forwarded_keys() {
        let mut g = GrabLogic::default();
        g.on_key(true, false, 0x2A, false, true);
        g.reset();
        assert_eq!(g.on_key(true, false, 0x2A, false, false), Decision::pass());
    }

    #[test]
    fn extended_and_prefixed_scancodes_normalize() {
        let mut g = GrabLogic::default();
        let d = g.on_key(true, false, 0xE01D, false, true);
        assert_eq!(d.emit, vec![k(0x1D, true, true)]);
        let d = g.on_key(true, false, 0x1D, true, false);
        assert_eq!(d.emit, vec![k(0x1D, true, false)]);
        // Unrepresentable: passes locally.
        assert_eq!(g.on_key(true, false, 0, false, true), Decision::pass());
    }
}
