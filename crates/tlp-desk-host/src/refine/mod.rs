//! Static refinement: decides *when* to send the one high-quality refresh picture.
//!
//! While the screen moves, rate control spends few bits per picture and text turns soft. Once
//! the content has stopped changing for [`SETTLE`], the encode thread re-encodes the last frame
//! once through `VideoEncoder::encode_refine` (a keyframe at a very low quantiser), so text is
//! crisp right after scrolling or typing stops. [`RefineGate`] is the pure timing policy
//! (testable without a clock): refine only if the content changed since the last refine, wait
//! for the settle time, keep a minimum gap between refreshes and back off when the viewer is
//! behind (a refresh keyframe is big).

use std::time::{Duration, Instant};

/// How long the content must be unchanged before it is refined.
pub(crate) const SETTLE: Duration = Duration::from_millis(300);
/// Minimum time between two refreshes, so a flickering screen cannot cause a keyframe storm.
pub(crate) const MIN_GAP: Duration = Duration::from_millis(1_500);
/// After a dropped video packet (viewer or network behind), refreshes pause for this long.
pub(crate) const BACKOFF: Duration = Duration::from_secs(3);

/// Timing state of the refinement policy.
#[derive(Debug, Default)]
pub(crate) struct RefineGate {
    /// When the screen content last changed; `None` = already refined (or nothing yet).
    changed_at: Option<Instant>,
    last_refine: Option<Instant>,
    backoff_until: Option<Instant>,
}

impl RefineGate {
    /// The screen content changed (a new captured frame; cursor-only moves do not count).
    pub(crate) fn note_change(&mut self, now: Instant) {
        self.changed_at = Some(now);
    }

    /// A video packet was dropped because the viewer is behind: hold refreshes back.
    pub(crate) fn note_congestion(&mut self, now: Instant) {
        self.backoff_until = Some(now + BACKOFF);
    }

    /// A refresh was sent (or deliberately skipped): nothing more to do until the next change.
    pub(crate) fn note_refined(&mut self, now: Instant) {
        self.changed_at = None;
        self.last_refine = Some(now);
    }

    /// The instant at which the next refresh becomes due, `None` when there is nothing to
    /// refine. Callers wake up at this time (if no real change arrives first).
    pub(crate) fn deadline(&self) -> Option<Instant> {
        let changed = self.changed_at?;
        let mut at = changed + SETTLE;
        if let Some(last) = self.last_refine {
            at = at.max(last + MIN_GAP);
        }
        if let Some(until) = self.backoff_until {
            at = at.max(until);
        }
        Some(at)
    }

    /// Whether a refresh is due at `now`.
    pub(crate) fn is_due(&self, now: Instant) -> bool {
        self.deadline().is_some_and(|at| now >= at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn nothing_changed_means_nothing_to_refine() {
        let g = RefineGate::default();
        assert_eq!(g.deadline(), None);
        assert!(!g.is_due(Instant::now() + Duration::from_secs(60)));
    }

    #[test]
    fn refines_after_the_settle_time_and_only_once() {
        let t0 = Instant::now();
        let mut g = RefineGate::default();
        g.note_change(t0);
        assert!(!g.is_due(t0 + ms(299)));
        assert!(g.is_due(t0 + SETTLE));
        g.note_refined(t0 + SETTLE);
        assert!(
            !g.is_due(t0 + ms(10_000)),
            "static screen: no second refresh"
        );
    }

    #[test]
    fn continued_motion_keeps_postponing() {
        let t0 = Instant::now();
        let mut g = RefineGate::default();
        for i in 0..20u64 {
            g.note_change(t0 + ms(i * 16));
            assert!(!g.is_due(t0 + ms(i * 16 + 200)));
        }
        assert!(g.is_due(t0 + ms(19 * 16) + SETTLE));
    }

    #[test]
    fn refreshes_keep_a_minimum_gap() {
        let t0 = Instant::now();
        let mut g = RefineGate::default();
        g.note_change(t0);
        g.note_refined(t0 + SETTLE);
        // A blinking caret changes the screen again right away.
        g.note_change(t0 + ms(400));
        assert!(!g.is_due(t0 + ms(800)));
        assert!(g.is_due(t0 + SETTLE + MIN_GAP));
    }

    #[test]
    fn congestion_pauses_refreshes() {
        let t0 = Instant::now();
        let mut g = RefineGate::default();
        g.note_change(t0);
        g.note_congestion(t0 + ms(100));
        assert!(!g.is_due(t0 + ms(1_000)));
        assert!(g.is_due(t0 + ms(100) + BACKOFF));
    }
}
