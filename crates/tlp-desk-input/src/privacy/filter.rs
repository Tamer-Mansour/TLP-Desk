//! Which low-level input events the privacy hooks swallow.
//!
//! Pure bookkeeping, no OS calls, so the rules are unit-tested:
//!
//! * **Injected events always pass.** They carry `LLKHF_INJECTED` / `LLMHF_INJECTED` (or the
//!   lower-integrity variants): our own `SendInput` for the remote viewer, or another program.
//!   Only the person at the host's physical keyboard and mouse is blocked.
//! * **Physical presses, moves and wheel turns are swallowed** while blocking is on.
//! * **A physical release passes unless its press was swallowed.** A key or button the user
//!   was already holding when privacy mode started must be allowed to come back up, otherwise
//!   Windows would keep it "down" (a stuck Shift or Ctrl would corrupt the viewer's typing).
//!   A release whose press we swallowed is swallowed too, so applications never see half a
//!   keystroke.
//! * **Draining** (after privacy mode ends while keys are still held, e.g. right after the
//!   emergency chord): new input passes, but auto-repeats and releases of keys whose press was
//!   swallowed are still swallowed until they come up. Otherwise a held `P` would start typing
//!   "ppp" into the focused window the moment the hooks were removed.

/// One event as a low-level hook reports it, already decoded from the hook structs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    /// A key, identified by its set-1 scancode and the `E0` prefix flag.
    Key {
        /// Low byte of the scancode (`KBDLLHOOKSTRUCT::scanCode`).
        scancode: u8,
        /// `LLKHF_EXTENDED`.
        extended: bool,
        /// `false` for a key release (`LLKHF_UP`).
        down: bool,
    },
    /// Pointer movement.
    MouseMove,
    /// A mouse button press or release.
    MouseButton {
        /// Which button.
        button: HookButton,
        /// `false` for a release.
        down: bool,
    },
    /// Vertical or horizontal wheel.
    Wheel,
}

/// Mouse buttons a low-level mouse hook distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookButton {
    /// Left button.
    Left,
    /// Right button.
    Right,
    /// Middle button (wheel click).
    Middle,
    /// First extra button (`XBUTTON1`, usually "back").
    X1,
    /// Second extra button (`XBUTTON2`, usually "forward").
    X2,
}

impl HookButton {
    fn bit(self) -> u8 {
        match self {
            HookButton::Left => 1,
            HookButton::Right => 2,
            HookButton::Middle => 4,
            HookButton::X1 => 8,
            HookButton::X2 => 16,
        }
    }
}

/// What the hook procedure does with an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Hand it on with `CallNextHookEx`.
    Pass,
    /// Return a non-zero value without calling the next hook: nobody sees the event.
    Swallow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Mode {
    /// Everything passes.
    #[default]
    Off,
    /// Physical input is swallowed.
    Blocking,
    /// Only the rest of already swallowed keystrokes is swallowed.
    Draining,
}

/// Decides pass/swallow per event and remembers which presses it swallowed.
#[derive(Debug, Clone, Default)]
pub struct PhysicalFilter {
    mode: Mode,
    /// One bit per `(extended, scancode)`: 2 x 256 keys.
    swallowed_keys: [u64; 8],
    swallowed_buttons: u8,
}

fn key_slot(scancode: u8, extended: bool) -> (usize, u64) {
    let index = usize::from(scancode) + if extended { 256 } else { 0 };
    (index / 64, 1u64 << (index % 64))
}

impl PhysicalFilter {
    /// A filter that blocks physical input (`blocking = true`) or lets everything pass.
    pub fn new(blocking: bool) -> Self {
        let mut f = Self::default();
        f.set_blocking(blocking);
        f
    }

    /// Whether physical input is currently blocked (not draining, not off).
    pub fn is_blocking(&self) -> bool {
        self.mode == Mode::Blocking
    }

    /// Switches blocking on, or off completely. Turning it off forgets the swallowed presses:
    /// from then on every event passes, including stray releases (harmless for Windows).
    pub fn set_blocking(&mut self, blocking: bool) {
        if blocking {
            self.mode = Mode::Blocking;
        } else {
            self.mode = Mode::Off;
            self.swallowed_keys = [0; 8];
            self.swallowed_buttons = 0;
        }
    }

    /// Stops blocking new input but keeps swallowing the auto-repeats and releases of keys
    /// (and buttons) whose press was swallowed, until [`is_drained`](Self::is_drained).
    pub fn start_draining(&mut self) {
        if self.mode == Mode::Blocking {
            self.mode = Mode::Draining;
        }
    }

    /// No swallowed key or button is still held: the hooks can go without side effects.
    pub fn is_drained(&self) -> bool {
        self.swallowed_keys.iter().all(|w| *w == 0) && self.swallowed_buttons == 0
    }

    /// The verdict for one event. `injected` is the hook's injected flag.
    pub fn decide(&mut self, event: HookEvent, injected: bool) -> Verdict {
        if injected || self.mode == Mode::Off {
            return Verdict::Pass;
        }
        let blocking = self.mode == Mode::Blocking;
        match event {
            HookEvent::MouseMove | HookEvent::Wheel => {
                if blocking {
                    Verdict::Swallow
                } else {
                    Verdict::Pass
                }
            }
            HookEvent::Key {
                scancode,
                extended,
                down,
            } => {
                let (word, bit) = key_slot(scancode, extended);
                let Some(slot) = self.swallowed_keys.get_mut(word) else {
                    return Verdict::Pass;
                };
                Self::press_or_release(slot, bit, down, blocking)
            }
            HookEvent::MouseButton { button, down } => {
                let mut bits = u64::from(self.swallowed_buttons);
                let verdict =
                    Self::press_or_release(&mut bits, u64::from(button.bit()), down, blocking);
                self.swallowed_buttons = u8::try_from(bits).unwrap_or(u8::MAX);
                verdict
            }
        }
    }

    fn press_or_release(set: &mut u64, bit: u64, down: bool, blocking: bool) -> Verdict {
        let held = *set & bit != 0;
        if down {
            if blocking {
                *set |= bit;
                Verdict::Swallow
            } else if held {
                // Auto-repeat of a key whose press we swallowed.
                Verdict::Swallow
            } else {
                Verdict::Pass
            }
        } else if held {
            *set &= !bit;
            Verdict::Swallow
        } else {
            // Pressed before blocking began (or after it ended): let Windows see it come up.
            Verdict::Pass
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(scancode: u8, extended: bool, down: bool) -> HookEvent {
        HookEvent::Key {
            scancode,
            extended,
            down,
        }
    }

    #[test]
    fn injected_events_always_pass() {
        let mut f = PhysicalFilter::new(true);
        for ev in [
            key(0x1E, false, true),
            key(0x1E, false, false),
            HookEvent::MouseMove,
            HookEvent::Wheel,
            HookEvent::MouseButton {
                button: HookButton::Left,
                down: true,
            },
        ] {
            assert_eq!(f.decide(ev, true), Verdict::Pass, "{ev:?}");
        }
    }

    #[test]
    fn physical_input_is_swallowed_while_blocking() {
        let mut f = PhysicalFilter::new(true);
        assert_eq!(f.decide(HookEvent::MouseMove, false), Verdict::Swallow);
        assert_eq!(f.decide(HookEvent::Wheel, false), Verdict::Swallow);
        assert_eq!(f.decide(key(0x1E, false, true), false), Verdict::Swallow);
        // The matching release is swallowed too: no half keystrokes reach applications.
        assert_eq!(f.decide(key(0x1E, false, false), false), Verdict::Swallow);
        let left = |down| HookEvent::MouseButton {
            button: HookButton::Left,
            down,
        };
        assert_eq!(f.decide(left(true), false), Verdict::Swallow);
        assert_eq!(f.decide(left(false), false), Verdict::Swallow);
    }

    #[test]
    fn releases_of_keys_held_before_blocking_pass() {
        let mut f = PhysicalFilter::new(true);
        // Shift was down when privacy mode started: its release must reach Windows.
        assert_eq!(f.decide(key(0x2A, false, false), false), Verdict::Pass);
        let right = |down| HookEvent::MouseButton {
            button: HookButton::Right,
            down,
        };
        assert_eq!(f.decide(right(false), false), Verdict::Pass);
        // Extended and plain variants are different keys (Right Ctrl vs Left Ctrl).
        assert_eq!(f.decide(key(0x1D, false, true), false), Verdict::Swallow);
        assert_eq!(f.decide(key(0x1D, true, false), false), Verdict::Pass);
        assert_eq!(f.decide(key(0x1D, false, false), false), Verdict::Swallow);
        // A second release of the same key is no longer "swallowed".
        assert_eq!(f.decide(key(0x1D, false, false), false), Verdict::Pass);
    }

    #[test]
    fn not_blocking_passes_everything_and_forgets_state() {
        let mut f = PhysicalFilter::new(false);
        assert!(!f.is_blocking());
        assert_eq!(f.decide(HookEvent::MouseMove, false), Verdict::Pass);
        f.set_blocking(true);
        assert_eq!(f.decide(key(0x19, false, true), false), Verdict::Swallow);
        f.set_blocking(false);
        assert_eq!(f.decide(key(0x19, false, false), false), Verdict::Pass);
        f.set_blocking(true);
        // The press swallowed before the switch was forgotten.
        assert_eq!(f.decide(key(0x19, false, false), false), Verdict::Pass);
    }

    #[test]
    fn draining_finishes_swallowed_keystrokes_only() {
        let mut f = PhysicalFilter::new(true);
        // The emergency chord is held: Ctrl, Alt, Shift, P all swallowed.
        for sc in [0x1D, 0x38, 0x2A, 0x19] {
            assert_eq!(f.decide(key(sc, false, true), false), Verdict::Swallow);
        }
        assert!(!f.is_drained());
        f.start_draining();
        assert!(!f.is_blocking());
        // Auto-repeat of the held P must not type "ppp" into the focused window.
        assert_eq!(f.decide(key(0x19, false, true), false), Verdict::Swallow);
        // New input already flows again.
        assert_eq!(f.decide(HookEvent::MouseMove, false), Verdict::Pass);
        assert_eq!(f.decide(HookEvent::Wheel, false), Verdict::Pass);
        assert_eq!(f.decide(key(0x1E, false, true), false), Verdict::Pass);
        assert_eq!(f.decide(key(0x1E, false, false), false), Verdict::Pass);
        for sc in [0x1D, 0x38, 0x2A] {
            assert_eq!(f.decide(key(sc, false, false), false), Verdict::Swallow);
        }
        assert!(!f.is_drained());
        assert_eq!(f.decide(key(0x19, false, false), false), Verdict::Swallow);
        assert!(f.is_drained());
        // Draining an idle filter is a no-op; off stays off.
        let mut off = PhysicalFilter::new(false);
        off.start_draining();
        assert_eq!(off.decide(key(0x19, false, true), false), Verdict::Pass);
        assert!(off.is_drained());
    }

    #[test]
    fn every_scancode_and_button_is_tracked_separately() {
        let mut f = PhysicalFilter::new(true);
        for sc in 0..=u8::MAX {
            for ext in [false, true] {
                assert_eq!(f.decide(key(sc, ext, true), false), Verdict::Swallow);
            }
        }
        for sc in 0..=u8::MAX {
            for ext in [false, true] {
                assert_eq!(f.decide(key(sc, ext, false), false), Verdict::Swallow);
                assert_eq!(f.decide(key(sc, ext, false), false), Verdict::Pass);
            }
        }
        for b in [
            HookButton::Left,
            HookButton::Right,
            HookButton::Middle,
            HookButton::X1,
            HookButton::X2,
        ] {
            let ev = |down| HookEvent::MouseButton { button: b, down };
            assert_eq!(f.decide(ev(true), false), Verdict::Swallow);
            assert_eq!(f.decide(ev(false), false), Verdict::Swallow);
            assert_eq!(f.decide(ev(false), false), Verdict::Pass);
        }
    }
}
