//! "The person at the host wins": pure decision logic for local input priority and the
//! remote pointer marker. No clock or OS calls inside; the caller passes `now` and the time
//! of the last physical local input, so everything is unit-testable.

use std::time::{Duration, Instant};

use tlp_desk_input::InputEvent;

/// How long after the last remote mouse event the green marker stays visible.
pub(crate) const MARKER_HOLD: Duration = Duration::from_millis(1500);

/// What the input thread must do after a [`PriorityGate`] step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Step {
    /// Release every key/button the remote holds (we just entered local priority).
    pub release_remote: bool,
    /// The priority state changed to this value (tell the viewer).
    pub changed: Option<bool>,
}

#[derive(Debug)]
pub(crate) struct PriorityGate {
    /// Zero disables local priority.
    grace: Duration,
    keyboard_active: bool,
    pointer_active: bool,
    last_remote_pointer: Option<Instant>,
}

/// Time of the last physical local input, per device kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct LocalInput {
    pub keyboard: Option<Instant>,
    pub pointer: Option<Instant>,
}

/// Any input (both kinds at the same time), for callers that do not tell them apart.
impl From<Option<Instant>> for LocalInput {
    fn from(t: Option<Instant>) -> Self {
        Self {
            keyboard: t,
            pointer: t,
        }
    }
}

fn is_pointer(ev: &InputEvent) -> bool {
    matches!(
        ev,
        InputEvent::MouseMove { .. }
            | InputEvent::MouseButton { .. }
            | InputEvent::MouseWheel { .. }
    )
}

impl PriorityGate {
    pub fn new(grace: Duration) -> Self {
        Self {
            grace,
            keyboard_active: false,
            pointer_active: false,
            last_remote_pointer: None,
        }
    }

    #[cfg(test)]
    pub fn local_active(&self) -> bool {
        self.keyboard_active || self.pointer_active
    }

    fn within_grace(&self, now: Instant, t: Option<Instant>) -> bool {
        !self.grace.is_zero() && t.is_some_and(|t| now.saturating_duration_since(t) < self.grace)
    }

    /// Re-evaluates the state at `now` given the last physical local input.
    pub fn update(&mut self, now: Instant, local: impl Into<LocalInput>) -> Step {
        let local = local.into();
        let before = (self.keyboard_active, self.pointer_active);
        let keyboard = self.within_grace(now, local.keyboard);
        let pointer = self.within_grace(now, local.pointer);
        self.keyboard_active = keyboard;
        self.pointer_active = pointer;
        let was_any = before.0 || before.1;
        let any = keyboard || pointer;
        Step {
            // Entering priority for a device kind: whatever the remote holds goes up.
            release_remote: (keyboard && !before.0) || (pointer && !before.1),
            changed: (any != was_any).then_some(any),
        }
    }

    /// Decides whether a remote event may be injected. `ReleaseAll` always passes (it can
    /// only help). The local person wins per device: local typing blocks remote keys, local
    /// mouse use blocks remote pointer events, but moving the mouse never stops remote
    /// typing (a jittery pointer must not make the keyboard look dead). Returns the state
    /// step (evaluated first) and the verdict.
    pub fn filter(
        &mut self,
        now: Instant,
        local: impl Into<LocalInput>,
        ev: &InputEvent,
    ) -> (Step, bool) {
        let step = self.update(now, local);
        let forward = match ev {
            InputEvent::ReleaseAll => true,
            e if is_pointer(e) => !self.pointer_active,
            _ => !self.keyboard_active,
        };
        if forward && is_pointer(ev) {
            self.last_remote_pointer = Some(now);
        }
        (step, forward)
    }

    /// The marker shows while the remote moved the pointer recently and the local user is
    /// not using the mouse.
    pub fn marker_visible(&self, now: Instant) -> bool {
        !self.pointer_active
            && self
                .last_remote_pointer
                .is_some_and(|t| now.saturating_duration_since(t) < MARKER_HOLD)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tlp_desk_input::{ActivityStamp, LocalActivity, MouseButton};

    const GRACE: Duration = Duration::from_millis(1000);

    fn mv() -> InputEvent {
        InputEvent::MouseMove { x: 10, y: 10 }
    }

    fn key() -> InputEvent {
        InputEvent::Key {
            scancode: 0x1e,
            extended: false,
            down: true,
        }
    }

    /// Injectable clock: a base instant plus milliseconds.
    struct Clock(Instant);
    impl Clock {
        fn at(&self, ms: u64) -> Instant {
            self.0 + Duration::from_millis(ms)
        }
    }

    #[test]
    fn remote_passes_without_local_input() {
        let c = Clock(Instant::now());
        let mut g = PriorityGate::new(GRACE);
        for ev in [mv(), key(), InputEvent::MouseWheel { dx: 0, dy: 120 }] {
            let (step, fwd) = g.filter(c.at(0), None, &ev);
            assert!(fwd);
            assert_eq!(step, Step::default());
        }
    }

    #[test]
    fn remote_events_blocked_during_grace() {
        let c = Clock(Instant::now());
        let mut g = PriorityGate::new(GRACE);
        let local = Some(c.at(100));
        for (t, ev) in [
            (100, mv()),
            (400, key()),
            (
                900,
                InputEvent::MouseButton {
                    button: MouseButton::Left,
                    down: true,
                },
            ),
            (1099, InputEvent::MouseWheel { dx: 0, dy: -120 }),
        ] {
            let (_, fwd) = g.filter(c.at(t), local, &ev);
            assert!(!fwd, "event at {t} ms must be dropped");
        }
        assert!(g.local_active());
        // ReleaseAll still passes.
        assert!(g.filter(c.at(500), local, &InputEvent::ReleaseAll).1);
    }

    #[test]
    fn takeover_releases_remote_held_state_once() {
        let c = Clock(Instant::now());
        let mut g = PriorityGate::new(GRACE);
        // Remote holds a key.
        assert!(g.filter(c.at(0), None, &key()).1);
        // Local user moves the mouse: the next tick enters local priority and asks for a
        // release; later ticks do not repeat it.
        let local = Some(c.at(50));
        let step = g.update(c.at(60), local);
        assert_eq!(
            step,
            Step {
                release_remote: true,
                changed: Some(true)
            }
        );
        assert_eq!(g.update(c.at(70), local), Step::default());
        let (step, fwd) = g.filter(c.at(80), local, &key());
        assert_eq!(step, Step::default());
        assert!(!fwd);
    }

    #[test]
    fn remote_resumes_after_grace() {
        let c = Clock(Instant::now());
        let mut g = PriorityGate::new(GRACE);
        let local = Some(c.at(0));
        assert!(!g.filter(c.at(10), local, &mv()).1);
        // Exactly at the end of the grace window the remote is back.
        let (step, fwd) = g.filter(c.at(1000), local, &mv());
        assert!(fwd);
        assert_eq!(
            step,
            Step {
                release_remote: false,
                changed: Some(false)
            }
        );
        // New local input takes over again.
        let (step, fwd) = g.filter(c.at(1500), Some(c.at(1490)), &mv());
        assert!(!fwd);
        assert_eq!(step.changed, Some(true));
    }

    #[test]
    fn injected_events_do_not_count_as_local_input() {
        // The hook feeds an ActivityStamp; our own SendInput arrives flagged as injected.
        let c = Clock(Instant::now());
        let stamp = ActivityStamp::new();
        let mut g = PriorityGate::new(GRACE);
        stamp.record(true, c.at(10));
        assert!(g.filter(c.at(20), stamp.last_physical_input(), &mv()).1);
        stamp.record(false, c.at(30));
        assert!(!g.filter(c.at(40), stamp.last_physical_input(), &mv()).1);
    }

    #[test]
    fn zero_grace_disables_priority() {
        let c = Clock(Instant::now());
        let mut g = PriorityGate::new(Duration::ZERO);
        assert!(g.filter(c.at(0), Some(c.at(0)), &key()).1);
        assert!(!g.local_active());
    }

    #[test]
    fn marker_follows_remote_pointer_and_hides_for_local() {
        let c = Clock(Instant::now());
        let mut g = PriorityGate::new(GRACE);
        assert!(!g.marker_visible(c.at(0)));
        // Keys alone do not show the marker.
        g.filter(c.at(0), None, &key());
        assert!(!g.marker_visible(c.at(1)));
        g.filter(c.at(100), None, &mv());
        assert!(g.marker_visible(c.at(100)));
        assert!(g.marker_visible(c.at(1599)));
        assert!(!g.marker_visible(c.at(1600)));
        // Local takeover hides it immediately.
        g.filter(c.at(2000), None, &mv());
        g.update(c.at(2010), Some(c.at(2005)));
        assert!(!g.marker_visible(c.at(2010)));
        // A dropped remote move does not bring it back.
        g.filter(c.at(2020), Some(c.at(2005)), &mv());
        assert!(!g.marker_visible(c.at(2020)));
    }

    fn at(c: &Clock, ms: u64) -> Option<Instant> {
        Some(c.at(ms))
    }

    /// Regression: a local mouse (or jittery pointer device) used to put the whole session
    /// in local priority and silently drop every remote key.
    #[test]
    fn local_mouse_does_not_block_remote_keys() {
        let c = Clock(Instant::now());
        let mut g = PriorityGate::new(GRACE);
        let local = LocalInput {
            keyboard: None,
            pointer: at(&c, 100),
        };
        assert!(g.filter(c.at(200), local, &key()).1, "key must pass");
        assert!(!g.filter(c.at(210), local, &mv()).1, "pointer is blocked");
        assert!(g.filter(c.at(220), local, &InputEvent::ReleaseAll).1);
    }

    #[test]
    fn local_typing_blocks_remote_keys_but_not_the_remote_pointer() {
        let c = Clock(Instant::now());
        let mut g = PriorityGate::new(GRACE);
        let local = LocalInput {
            keyboard: at(&c, 100),
            pointer: None,
        };
        assert!(!g.filter(c.at(200), local, &key()).1);
        assert!(g.filter(c.at(210), local, &mv()).1);
        assert!(g.local_active());
    }

    #[test]
    fn viewer_is_told_once_about_overall_priority() {
        let c = Clock(Instant::now());
        let mut g = PriorityGate::new(GRACE);
        let kb = LocalInput {
            keyboard: at(&c, 0),
            pointer: None,
        };
        assert_eq!(g.update(c.at(10), kb).changed, Some(true));
        // The mouse joins in: still "active", no second notification, but the remote's
        // buttons are released.
        let both = LocalInput {
            keyboard: at(&c, 0),
            pointer: at(&c, 500),
        };
        let step = g.update(c.at(510), both);
        assert_eq!(step.changed, None);
        assert!(step.release_remote);
        // Everything idle: back to the remote.
        assert_eq!(g.update(c.at(2000), both).changed, Some(false));
    }

    #[test]
    fn injected_keys_never_start_priority() {
        let c = Clock(Instant::now());
        let stamp = ActivityStamp::new();
        let mut g = PriorityGate::new(GRACE);
        stamp.record_keyboard(true, c.at(10));
        stamp.record_pointer(true, c.at(10));
        let local = LocalInput {
            keyboard: stamp.last_physical_keyboard(),
            pointer: stamp.last_physical_pointer(),
        };
        assert!(g.filter(c.at(20), local, &key()).1);
        assert!(g.filter(c.at(20), local, &mv()).1);
        assert!(!g.local_active());
    }
}
