//! Which window-level (egui) key events of the session view are sent to the host.
//!
//! The OS keyboard grab swallows the keys it captures, so a key event that still reaches the
//! window while the grab is on proves the grab did **not** capture it (hook removed by
//! Windows after a timeout, wrong target window, key without a scancode, ...). Those keys
//! must be forwarded from the window path, otherwise nothing could be typed at all. Key-ups
//! are only forwarded for keys this path sent down, so a key held before the grab started
//! (its up is deliberately passed through by the grab) never produces a stray event.

use std::collections::BTreeSet;

use tlp_desk_proto::InputEvent;

/// Verdict for one window key event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Forward {
    /// What to send to the host, if anything.
    pub event: Option<InputEvent>,
    /// A key-down arrived while the grab was on: the grab is not capturing keys.
    pub grab_leaked: bool,
}

/// Remembers which keys the window path holds down on the host.
#[derive(Debug, Default)]
pub(crate) struct KeyForwarder {
    held: BTreeSet<(u16, bool)>,
}

impl KeyForwarder {
    /// Decides on a key event (`InputEvent::Key` only; anything else is passed on as is).
    pub(crate) fn on_event(&mut self, grab_on: bool, ev: InputEvent) -> Forward {
        let InputEvent::Key {
            scancode,
            extended,
            down,
        } = ev
        else {
            return Forward {
                event: Some(ev),
                grab_leaked: false,
            };
        };
        let id = (scancode, extended);
        if down {
            self.held.insert(id);
            return Forward {
                event: Some(ev),
                grab_leaked: grab_on,
            };
        }
        Forward {
            event: self.held.remove(&id).then_some(ev),
            grab_leaked: false,
        }
    }

    /// Forget all held keys (the caller sends `ReleaseAll`).
    pub(crate) fn clear(&mut self) {
        self.held.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(down: bool) -> InputEvent {
        InputEvent::Key {
            scancode: 0x1E,
            extended: false,
            down,
        }
    }

    #[test]
    fn down_is_forwarded_and_flags_a_leak_only_under_grab() {
        let mut f = KeyForwarder::default();
        let r = f.on_event(false, key(true));
        assert_eq!((r.event, r.grab_leaked), (Some(key(true)), false));
        let r = f.on_event(true, key(true));
        assert_eq!((r.event, r.grab_leaked), (Some(key(true)), true));
    }

    #[test]
    fn up_without_our_down_is_dropped_with_and_without_grab() {
        let mut f = KeyForwarder::default();
        assert_eq!(f.on_event(true, key(false)).event, None);
        assert_eq!(f.on_event(false, key(false)).event, None);
    }

    #[test]
    fn up_of_a_forwarded_down_is_forwarded_once() {
        let mut f = KeyForwarder::default();
        f.on_event(true, key(true));
        assert_eq!(f.on_event(true, key(false)).event, Some(key(false)));
        assert_eq!(f.on_event(true, key(false)).event, None);
    }

    #[test]
    fn clear_forgets_held_keys() {
        let mut f = KeyForwarder::default();
        f.on_event(false, key(true));
        f.clear();
        assert_eq!(f.on_event(false, key(false)).event, None);
    }
}
