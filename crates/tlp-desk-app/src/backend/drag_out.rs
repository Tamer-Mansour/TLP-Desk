//! Drag-out glue for the real backend (TLP-drag-out): the one drag of remote files prepared
//! when a pointer drag starts in the file browser, run when the pointer leaves the window, or
//! abandoned when it is released inside (see `tlp_desk_files::shellclip::drag`).

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tlp_desk_files::shellclip::drag::{default_drag_root, DragEngine, DragOut, DragOutcome};
use tokio::runtime::Handle;

use super::SessionId;

/// Holds at most one prepared drag (there is one mouse).
pub(crate) struct DragOutSlot {
    pending: Mutex<Option<(SessionId, DragOut)>>,
    root: PathBuf,
}

impl Default for DragOutSlot {
    fn default() -> Self {
        Self::new(default_drag_root())
    }
}

impl DragOutSlot {
    /// Staging folders go under `root`.
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            pending: Mutex::new(None),
            root,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Option<(SessionId, DragOut)>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts listing `remote` of viewer session `session` (feature key `key`), replacing an
    /// earlier prepared drag.
    pub(crate) fn prepare(
        &self,
        rt: &Handle,
        engine: Arc<dyn DragEngine>,
        session: SessionId,
        key: u64,
        remote: Vec<String>,
    ) {
        if remote.is_empty() {
            return;
        }
        let drag = match DragOut::prepare(engine, rt, key, remote, &self.root) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(%e, "cannot prepare the drag-out staging folder");
                return;
            }
        };
        let old = self.lock().replace((session, drag));
        if let Some((_, old)) = old {
            old.abandon();
        }
    }

    /// Runs the prepared drag of `session` (blocks in the OS drag loop).
    pub(crate) fn run(&self, session: SessionId) -> DragOutcome {
        // Take it out first: the drag loop must not run with the lock held.
        let taken = self.take(session);
        match taken {
            Some(drag) => drag.run(),
            None => DragOutcome::Failed("no prepared drag".into()),
        }
    }

    /// Forgets the prepared drag of `session`.
    pub(crate) fn cancel(&self, session: SessionId) {
        if let Some(drag) = self.take(session) {
            drag.abandon();
        }
    }

    fn take(&self, session: SessionId) -> Option<DragOut> {
        let mut pending = self.lock();
        if pending.as_ref().is_some_and(|(s, _)| *s == session) {
            pending.take().map(|(_, d)| d)
        } else {
            None
        }
    }

    /// Whether `session` has a prepared drag (tests).
    #[cfg(test)]
    fn has(&self, session: SessionId) -> bool {
        self.lock().as_ref().is_some_and(|(s, _)| *s == session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tlp_desk_files::{FileTransfer, TransferConfig};

    fn gone(dir: &std::path::Path) -> bool {
        for _ in 0..150 {
            if !dir.exists() || std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_none()) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn one_prepared_drag_at_a_time_and_cancel_cleans_up() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let root = tempfile::tempdir().unwrap();
        let slot = DragOutSlot::new(root.path().to_path_buf());
        // No session in this engine: the listing fails, which is fine for the bookkeeping.
        let engine: Arc<dyn DragEngine> = FileTransfer::new(TransferConfig::default());
        slot.prepare(rt.handle(), engine.clone(), SessionId(1), 5, vec![]);
        assert!(
            !slot.has(SessionId(1)),
            "nothing selected, nothing prepared"
        );

        slot.prepare(
            rt.handle(),
            engine.clone(),
            SessionId(1),
            5,
            vec!["C:\\مستندات\\ملف.txt".into()],
        );
        assert!(slot.has(SessionId(1)));
        // A newer drag (another session) replaces it.
        slot.prepare(rt.handle(), engine, SessionId(2), 6, vec!["D:\\صور".into()]);
        assert!(!slot.has(SessionId(1)));
        assert!(slot.has(SessionId(2)));
        // Cancelling another session's drag does nothing; its own clears it.
        slot.cancel(SessionId(1));
        assert!(slot.has(SessionId(2)));
        slot.cancel(SessionId(2));
        assert!(!slot.has(SessionId(2)));
        assert!(gone(root.path()), "staging folders left behind");
        // Running without a prepared drag reports it instead of blocking.
        assert!(matches!(slot.run(SessionId(2)), DragOutcome::Failed(_)));
    }
}
