//! What a soak/leak test measures.

use std::time::Duration;

/// Resident (physical) memory of this process in bytes, if the OS reports it.
pub fn resident_bytes() -> Option<usize> {
    memory_stats::memory_stats().map(|m| m.physical_mem)
}

/// Tasks alive on the current tokio runtime (0 outside a runtime).
pub fn alive_tasks() -> usize {
    tokio::runtime::Handle::try_current()
        .map(|h| h.metrics().num_alive_tasks())
        .unwrap_or(0)
}

/// Bytes as `"123.4 MiB"` for test output.
pub fn mib(bytes: usize) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

/// One sample of a soak run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Time since the soak started.
    pub at: Duration,
    /// [`resident_bytes`] (0 if unknown).
    pub resident: usize,
    /// [`alive_tasks`].
    pub tasks: usize,
}

impl Sample {
    /// Measures now.
    pub fn now(at: Duration) -> Self {
        Self {
            at,
            resident: resident_bytes().unwrap_or(0),
            tasks: alive_tasks(),
        }
    }
}

/// Growth of resident memory between the median of the first and the last third of
/// `samples` (medians ignore single spikes from allocator behaviour). `None` with fewer than
/// three samples or without memory figures.
pub fn memory_growth(samples: &[Sample]) -> Option<i64> {
    if samples.len() < 3 || samples.iter().any(|s| s.resident == 0) {
        return None;
    }
    let third = samples.len() / 3;
    let median = |part: &[Sample]| {
        let mut v: Vec<usize> = part.iter().map(|s| s.resident).collect();
        v.sort_unstable();
        v.get(v.len() / 2).copied().unwrap_or(0)
    };
    let first = median(samples.get(..third.max(1))?);
    let last = median(samples.get(samples.len() - third.max(1)..)?);
    Some(i64::try_from(last).ok()? - i64::try_from(first).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(resident: usize) -> Sample {
        Sample {
            at: Duration::ZERO,
            resident,
            tasks: 0,
        }
    }

    #[test]
    fn growth_uses_medians() {
        let flat = [
            s(100),
            s(500),
            s(100),
            s(100),
            s(100),
            s(100),
            s(100),
            s(100),
            s(100),
        ];
        assert_eq!(memory_growth(&flat), Some(0));
        let rising = [s(100), s(110), s(200), s(210), s(300), s(310)];
        assert_eq!(memory_growth(&rising), Some(200));
        assert_eq!(memory_growth(&[s(1), s(2)]), None);
        assert!(resident_bytes().is_some_and(|b| b > 0));
    }
}
