//! Reading the file list of the local clipboard (CF_HDROP on Windows) and watching it for
//! changes.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::watch;

/// Something that can read the file list currently on the clipboard.
pub trait ClipboardFileSource: Send {
    /// The copied files, or `None` if the clipboard holds no files (or cannot be read now).
    fn file_list(&mut self) -> Option<Vec<PathBuf>>;
}

/// Creates the clipboard reader for the watcher thread (`None` = no clipboard access, e.g.
/// in a service without a desktop).
pub type SourceFactory = Arc<dyn Fn() -> Option<Box<dyn ClipboardFileSource>> + Send + Sync>;

/// The real clipboard: arboard on Windows, nothing elsewhere yet.
pub fn system_source() -> SourceFactory {
    Arc::new(system::open)
}

/// Files currently on this device's clipboard (e.g. for "paste my copied files on the remote
/// PC"). Empty if there are none or the clipboard is unavailable.
pub fn local_file_list() -> Vec<PathBuf> {
    system::open()
        .and_then(|mut s| s.file_list())
        .unwrap_or_default()
}

#[cfg(windows)]
mod system {
    use super::ClipboardFileSource;
    use std::path::PathBuf;

    struct Arboard(arboard::Clipboard);

    impl ClipboardFileSource for Arboard {
        fn file_list(&mut self) -> Option<Vec<PathBuf>> {
            // Any error (no CF_HDROP, clipboard busy in another app) reads as "no files"; the
            // next poll tries again.
            self.0.get().file_list().ok().filter(|l| !l.is_empty())
        }
    }

    pub(super) fn open() -> Option<Box<dyn ClipboardFileSource>> {
        match arboard::Clipboard::new() {
            Ok(c) => Some(Box::new(Arboard(c))),
            Err(e) => {
                tracing::debug!(%e, "clipboard unavailable");
                None
            }
        }
    }
}

#[cfg(not(windows))]
mod system {
    use super::ClipboardFileSource;

    pub(super) fn open() -> Option<Box<dyn ClipboardFileSource>> {
        None
    }
}

/// What the watcher publishes: the current copied file list (empty = no files).
pub type Snapshot = Arc<Vec<PathBuf>>;

/// One polling thread per process, shared by every session. It runs only while at least one
/// session watches and publishes the list only when it changes.
pub(crate) struct Watcher {
    factory: SourceFactory,
    poll: Duration,
    tx: Mutex<Option<watch::Sender<Snapshot>>>,
}

impl Watcher {
    pub(crate) fn new(factory: SourceFactory, poll: Duration) -> Self {
        Self {
            factory,
            poll,
            tx: Mutex::new(None),
        }
    }

    /// A receiver of clipboard changes; starts the polling thread if none is running.
    pub(crate) fn subscribe(&self) -> watch::Receiver<Snapshot> {
        let mut slot = self.tx.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(tx) = slot.as_ref().filter(|tx| !tx.is_closed()) {
            return tx.subscribe();
        }
        let (tx, rx) = watch::channel(Snapshot::default());
        let factory = self.factory.clone();
        let poll = self.poll;
        let thread_tx = tx.clone();
        let spawned = std::thread::Builder::new()
            .name("tlp-clipfiles".into())
            .spawn(move || run(&factory, poll, &thread_tx));
        if let Err(e) = spawned {
            tracing::warn!(%e, "cannot start the clipboard watcher");
        }
        *slot = Some(tx);
        rx
    }
}

fn run(factory: &SourceFactory, poll: Duration, tx: &watch::Sender<Snapshot>) {
    let Some(mut source) = factory() else {
        return;
    };
    // `is_closed`: every session dropped its receiver; a new session starts a new thread.
    while !tx.is_closed() {
        let now = source.file_list().unwrap_or_default();
        tx.send_if_modified(|cur| {
            if **cur == now {
                false
            } else {
                *cur = Arc::new(now);
                true
            }
        });
        std::thread::sleep(poll);
    }
}

/// A clipboard that holds whatever [`ManualClipboard::set`] put there: for tests, demos and
/// the app's simulated backend. Never touches the real clipboard.
#[derive(Clone, Default)]
pub struct ManualClipboard(Arc<Mutex<Vec<PathBuf>>>);

impl std::fmt::Debug for ManualClipboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ManualClipboard").finish()
    }
}

impl ManualClipboard {
    /// "Copies" `paths` (empty = the clipboard holds no files).
    pub fn set(&self, paths: Vec<PathBuf>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = paths;
    }

    /// A [`SourceFactory`] reading this clipboard.
    pub fn factory(&self) -> SourceFactory {
        let me = self.clone();
        Arc::new(move || Some(Box::new(me.clone()) as Box<dyn ClipboardFileSource>))
    }
}

impl ClipboardFileSource for ManualClipboard {
    fn file_list(&mut self) -> Option<Vec<PathBuf>> {
        let l = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        (!l.is_empty()).then_some(l)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn watcher_publishes_changes_and_stops_without_subscribers() {
        let clip = ManualClipboard::default();
        let w = Watcher::new(clip.factory(), Duration::from_millis(5));
        let mut rx = w.subscribe();
        clip.set(vec![PathBuf::from("ملف.txt")]);
        tokio::time::timeout(Duration::from_secs(5), rx.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(**rx.borrow_and_update(), vec![PathBuf::from("ملف.txt")]);
        clip.set(Vec::new());
        tokio::time::timeout(Duration::from_secs(5), rx.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(rx.borrow().is_empty());
        drop(rx);
        // A later subscriber gets a working watcher again.
        let mut rx = w.subscribe();
        clip.set(vec![PathBuf::from("b")]);
        tokio::time::timeout(Duration::from_secs(5), rx.changed())
            .await
            .unwrap()
            .unwrap();
    }

    /// Reads the real clipboard; run by hand (`cargo test -- --ignored`) after copying files
    /// in Explorer. Only reads, never writes the clipboard.
    #[test]
    #[ignore = "reads the real clipboard"]
    fn real_clipboard_file_list() {
        println!("{:?}", local_file_list());
    }
}
