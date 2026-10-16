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

    /// Like [`last_physical_input`](Self::last_physical_input) for the keyboard only. Hosts use
    /// it so that a local mouse does not stop remote typing. Defaults to any input.
    fn last_physical_keyboard(&self) -> Option<Instant> {
        self.last_physical_input()
    }

    /// Like [`last_physical_input`](Self::last_physical_input) for the mouse only. Defaults to
    /// any input.
    fn last_physical_pointer(&self) -> Option<Instant> {
        self.last_physical_input()
    }

    /// Whether physical local input happened within `within` before now.
    fn is_local_active(&self, within: Duration) -> bool {
        self.last_physical_input()
            .is_some_and(|t| Instant::now().saturating_duration_since(t) <= within)
    }
}

/// One lock-free "last seen" timestamp.
#[derive(Debug, Default)]
struct Stamp(AtomicU64);

impl Stamp {
    fn record(&self, base: Instant, now: Instant) {
        let nanos = now.saturating_duration_since(base).as_nanos();
        let v = u64::try_from(nanos).unwrap_or(u64::MAX - 1) + 1;
        self.0.fetch_max(v, Ordering::Relaxed);
    }

    fn get(&self, base: Instant) -> Option<Instant> {
        match self.0.load(Ordering::Relaxed) {
            0 => None,
            v => Some(base + Duration::from_nanos(v - 1)),
        }
    }
}

/// Lock-free timestamps of the last physical keyboard and pointer input.
#[derive(Debug)]
pub struct ActivityStamp {
    base: Instant,
    keyboard: Stamp,
    pointer: Stamp,
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
            keyboard: Stamp::default(),
            pointer: Stamp::default(),
        }
    }

    /// Records one input event of unknown kind (counts as both keyboard and pointer).
    /// Injected events (`LLMHF_INJECTED` / `LLKHF_INJECTED` and their lower-integrity
    /// variants) are ignored: they are our own `SendInput` calls or another program's, not
    /// the person at the keyboard.
    pub fn record(&self, injected: bool, now: Instant) {
        self.record_keyboard(injected, now);
        self.record_pointer(injected, now);
    }

    /// Records a keyboard event; injected ones are ignored.
    pub fn record_keyboard(&self, injected: bool, now: Instant) {
        if !injected {
            self.keyboard.record(self.base, now);
        }
    }

    /// Records a mouse event; injected ones are ignored.
    pub fn record_pointer(&self, injected: bool, now: Instant) {
        if !injected {
            self.pointer.record(self.base, now);
        }
    }

    /// Latest physical input of either kind.
    pub fn last(&self) -> Option<Instant> {
        self.keyboard
            .get(self.base)
            .max(self.pointer.get(self.base))
    }
}

/// Whether a hook event counts as the host user's own activity: physical (not injected), and
/// not swallowed by privacy mode. While privacy mode blocks the local keyboard and mouse,
/// local input priority is suspended: the blocked events reach nobody, so they must not take
/// control away from the viewer either.
pub(crate) fn counts_as_local(injected: bool) -> bool {
    !injected && !crate::privacy::local_input_blocked()
}

impl LocalActivity for ActivityStamp {
    fn last_physical_input(&self) -> Option<Instant> {
        self.last()
    }

    fn last_physical_keyboard(&self) -> Option<Instant> {
        self.keyboard.get(self.base)
    }

    fn last_physical_pointer(&self) -> Option<Instant> {
        self.pointer.get(self.base)
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
    fn privacy_blocking_suspends_local_priority() {
        let _serial = crate::privacy::TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(counts_as_local(false));
        assert!(!counts_as_local(true));
        let guard = crate::privacy::BlockingGuard::new();
        assert!(
            !counts_as_local(false),
            "swallowed input must not take over"
        );
        assert!(!counts_as_local(true));
        drop(guard);
        assert!(
            counts_as_local(false),
            "priority resumes when privacy mode ends"
        );
    }

    #[test]
    fn keyboard_and_pointer_are_tracked_apart() {
        let s = ActivityStamp::new();
        let t = Instant::now() + Duration::from_millis(5);
        s.record_pointer(false, t);
        assert_eq!(s.last_physical_pointer(), Some(t));
        assert_eq!(s.last_physical_keyboard(), None);
        s.record_keyboard(true, t + Duration::from_secs(1));
        assert_eq!(
            s.last_physical_keyboard(),
            None,
            "injected keys never count"
        );
        s.record_keyboard(false, t + Duration::from_secs(2));
        assert_eq!(s.last_physical_input(), Some(t + Duration::from_secs(2)));
        assert_eq!(s.last_physical_pointer(), Some(t));
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
