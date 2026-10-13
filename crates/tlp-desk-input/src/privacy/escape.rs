//! The local emergency exit: holding **Ctrl + Alt + Shift + P** on the host's physical
//! keyboard for [`EMERGENCY_HOLD`] ends privacy mode, whatever the viewer does.
//!
//! The privacy hook still *sees* the physical keys it swallows, so it can detect the chord
//! even though no application receives it. Keys are matched by scancode (the key's position),
//! so the chord works on every keyboard layout, Arabic included. Either Ctrl, Alt or Shift key
//! counts. Injected keys (the viewer's) are never fed in, so a remote viewer cannot trigger or
//! block the exit.
//!
//! Ctrl+Alt+Del is the second, OS-level exit: no hook can intercept it, and the secure desktop
//! it opens makes the shield turn itself off (see the crate's privacy module docs).

use std::time::{Duration, Instant};

/// How long the chord must be held.
pub const EMERGENCY_HOLD: Duration = Duration::from_secs(3);

/// Set-1 scancode of the `P` key (same position on every layout).
const SC_P: u8 = 0x19;
const SC_CTRL: u8 = 0x1D;
const SC_ALT: u8 = 0x38;
const SC_LSHIFT: u8 = 0x2A;
const SC_RSHIFT: u8 = 0x36;

/// Physical key state for the chord, fed from the keyboard hook.
#[derive(Debug, Clone, Default)]
pub struct EmergencyExit {
    /// Bit 0 = left, bit 1 = right.
    ctrl: u8,
    alt: u8,
    shift: u8,
    p: bool,
    /// When the full chord started being held.
    since: Option<Instant>,
    /// Already triggered for the current hold (fires once until released).
    fired: bool,
}

impl EmergencyExit {
    /// No keys held.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one **physical** key event (`scancode` = low byte, `extended` = `E0` prefix) at
    /// `now`. Returns `true` exactly once per hold, when the chord has been held long enough.
    pub fn key(&mut self, scancode: u8, extended: bool, down: bool, now: Instant) -> bool {
        let side = if extended { 2 } else { 1 };
        let set = |bits: &mut u8, bit: u8| {
            if down {
                *bits |= bit;
            } else {
                *bits &= !bit;
            }
        };
        match (scancode, extended) {
            (SC_CTRL, _) => set(&mut self.ctrl, side),
            (SC_ALT, _) => set(&mut self.alt, side),
            (SC_LSHIFT, false) => set(&mut self.shift, 1),
            (SC_RSHIFT, false) => set(&mut self.shift, 2),
            (SC_P, false) => self.p = down,
            _ => {}
        }
        if self.chord_held() {
            if self.since.is_none() {
                self.since = Some(now);
            }
        } else {
            self.since = None;
            self.fired = false;
        }
        self.poll(now)
    }

    /// Re-checks the hold time without a key event (the hook thread calls this from a timer,
    /// because a held key does not auto-repeat on every keyboard). Returns `true` once per
    /// hold.
    pub fn poll(&mut self, now: Instant) -> bool {
        match self.since {
            Some(t) if !self.fired && now.saturating_duration_since(t) >= EMERGENCY_HOLD => {
                self.fired = true;
                true
            }
            _ => false,
        }
    }

    /// How long the full chord has been held so far (`None` = not held).
    pub fn held_for(&self, now: Instant) -> Option<Duration> {
        self.since.map(|t| now.saturating_duration_since(t))
    }

    fn chord_held(&self) -> bool {
        self.ctrl != 0 && self.alt != 0 && self.shift != 0 && self.p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Clock(Instant);
    impl Clock {
        fn at(&self, ms: u64) -> Instant {
            self.0 + Duration::from_millis(ms)
        }
    }

    fn press_chord(e: &mut EmergencyExit, t: Instant) -> bool {
        e.key(SC_CTRL, false, true, t)
            | e.key(SC_ALT, false, true, t)
            | e.key(SC_LSHIFT, false, true, t)
            | e.key(SC_P, false, true, t)
    }

    #[test]
    fn fires_once_after_three_seconds() {
        let c = Clock(Instant::now());
        let mut e = EmergencyExit::new();
        assert!(!press_chord(&mut e, c.at(0)));
        assert_eq!(e.held_for(c.at(1000)), Some(Duration::from_secs(1)));
        assert!(!e.poll(c.at(2999)));
        assert!(e.poll(c.at(3000)));
        // Only once per hold, even with auto-repeat key-downs.
        assert!(!e.poll(c.at(4000)));
        assert!(!e.key(SC_P, false, true, c.at(4100)));
    }

    #[test]
    fn auto_repeat_alone_also_fires() {
        let c = Clock(Instant::now());
        let mut e = EmergencyExit::new();
        press_chord(&mut e, c.at(0));
        let mut fired = false;
        for ms in (33..=3300).step_by(33) {
            fired |= e.key(SC_P, false, true, c.at(ms));
        }
        assert!(fired);
    }

    #[test]
    fn releasing_any_key_restarts_the_hold() {
        let c = Clock(Instant::now());
        let mut e = EmergencyExit::new();
        press_chord(&mut e, c.at(0));
        e.key(SC_ALT, false, false, c.at(2000));
        assert_eq!(e.held_for(c.at(2500)), None);
        assert!(!e.poll(c.at(3500)));
        e.key(SC_ALT, true, true, c.at(4000)); // right Alt (AltGr) counts as well
        assert!(!e.poll(c.at(6999)));
        assert!(e.poll(c.at(7000)));
        // After a full release the exit can fire again (privacy mode re-enabled later).
        for sc in [SC_CTRL, SC_ALT, SC_LSHIFT, SC_P] {
            e.key(sc, false, false, c.at(8000));
        }
        e.key(SC_ALT, true, false, c.at(8000));
        press_chord(&mut e, c.at(9000));
        assert!(e.poll(c.at(12_000)));
    }

    #[test]
    fn either_side_of_each_modifier_counts() {
        let c = Clock(Instant::now());
        let mut e = EmergencyExit::new();
        e.key(SC_CTRL, true, true, c.at(0));
        e.key(SC_ALT, true, true, c.at(0));
        e.key(SC_RSHIFT, false, true, c.at(0));
        e.key(SC_P, false, true, c.at(0));
        assert!(e.poll(c.at(3000)));
        // Releasing only the left Ctrl (never pressed) keeps the right one held.
        let mut e = EmergencyExit::new();
        press_chord(&mut e, c.at(0));
        e.key(SC_CTRL, true, true, c.at(10));
        e.key(SC_CTRL, false, false, c.at(20));
        assert!(e.poll(c.at(3000)));
    }

    #[test]
    fn incomplete_or_wrong_chords_never_fire() {
        let c = Clock(Instant::now());
        let mut e = EmergencyExit::new();
        e.key(SC_CTRL, false, true, c.at(0));
        e.key(SC_ALT, false, true, c.at(0));
        e.key(SC_P, false, true, c.at(0));
        assert!(!e.poll(c.at(60_000)), "without Shift");
        // Extended 0x19 is a media key, not P; extended 0x2A is the PrintScreen fake shift.
        let mut e = EmergencyExit::new();
        e.key(SC_CTRL, false, true, c.at(0));
        e.key(SC_ALT, false, true, c.at(0));
        e.key(SC_LSHIFT, true, true, c.at(0));
        e.key(SC_P, true, true, c.at(0));
        assert!(!e.poll(c.at(60_000)));
        // Other keys do not matter.
        let mut e = EmergencyExit::new();
        press_chord(&mut e, c.at(0));
        e.key(0x1E, false, true, c.at(100));
        assert!(e.poll(c.at(3000)));
    }
}
