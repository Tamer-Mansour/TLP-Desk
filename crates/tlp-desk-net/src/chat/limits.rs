//! Chat limits: message length cap, anti-spam token buckets, history size.

use std::time::{Duration, Instant};

/// Bounds that keep a chat usable and a hostile peer harmless.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatLimits {
    /// Longest message, in Unicode scalar values (`char`s), we send or accept.
    pub max_chars: usize,
    /// Messages the local user may send back to back before the rate limit applies.
    pub burst: u32,
    /// One more message becomes available this often (sustained rate). Zero disables the
    /// rate limit.
    pub refill: Duration,
    /// Burst tolerated from the peer before its messages are dropped. Larger than
    /// [`ChatLimits::burst`] so network jitter never drops a well-behaved peer's messages.
    pub peer_burst: u32,
    /// Messages kept in memory per session (oldest dropped first).
    pub max_history: usize,
}

impl Default for ChatLimits {
    /// 4000 characters, 8 messages at once then one every 750 ms (80 per minute), 16 at once
    /// from the peer, 500 messages of history.
    fn default() -> Self {
        Self {
            max_chars: 4000,
            burst: 8,
            refill: Duration::from_millis(750),
            peer_burst: 16,
            max_history: 500,
        }
    }
}

/// Classic token bucket: `capacity` tokens, one more every `refill`.
#[derive(Debug, Clone)]
pub(crate) struct TokenBucket {
    capacity: u32,
    refill: Duration,
    tokens: u32,
    /// When the last token was added (or the bucket was full).
    last: Instant,
}

impl TokenBucket {
    pub(crate) fn new(capacity: u32, refill: Duration, now: Instant) -> Self {
        Self {
            capacity,
            refill,
            tokens: capacity,
            last: now,
        }
    }

    /// Takes one token, or says how long until one is available.
    pub(crate) fn try_take(&mut self, now: Instant) -> Result<(), Duration> {
        if self.refill.is_zero() {
            return Ok(());
        }
        self.top_up(now);
        if self.tokens > 0 {
            self.tokens -= 1;
            return Ok(());
        }
        let since = now.saturating_duration_since(self.last);
        Err(self
            .refill
            .saturating_sub(since)
            .max(Duration::from_millis(1)))
    }

    fn top_up(&mut self, now: Instant) {
        if self.tokens >= self.capacity {
            self.last = now;
            return;
        }
        let elapsed = now.saturating_duration_since(self.last).as_nanos();
        let step = self.refill.as_nanos().max(1);
        let earned = elapsed / step;
        if earned == 0 {
            return;
        }
        let missing = u128::from(self.capacity - self.tokens);
        if earned >= missing {
            self.tokens = self.capacity;
            self.last = now;
        } else {
            // `earned < missing <= u32::MAX`, so both conversions are exact.
            self.tokens += u32::try_from(earned).unwrap_or(0);
            let advance = u64::try_from(earned.saturating_mul(step)).unwrap_or(u64::MAX);
            self.last += Duration::from_nanos(advance);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_then_refill() {
        let t0 = Instant::now();
        let refill = Duration::from_millis(100);
        let mut b = TokenBucket::new(3, refill, t0);
        for _ in 0..3 {
            assert!(b.try_take(t0).is_ok());
        }
        let wait = b.try_take(t0).unwrap_err();
        assert_eq!(wait, refill);
        // Half a period later: still empty, half the wait left.
        assert_eq!(
            b.try_take(t0 + Duration::from_millis(50)).unwrap_err(),
            Duration::from_millis(50)
        );
        assert!(b.try_take(t0 + refill).is_ok());
        assert!(b.try_take(t0 + refill).is_err());
        // A long pause refills to capacity, never beyond.
        let later = t0 + Duration::from_secs(60);
        for _ in 0..3 {
            assert!(b.try_take(later).is_ok());
        }
        assert!(b.try_take(later).is_err());
    }

    #[test]
    fn zero_refill_means_unlimited_and_zero_capacity_never_allows() {
        let t0 = Instant::now();
        let mut free = TokenBucket::new(0, Duration::ZERO, t0);
        for _ in 0..1000 {
            assert!(free.try_take(t0).is_ok());
        }
        let mut closed = TokenBucket::new(0, Duration::from_millis(10), t0);
        assert!(closed.try_take(t0 + Duration::from_secs(5)).is_err());
    }

    #[test]
    fn clock_going_backwards_does_not_panic() {
        let t0 = Instant::now() + Duration::from_secs(10);
        let mut b = TokenBucket::new(1, Duration::from_millis(10), t0);
        assert!(b.try_take(t0).is_ok());
        assert!(b.try_take(t0 - Duration::from_secs(5)).is_err());
    }
}
