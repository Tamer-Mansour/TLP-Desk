//! Explorer-native paste of the peer's copied files ("virtual files" on the clipboard).
//!
//! When the other PC announces copied files (`ClipboardFiles`), [`ShellClipboard::offer`] puts
//! a COM data object on this PC's clipboard that describes them
//! (`CFSTR_FILEDESCRIPTORW`) and renders their bytes only when Explorer pastes
//! (`CFSTR_FILECONTENTS`, delayed rendering). The user presses Ctrl+V in Explorer or on the
//! desktop and the files arrive, with Explorer's own progress dialog.
//!
//! * [`descriptor`]: the `FILEGROUPDESCRIPTORW` bytes (pure, tested everywhere).
//! * [`staged`]: the bytes of one virtual file, read from a staging folder that the transfer
//!   engine fills ([`Stager`]); a read waits (pumping messages) until the file is complete.
//! * `windows` (Windows only, the only `unsafe` code): the `IDataObject`, `IStream` and
//!   `IDataObjectAsyncCapability` implementations and the STA thread that owns the clipboard.
//! * [`ShellClipboard`]: the per-process manager (one offer at a time, keyed by session).
//!
//! The clipboard object lives in the process that owns the GUI session. In the service
//! deployment the agent in the user's session must own it (a service on session 0 has no
//! user clipboard); the in-process GUI path is what this crate wires up.
//!
//! References: Microsoft Learn, "Shell Clipboard Formats" (`CFSTR_FILEDESCRIPTOR`,
//! `CFSTR_FILECONTENTS`, `CFSTR_PREFERREDDROPEFFECT`, `CFSTR_PERFORMEDDROPEFFECT`),
//! "IDataObjectAsyncCapability", `OleSetClipboard`.

pub mod descriptor;
pub mod staged;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

pub use descriptor::{build_group_descriptor, filetime_from_unix_ms, GroupDescriptor};
pub use staged::{StageState, StagedItems, Stager};

/// Puts a virtual-files object on (and takes it off) the real clipboard. The real
/// implementation is the Windows STA thread; tests use a recording fake.
pub trait ClipboardPort: Send + Sync {
    /// Replaces the clipboard content with the virtual files of `offer`.
    fn set(&self, offer: Arc<StagedItems>);
    /// Empties the clipboard if it still holds our virtual files.
    fn clear(&self);
}

/// A port that does nothing (platforms without shell integration).
#[derive(Debug, Default, Clone, Copy)]
pub struct NullPort;

impl ClipboardPort for NullPort {
    fn set(&self, _offer: Arc<StagedItems>) {}
    fn clear(&self) {}
}

/// Owns the one active virtual clipboard offer of the process.
pub struct ShellClipboard {
    port: Arc<dyn ClipboardPort>,
    current: Mutex<Option<(u64, Arc<StagedItems>)>>,
}

impl std::fmt::Debug for ShellClipboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellClipboard").finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ShellClipboard {
    /// A manager on the real clipboard (Windows; elsewhere it does nothing).
    pub fn system() -> Arc<Self> {
        #[cfg(windows)]
        let port: Arc<dyn ClipboardPort> = Arc::new(windows::WindowsPort::new());
        #[cfg(not(windows))]
        let port: Arc<dyn ClipboardPort> = Arc::new(NullPort);
        Self::with_port(port)
    }

    /// A manager on `port` (tests).
    pub fn with_port(port: Arc<dyn ClipboardPort>) -> Arc<Self> {
        Arc::new(Self {
            port,
            current: Mutex::new(None),
        })
    }

    /// Offers `staged` as the clipboard content of session `key`, replacing (and cancelling,
    /// and deleting the staging folder of) any earlier offer.
    pub fn offer(&self, key: u64, staged: Arc<StagedItems>) {
        let old = lock(&self.current).replace((key, staged.clone()));
        if let Some((_, old)) = old {
            discard(&old);
        }
        tracing::info!(
            session = key,
            items = staged.items().len(),
            "virtual files placed on the clipboard"
        );
        self.port.set(staged);
    }

    /// Session `key` has no files to paste any more (peer cleared, permission revoked, session
    /// ended): takes our object off the clipboard if the session's offer is the current one.
    pub fn clear(&self, key: u64) {
        let old = {
            let mut cur = lock(&self.current);
            if cur.as_ref().is_some_and(|(k, _)| *k == key) {
                cur.take()
            } else {
                None
            }
        };
        if let Some((_, old)) = old {
            discard(&old);
            self.port.clear();
            tracing::info!(session = key, "virtual files removed from the clipboard");
        }
    }

    /// The session that owns the current offer, if any.
    pub fn current_session(&self) -> Option<u64> {
        lock(&self.current).as_ref().map(|(k, _)| *k)
    }
}

/// Stops the pull of `old` and removes what it staged.
fn discard(old: &Arc<StagedItems>) {
    old.cancel();
    let dir = old.dir().clone();
    // Explorer may still hold a handle for a moment: delete off-thread, retry once.
    let _ = std::thread::Builder::new()
        .name("tlp-shellclip-cleanup".into())
        .spawn(move || {
            for attempt in 0..3 {
                if !dir.exists() || std::fs::remove_dir_all(&dir).is_ok() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(300 * (attempt + 1)));
            }
            tracing::debug!(dir = %dir.display(), "staging folder not removed");
        });
}

/// Deletes staging folders left by an earlier run that crashed (older than a day).
pub fn purge_stale_staging(root: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    let day = std::time::Duration::from_secs(24 * 3600);
    for e in rd.filter_map(Result::ok) {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > day);
        if old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tlp_desk_proto::TransferItem;

    #[derive(Default)]
    struct FakePort {
        sets: AtomicUsize,
        clears: AtomicUsize,
        on_clipboard: Mutex<Option<Arc<StagedItems>>>,
    }

    impl ClipboardPort for FakePort {
        fn set(&self, offer: Arc<StagedItems>) {
            self.sets.fetch_add(1, Ordering::SeqCst);
            *lock(&self.on_clipboard) = Some(offer);
        }
        fn clear(&self) {
            self.clears.fetch_add(1, Ordering::SeqCst);
            *lock(&self.on_clipboard) = None;
        }
    }

    struct Idle;
    impl Stager for Idle {
        fn begin(&self) {}
        fn state(&self) -> StageState {
            StageState::Running
        }
        fn cancel(&self) {}
    }

    fn offer_in(dir: &std::path::Path) -> Arc<StagedItems> {
        let items = vec![TransferItem {
            rel_path: "ملف.txt".into(),
            size: 3,
            modified_unix_ms: 0,
            is_dir: false,
        }];
        StagedItems::new(dir.to_path_buf(), items, Arc::new(Idle))
    }

    #[test]
    fn announce_owns_the_clipboard_and_session_end_clears_it() {
        let tmp = tempfile::tempdir().unwrap();
        let port = Arc::new(FakePort::default());
        let sc = ShellClipboard::with_port(port.clone());
        let dir = tmp.path().join("s1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ملف.txt"), "abc").unwrap();
        let o = offer_in(&dir);
        sc.offer(7, o.clone());
        assert_eq!(sc.current_session(), Some(7));
        assert!(lock(&port.on_clipboard).is_some());

        // Another session ending does not touch our offer.
        sc.clear(8);
        assert!(lock(&port.on_clipboard).is_some());
        assert!(!o.is_cancelled());

        sc.clear(7);
        assert_eq!(sc.current_session(), None);
        assert!(lock(&port.on_clipboard).is_none());
        assert!(o.is_cancelled(), "pending reads must be interrupted");
        // The staging folder goes away.
        for _ in 0..100 {
            if !dir.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!dir.exists());
        // Clearing twice is harmless.
        sc.clear(7);
        assert_eq!(port.clears.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_newer_offer_replaces_and_cancels_the_older_one() {
        let tmp = tempfile::tempdir().unwrap();
        let port = Arc::new(FakePort::default());
        let sc = ShellClipboard::with_port(port.clone());
        let first = offer_in(&tmp.path().join("a"));
        let second = offer_in(&tmp.path().join("b"));
        sc.offer(1, first.clone());
        sc.offer(1, second.clone());
        assert!(first.is_cancelled());
        assert!(!second.is_cancelled());
        assert_eq!(port.sets.load(Ordering::SeqCst), 2);
        assert!(Arc::ptr_eq(
            lock(&port.on_clipboard).as_ref().unwrap(),
            &second
        ));
    }

    #[test]
    fn stale_staging_folders_are_purged_only_when_old() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("fresh")).unwrap();
        purge_stale_staging(tmp.path());
        assert!(tmp.path().join("fresh").exists());
        purge_stale_staging(&tmp.path().join("missing"));
    }
}
