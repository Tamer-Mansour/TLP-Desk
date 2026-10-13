use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Limits for [`RateLimiter`]. Defaults follow the security model: 5 attempts per minute per
/// peer, then a 10-minute lockout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitConfig {
    pub max_attempts: usize,
    pub window: Duration,
    pub lockout: Duration,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            window: Duration::from_secs(60),
            lockout: Duration::from_secs(600),
        }
    }
}

/// Returned when a peer is locked out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("too many attempts; retry in {retry_after:?}")]
pub struct RateLimited {
    pub retry_after: Duration,
}

#[derive(Debug, Default)]
struct Entry {
    attempts: VecDeque<Instant>,
    locked_until: Option<Instant>,
}

/// Per-peer authentication attempt limiter (sliding window + lockout).
///
/// Pure logic: the caller passes `now`, so tests can drive time. Every credential attempt
/// (right or wrong) must call [`RateLimiter::attempt`] first; a successful authentication then
/// calls [`RateLimiter::record_success`] to clear the peer's history.
#[derive(Debug)]
pub struct RateLimiter<K = IpAddr> {
    cfg: RateLimitConfig,
    entries: HashMap<K, Entry>,
}

impl<K: Hash + Eq + Clone> Default for RateLimiter<K> {
    fn default() -> Self {
        Self::new(RateLimitConfig::default())
    }
}

impl<K: Hash + Eq + Clone> RateLimiter<K> {
    pub fn new(cfg: RateLimitConfig) -> Self {
        Self {
            cfg,
            entries: HashMap::new(),
        }
    }

    pub fn config(&self) -> RateLimitConfig {
        self.cfg
    }

    /// Registers an attempt by `key` at `now`. `Err` means the attempt must be refused without
    /// checking the credential. The attempt that exceeds `max_attempts` in `window` starts the
    /// lockout.
    pub fn attempt(&mut self, key: &K, now: Instant) -> Result<(), RateLimited> {
        if self.entries.len() > 10_000 {
            self.prune(now);
        }
        let cfg = self.cfg;
        let e = self.entries.entry(key.clone()).or_default();
        if let Some(until) = e.locked_until {
            if now < until {
                return Err(RateLimited {
                    retry_after: until - now,
                });
            }
            e.locked_until = None;
            e.attempts.clear();
        }
        while let Some(&t) = e.attempts.front() {
            if now.saturating_duration_since(t) >= cfg.window {
                e.attempts.pop_front();
            } else {
                break;
            }
        }
        if e.attempts.len() >= cfg.max_attempts {
            e.locked_until = Some(now + cfg.lockout);
            e.attempts.clear();
            return Err(RateLimited {
                retry_after: cfg.lockout,
            });
        }
        e.attempts.push_back(now);
        Ok(())
    }

    /// Remaining lockout for `key`, if any (does not register an attempt).
    pub fn locked_for(&self, key: &K, now: Instant) -> Option<Duration> {
        self.entries
            .get(key)
            .and_then(|e| e.locked_until)
            .filter(|&u| now < u)
            .map(|u| u - now)
    }

    /// Clears the history of `key` after a successful authentication.
    pub fn record_success(&mut self, key: &K) {
        self.entries.remove(key);
    }

    fn prune(&mut self, now: Instant) {
        let cfg = self.cfg;
        self.entries.retain(|_, e| {
            e.locked_until.is_some_and(|u| now < u)
                || e.attempts
                    .back()
                    .is_some_and(|&t| now.saturating_duration_since(t) < cfg.window)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(n: u8) -> IpAddr {
        IpAddr::from([192, 168, 1, n])
    }

    #[test]
    fn rate_limiter_locks_out_after_five_attempts_per_minute() {
        let mut rl = RateLimiter::default();
        let t0 = Instant::now();
        for i in 0..5 {
            assert!(rl.attempt(&ip(1), t0 + Duration::from_secs(i)).is_ok());
        }
        // 6th within the minute -> lockout
        let err = rl
            .attempt(&ip(1), t0 + Duration::from_secs(10))
            .unwrap_err();
        assert_eq!(err.retry_after, Duration::from_secs(600));
        // other peers unaffected
        assert!(rl.attempt(&ip(2), t0 + Duration::from_secs(10)).is_ok());
        // still locked 9 minutes later
        let t9 = t0 + Duration::from_secs(10 + 540);
        let err = rl.attempt(&ip(1), t9).unwrap_err();
        assert_eq!(err.retry_after, Duration::from_secs(60));
        assert_eq!(rl.locked_for(&ip(1), t9), Some(Duration::from_secs(60)));
        // after 10 minutes the lockout ends and the counter is fresh
        let t10 = t0 + Duration::from_secs(10 + 600);
        assert_eq!(rl.locked_for(&ip(1), t10), None);
        for i in 0..5 {
            assert!(rl.attempt(&ip(1), t10 + Duration::from_secs(i)).is_ok());
        }
        assert!(rl.attempt(&ip(1), t10 + Duration::from_secs(6)).is_err());
    }

    #[test]
    fn rate_limiter_window_slides() {
        let mut rl = RateLimiter::default();
        let t0 = Instant::now();
        // 5 attempts per minute forever is fine
        for i in 0..30u64 {
            assert!(
                rl.attempt(&ip(1), t0 + Duration::from_secs(i * 13)).is_ok(),
                "attempt {i}"
            );
        }
    }

    #[test]
    fn rate_limiter_success_resets() {
        let mut rl = RateLimiter::default();
        let t0 = Instant::now();
        for _ in 0..5 {
            rl.attempt(&ip(1), t0).unwrap();
        }
        rl.record_success(&ip(1));
        assert!(rl.attempt(&ip(1), t0).is_ok());
    }
}
