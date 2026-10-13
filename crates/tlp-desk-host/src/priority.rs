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
    local_active: bool,
    last_remote_pointer: Option<Instant>,
}

fn is_remote_control(ev: &InputEvent) -> bool {
    !matches!(ev, InputEvent::ReleaseAll)
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
            local_active: false,
            last_remote_pointer: None,
        }
    }

    #[cfg(test)]
    pub fn local_active(&self) -> bool {
        self.local_active
    }

    /// Re-evaluates the state at `now` given the last physical local input.
    pub fn update(&mut self, now: Instant, last_local: Option<Instant>) -> Step {
        let active = !self.grace.is_zero()
            && last_local.is_some_and(|t| now.saturating_duration_since(t) < self.grace);
        if active == self.local_active {
            return Step::default();
        }
        self.local_active = active;
        Step {
            release_remote: active,
            changed: Some(active),
        }
    }

    /// Decides whether a remote event may be injected. `ReleaseAll` always passes (it can
    /// only help). Returns the state step (evaluated first) and the verdict.
    pub fn filter(
        &mut self,
        now: Instant,
        last_local: Option<Instant>,
        ev: &InputEvent,
    ) -> (Step, bool) {
        let step = self.update(now, last_local);
        let forward = !(self.local_active && is_remote_control(ev));
        if forward && is_pointer(ev) {
            self.last_remote_pointer = Some(now);
        }
        (step, forward)
    }

    /// The marker shows while the remote moved the pointer recently and the local user is
    /// not active.
    pub fn marker_visible(&self, now: Instant) -> bool {
        !self.local_active
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
}
