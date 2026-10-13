//! An in-memory clipboard for tests, the CLI and the app's fake backend. It never touches
//! the user's real clipboard.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use tokio::sync::watch;

use super::{ClipContent, ClipboardBackend};
use crate::FilesError;

/// A process-local clipboard with change notifications, like the OS one.
#[derive(Debug)]
pub struct MemoryClipboard {
    state: Mutex<State>,
    seq: watch::Sender<u64>,
    writes: AtomicUsize,
}

#[derive(Debug, Default)]
struct State {
    content: Option<ClipContent>,
    seq: u64,
}

impl Default for MemoryClipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryClipboard {
    /// An empty clipboard.
    pub fn new() -> Self {
        Self {
            state: Mutex::default(),
            seq: watch::Sender::new(0),
            writes: AtomicUsize::new(0),
        }
    }

    /// A clipboard that already holds `content` (no change notification).
    pub fn with(content: ClipContent) -> Self {
        let c = Self::new();
        c.lock().content = Some(content);
        c
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set(&self, content: Option<ClipContent>) -> u64 {
        let seq = {
            let mut st = self.lock();
            st.content = content;
            st.seq = st.seq.wrapping_add(1);
            st.seq
        };
        self.seq.send_replace(seq);
        seq
    }

    /// Simulates the local user copying `content` (notifies like a real change).
    pub fn copy(&self, content: ClipContent) {
        self.set(Some(content));
    }

    /// Simulates the local user clearing the clipboard.
    pub fn clear(&self) {
        self.set(None);
    }

    /// What the clipboard holds now.
    pub fn content(&self) -> Option<ClipContent> {
        self.lock().content.clone()
    }

    /// How many times [`ClipboardBackend::write`] was called (writes from the peer).
    pub fn write_count(&self) -> usize {
        self.writes.load(Ordering::Relaxed)
    }
}

impl ClipboardBackend for MemoryClipboard {
    fn read(&self) -> Result<Option<ClipContent>, FilesError> {
        Ok(self.content())
    }

    fn write(&self, content: &ClipContent) -> Result<u64, FilesError> {
        self.writes.fetch_add(1, Ordering::Relaxed);
        Ok(self.set(Some(content.clone())))
    }

    fn changes(&self) -> Option<watch::Receiver<u64>> {
        Some(self.seq.subscribe())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_notifies_and_write_counts() {
        let c = MemoryClipboard::new();
        let mut rx = c.changes().unwrap();
        c.copy(ClipContent::Text("نص".into()));
        assert!(rx.has_changed().unwrap());
        assert_eq!(*rx.borrow_and_update(), 1);
        let seq = c.write(&ClipContent::Text("b".into())).unwrap();
        assert_eq!(seq, 2);
        assert_eq!(c.write_count(), 1);
        assert_eq!(c.read().unwrap(), Some(ClipContent::Text("b".into())));
        c.clear();
        assert_eq!(c.read().unwrap(), None);
    }
}
