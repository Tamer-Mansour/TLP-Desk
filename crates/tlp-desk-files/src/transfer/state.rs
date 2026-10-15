//! Shared per-transfer state used by the sender, the receiver and the session loop.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use tokio::sync::watch;

use super::receiver::Receiving;
use super::sender::Sending;
use super::types::{TransferId, TransferInfo, TransferSnapshot, TransferState};

/// Locks a mutex, recovering from poisoning (a panicked holder must not take the transfer
/// engine down with it).
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Which half of the engine runs this transfer.
pub enum Side {
    /// We read files and send them.
    Send(Arc<Sending>),
    /// We receive and write files.
    Recv(Arc<Receiving>),
}

/// One transfer in one session.
pub struct Transfer {
    /// Local handle.
    pub id: TransferId,
    /// Id on the wire (offer id, `FileData` stream id).
    pub wire: u64,
    /// Static description.
    pub info: TransferInfo,
    /// Set to `true` to stop every task of the transfer.
    pub stop: watch::Sender<bool>,
    state: Mutex<TransferState>,
    /// Bytes sent / written, including resumed and identical files.
    pub bytes_done: AtomicU64,
    /// Bytes already at the receiver when the transfer started (not sent again).
    pub resumed: AtomicU64,
    /// Files finished.
    pub files_done: AtomicU32,
    /// Files failed.
    pub failed: AtomicU32,
    /// Smoothed throughput (bytes per second), updated by the session ticker.
    pub rate: AtomicU64,
    meter: Mutex<(Instant, u64)>,
    /// Receiver: the destination folder.
    pub local_dest: Option<PathBuf>,
    /// Sender or receiver half.
    pub side: Side,
}

impl Transfer {
    /// A new transfer in [`TransferState::Preparing`].
    pub fn new(
        id: TransferId,
        wire: u64,
        info: TransferInfo,
        local_dest: Option<PathBuf>,
        side: Side,
    ) -> Self {
        let (stop, _) = watch::channel(false);
        Self {
            id,
            wire,
            info,
            stop,
            state: Mutex::new(TransferState::Preparing),
            bytes_done: AtomicU64::new(0),
            resumed: AtomicU64::new(0),
            files_done: AtomicU32::new(0),
            failed: AtomicU32::new(0),
            rate: AtomicU64::new(0),
            meter: Mutex::new((Instant::now(), 0)),
            local_dest,
            side,
        }
    }

    /// Current state.
    pub fn state(&self) -> TransferState {
        lock(&self.state).clone()
    }

    /// Moves to `Running` unless already final.
    pub fn set_running(&self) {
        let mut s = lock(&self.state);
        if !s.is_final() {
            *s = TransferState::Running;
        }
    }

    /// Moves to a final state; `false` if it already was final (only the first stop counts).
    pub fn finish(&self, state: TransferState) -> bool {
        let mut s = lock(&self.state);
        if s.is_final() {
            return false;
        }
        *s = state;
        drop(s);
        self.stop.send_replace(true);
        true
    }

    /// Whether the transfer was told to stop.
    pub fn stopped(&self) -> bool {
        *self.stop.borrow()
    }

    /// Resolves when the transfer is told to stop.
    pub async fn wait_stop(&self) {
        let mut rx = self.stop.subscribe();
        let _ = rx.wait_for(|s| *s).await;
    }

    /// Updates the throughput estimate; returns the new rate.
    pub fn tick_rate(&self) -> u64 {
        let now = Instant::now();
        let bytes = self.bytes_done.load(Ordering::Relaxed);
        let mut m = lock(&self.meter);
        let dt = now.duration_since(m.0).as_secs_f64();
        if dt < 0.05 {
            return self.rate.load(Ordering::Relaxed);
        }
        let inst = bytes.saturating_sub(m.1) as f64 / dt;
        *m = (now, bytes);
        let old = self.rate.load(Ordering::Relaxed) as f64;
        // Exponential smoothing: steady numbers in the UI, still reacts within ~1 s.
        let new = if old == 0.0 {
            inst
        } else {
            old * 0.6 + inst * 0.4
        };
        let new = new as u64;
        self.rate.store(new, Ordering::Relaxed);
        new
    }

    /// Point-in-time view for the UI.
    pub fn snapshot(&self) -> TransferSnapshot {
        TransferSnapshot {
            info: self.info.clone(),
            state: self.state(),
            bytes_done: self.bytes_done.load(Ordering::Relaxed),
            resumed_bytes: self.resumed.load(Ordering::Relaxed),
            files_done: self.files_done.load(Ordering::Relaxed),
            failed_items: self.failed.load(Ordering::Relaxed),
            bytes_per_sec: self.rate.load(Ordering::Relaxed),
            local_dest: self.local_dest.clone(),
        }
    }
}

/// Sorted, disjoint byte ranges `[start, end)` (adjacent ranges are merged).
#[derive(Debug, Default, Clone)]
pub struct RangeSet(Vec<(u64, u64)>);

impl RangeSet {
    /// Adds `[start, end)`; `false` (and nothing added) if it overlaps an existing range.
    pub fn insert(&mut self, start: u64, end: u64) -> bool {
        if start >= end {
            return true;
        }
        let i = self.0.partition_point(|r| r.1 <= start);
        if let Some(r) = self.0.get(i) {
            if r.0 < end {
                return false;
            }
        }
        self.0.insert(i, (start, end));
        // Merge with neighbours that touch.
        if i + 1 < self.0.len() && self.0[i].1 == self.0[i + 1].0 {
            self.0[i].1 = self.0[i + 1].1;
            self.0.remove(i + 1);
        }
        if i > 0 && self.0[i - 1].1 == self.0[i].0 {
            self.0[i - 1].1 = self.0[i].1;
            self.0.remove(i);
        }
        true
    }

    /// Total bytes covered.
    pub fn total(&self) -> u64 {
        self.0.iter().map(|r| r.1 - r.0).sum()
    }

    /// End of the contiguous run starting at `from` (or `from` if nothing starts there).
    pub fn contiguous_end(&self, from: u64) -> u64 {
        match self.0.first() {
            Some(&(s, e)) if s <= from && e > from => e,
            _ => from,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_set_merges_and_detects_overlap() {
        let mut r = RangeSet::default();
        assert!(r.insert(10, 20));
        assert!(r.insert(30, 40));
        assert!(r.insert(20, 30));
        assert_eq!(r.0, vec![(10, 40)]);
        assert!(!r.insert(35, 45));
        assert!(!r.insert(0, 11));
        assert!(r.insert(0, 10));
        assert_eq!(r.total(), 40);
        assert_eq!(r.contiguous_end(0), 40);
        assert!(r.insert(50, 60));
        assert_eq!(r.contiguous_end(0), 40);
        assert_eq!(RangeSet::default().contiguous_end(7), 7);
    }
}
