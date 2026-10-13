//! Host-side "is the local user touching the mouse/keyboard?" bookkeeping.
//!
//! The platform hook ([`LocalActivityMonitor`](crate::LocalActivityMonitor) on Windows) only
//! calls [`ActivityStamp::record`] from its hook procedures: one atomic store, never a lock, so
//! the hook can never stall system input.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Source of the time of the last **physical** local input on the host.
pub trait LocalActivity: Send + Sync {
    /// When the local user last moved/clicked the mouse or pressed a key, if ever. Input
    /// synthesized by software (including our own `SendInput`) never counts.
    fn last_physical_input(&self) -> Option<Instant>;

    /// Whether physical local input happened within `within` before now.
    fn is_local_active(&self, within: Duration) -> bool {
        self.last_physical_input()
            .is_some_and(|t| Instant::now().saturating_duration_since(t) <= within)
    }
}

/// Lock-free timestamp of the last physical input.
#[derive(Debug)]
pub struct ActivityStamp {
    base: Instant,
    /// Nanoseconds since `base` plus one; 0 = never.
    last: AtomicU64,
}

impl Default for ActivityStamp {
    fn default() -> Self {
        Self::new()
    }
}

impl ActivityStamp {
    pub fn new() -> Self {
        Self {
            base: Instant::now(),
            last: AtomicU64::new(0),
        }
    }

    /// Records one input event seen at `now`. Injected events (`LLMHF_INJECTED` /
    /// `LLKHF_INJECTED` and their lower-integrity variants) are ignored: they are our own
    /// `SendInput` calls or another program's, not the person at the keyboard.
    pub fn record(&self, injected: bool, now: Instant) {
        if injected {
            return;
        }
        let nanos = now.saturating_duration_since(self.base).as_nanos();
        let v = u64::try_from(nanos).unwrap_or(u64::MAX - 1) + 1;
        self.last.fetch_max(v, Ordering::Relaxed);
    }

    pub fn last(&self) -> Option<Instant> {
        match self.last.load(Ordering::Relaxed) {
            0 => None,
            v => Some(self.base + Duration::from_nanos(v - 1)),
        }
    }
}

impl LocalActivity for ActivityStamp {
    fn last_physical_input(&self) -> Option<Instant> {
        self.last()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injected_events_do_not_count() {
        let s = ActivityStamp::new();
        let t = Instant::now() + Duration::from_millis(5);
        s.record(true, t);
        assert_eq!(s.last_physical_input(), None);
        assert!(!s.is_local_active(Duration::from_secs(3600)));
        s.record(false, t);
        assert_eq!(s.last_physical_input(), Some(t));
        // A later injected event does not move the stamp.
        s.record(true, t + Duration::from_secs(1));
        assert_eq!(s.last_physical_input(), Some(t));
    }

    #[test]
    fn stamp_only_moves_forward() {
        let s = ActivityStamp::new();
        let t = Instant::now();
        s.record(false, t + Duration::from_millis(10));
        s.record(false, t);
        assert_eq!(s.last(), Some(t + Duration::from_millis(10)));
    }

    #[test]
    fn is_local_active_uses_window() {
        let s = ActivityStamp::new();
        s.record(false, Instant::now());
        assert!(s.is_local_active(Duration::from_secs(5)));
        let old = ActivityStamp::new();
        old.record(false, old.base);
        std::thread::sleep(Duration::from_millis(30));
        assert!(!old.is_local_active(Duration::from_millis(10)));
    }
}
