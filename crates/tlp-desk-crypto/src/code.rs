use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use rand::rngs::OsRng;
use rand::Rng;

/// After this many failed attempts against the same code, the code rotates anyway, so an
/// attacker spreading guesses across many IPs (to dodge the per-IP rate limit) gets at most
/// this many guesses per code.
pub const MAX_FAILURES_PER_CODE: u32 = 3;

/// The host's one-time 6-digit access code.
///
/// Generated from the OS CSPRNG (uniform over `000000..=999999`). It rotates after every
/// successful use and after [`MAX_FAILURES_PER_CODE`] failures. Cheap to clone; clones share
/// state, so the GUI and the network layer can hold the same code.
#[derive(Clone)]
pub struct OneTimeCode {
    inner: Arc<Mutex<State>>,
}

struct State {
    code: String,
    failures: u32,
    generation: u64,
}

impl fmt::Debug for OneTimeCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OneTimeCode").finish_non_exhaustive()
    }
}

impl Default for OneTimeCode {
    fn default() -> Self {
        Self::new()
    }
}

fn fresh() -> String {
    format!("{:06}", OsRng.gen_range(0..1_000_000u32))
}

impl OneTimeCode {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(State {
                code: fresh(),
                failures: 0,
                generation: 0,
            })),
        }
    }

    /// Fixed code, for tests only.
    #[doc(hidden)]
    pub fn from_fixed(code: &str) -> Self {
        let c = Self::new();
        c.lock().code = code.to_owned();
        c
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The current code, 6 ASCII digits (e.g. `"042917"`).
    pub fn current(&self) -> String {
        self.lock().code.clone()
    }

    /// Display form, e.g. `"042 917"`.
    pub fn display(&self) -> String {
        let c = self.current();
        format!("{} {}", &c[..3], &c[3..])
    }

    /// Increments every time the code changes, so a GUI can cheaply notice rotation.
    pub fn generation(&self) -> u64 {
        self.lock().generation
    }

    /// Replaces the code with a new random one.
    pub fn rotate(&self) {
        let mut s = self.lock();
        s.code = fresh();
        s.failures = 0;
        s.generation += 1;
    }

    /// Call after the code was used successfully: rotates it.
    pub fn record_success(&self) {
        self.rotate();
    }

    /// Call after a wrong guess; rotates after [`MAX_FAILURES_PER_CODE`] failures.
    pub fn record_failure(&self) {
        let rotate = {
            let mut s = self.lock();
            s.failures += 1;
            s.failures >= MAX_FAILURES_PER_CODE
        };
        if rotate {
            self.rotate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_is_six_digits() {
        for _ in 0..200 {
            let c = OneTimeCode::new();
            let s = c.current();
            assert_eq!(s.len(), 6);
            assert!(s.chars().all(|ch| ch.is_ascii_digit()));
            assert_eq!(c.display().len(), 7);
        }
    }

    #[test]
    fn code_rotates_after_use() {
        let c = OneTimeCode::from_fixed("123456");
        let shared = c.clone();
        c.record_success();
        assert_ne!(shared.current(), "123456");
        assert_eq!(shared.generation(), 1);
    }

    #[test]
    fn code_rotates_after_max_failures() {
        let c = OneTimeCode::from_fixed("123456");
        for _ in 0..MAX_FAILURES_PER_CODE - 1 {
            c.record_failure();
            assert_eq!(c.current(), "123456");
        }
        c.record_failure();
        assert_ne!(c.current(), "123456");
    }
}
