//! Serves the bytes of a virtual clipboard file from a staging folder that the transfer engine
//! fills in the background.
//!
//! The engine writes every file as `name.tlpdesk-part` and renames it to its real name only
//! after BLAKE3 verification, so "the final file exists with the announced size" is the
//! complete, correct signal (no partial reads, no torn data). A read of a virtual file waits
//! for that signal; while waiting it calls a `pump` closure so the COM thread keeps handling
//! messages. Waiting is bounded: the pull must start, the transfer must not fail, and the
//! virtual clipboard must not be cancelled (session ended, peer copied something else).

use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tlp_desk_proto::TransferItem;

use crate::clipfiles::is_safe_rel_path;

/// How far the background pull of a virtual clipboard has come.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageState {
    /// Not started, or the engine has not registered the transfer yet.
    Idle,
    /// Bytes are moving.
    Running,
    /// The pull finished (files that did not fail are in the staging folder).
    Finished,
    /// The pull failed or was refused; the text is for the log only.
    Failed(String),
}

/// Pulls the announced files of the peer into a staging folder. Implemented by the app on top
/// of the transfer engine (viewer: a download; host: a request that makes the viewer upload).
pub trait Stager: Send + Sync {
    /// Starts pulling every item. Idempotent: only the first call starts anything.
    fn begin(&self);
    /// Progress of the pull.
    fn state(&self) -> StageState;
    /// Stops the pull (the virtual clipboard is gone).
    fn cancel(&self);
}

/// The files of one virtual clipboard offer and where they are staged.
pub struct StagedItems {
    dir: PathBuf,
    items: Vec<TransferItem>,
    stager: Arc<dyn Stager>,
    cancelled: AtomicBool,
    /// Longest wait for the pull to start (state still [`StageState::Idle`]).
    start_timeout: Duration,
}

impl std::fmt::Debug for StagedItems {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StagedItems")
            .field("items", &self.items.len())
            .finish_non_exhaustive()
    }
}

impl StagedItems {
    /// Items announced by the peer, staged under `dir` by `stager`.
    pub fn new(dir: PathBuf, items: Vec<TransferItem>, stager: Arc<dyn Stager>) -> Arc<Self> {
        Arc::new(Self {
            dir,
            items,
            stager,
            cancelled: AtomicBool::new(false),
            start_timeout: Duration::from_secs(60),
        })
    }

    /// Like [`StagedItems::new`] with a custom start timeout (tests).
    pub fn with_start_timeout(self: Arc<Self>, timeout: Duration) -> Arc<Self> {
        Arc::new(Self {
            dir: self.dir.clone(),
            items: self.items.clone(),
            stager: self.stager.clone(),
            cancelled: AtomicBool::new(false),
            start_timeout: timeout,
        })
    }

    /// The announced items.
    pub fn items(&self) -> &[TransferItem] {
        &self.items
    }

    /// The staging folder.
    pub fn dir(&self) -> &PathBuf {
        &self.dir
    }

    /// Starts the background pull (idempotent).
    pub fn begin(&self) {
        self.stager.begin();
    }

    /// Makes every pending and future wait fail and stops the pull.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.stager.cancel();
    }

    /// Whether [`StagedItems::cancel`] was called.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// The staged path of item `index` (never outside the staging folder).
    fn path_of(&self, index: usize) -> io::Result<(PathBuf, u64)> {
        let item = self
            .items
            .get(index)
            .filter(|i| !i.is_dir && is_safe_rel_path(&i.rel_path))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no such file"))?;
        let mut p = self.dir.clone();
        p.extend(item.rel_path.split('/'));
        Ok((p, item.size))
    }

    /// Whether item `index` is completely staged right now.
    pub fn is_ready(&self, index: usize) -> bool {
        self.path_of(index)
            .ok()
            .and_then(|(p, size)| {
                std::fs::metadata(p)
                    .ok()
                    .map(|m| m.is_file() && m.len() == size)
            })
            .unwrap_or(false)
    }

    /// Starts the pull and waits until item `index` is completely staged, then opens it.
    /// `pump` is called every ~20 ms while waiting.
    ///
    /// # Errors
    /// Cancelled, the pull failed or never started, or the item is unknown.
    pub fn open(&self, index: usize, pump: &mut dyn FnMut()) -> io::Result<File> {
        self.begin();
        let (path, size) = self.path_of(index)?;
        let started = Instant::now();
        loop {
            if self.is_cancelled() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            let state = self.stager.state();
            if self.is_ready(index) {
                return File::open(&path);
            }
            match state {
                StageState::Failed(why) => {
                    return Err(io::Error::other(why));
                }
                // The check above ran before `Finished` was read: look once more.
                StageState::Finished if !self.is_ready(index) => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("file missing after the transfer ({size} bytes expected)"),
                    ));
                }
                StageState::Idle if started.elapsed() > self.start_timeout => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "the other PC did not start sending",
                    ));
                }
                _ => {}
            }
            pump();
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::sync::Mutex;

    /// A fake engine: `begin` starts a thread that "downloads" the files one by one.
    struct Fake {
        dir: PathBuf,
        files: Vec<(String, Vec<u8>)>,
        state: Mutex<StageState>,
        begun: AtomicBool,
        fail_after: Option<usize>,
        begin_calls: std::sync::atomic::AtomicUsize,
    }

    impl Fake {
        fn new(
            dir: PathBuf,
            files: Vec<(String, Vec<u8>)>,
            fail_after: Option<usize>,
        ) -> Arc<Self> {
            Arc::new(Self {
                dir,
                files,
                state: Mutex::new(StageState::Idle),
                begun: AtomicBool::new(false),
                fail_after,
                begin_calls: Default::default(),
            })
        }
    }

    impl Stager for Arc<Fake> {
        fn begin(&self) {
            self.begin_calls.fetch_add(1, Ordering::SeqCst);
            if self.begun.swap(true, Ordering::SeqCst) {
                return;
            }
            let me = self.clone();
            std::thread::spawn(move || {
                *me.state.lock().unwrap() = StageState::Running;
                for (i, (rel, data)) in me.files.iter().enumerate() {
                    if me.fail_after == Some(i) {
                        *me.state.lock().unwrap() = StageState::Failed("boom".into());
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(60));
                    let mut p = me.dir.clone();
                    p.extend(rel.split('/'));
                    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                    let part = p.with_extension("tlpdesk-part");
                    std::fs::write(&part, &data[..data.len() / 2]).unwrap();
                    std::thread::sleep(Duration::from_millis(30));
                    std::fs::write(&part, data).unwrap();
                    std::fs::rename(&part, &p).unwrap();
                }
                *me.state.lock().unwrap() = StageState::Finished;
            });
        }
        fn state(&self) -> StageState {
            self.state.lock().unwrap().clone()
        }
        fn cancel(&self) {
            *self.state.lock().unwrap() = StageState::Failed("cancelled".into());
        }
    }

    fn items_of(files: &[(String, Vec<u8>)]) -> Vec<TransferItem> {
        files
            .iter()
            .map(|(r, d)| TransferItem {
                rel_path: r.clone(),
                size: d.len() as u64,
                modified_unix_ms: 0,
                is_dir: false,
            })
            .collect()
    }

    fn staged(
        files: Vec<(String, Vec<u8>)>,
        fail_after: Option<usize>,
    ) -> (tempfile::TempDir, Arc<Fake>, Arc<StagedItems>) {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Fake::new(tmp.path().to_path_buf(), files.clone(), fail_after);
        let s = StagedItems::new(
            tmp.path().to_path_buf(),
            items_of(&files),
            Arc::new(fake.clone()),
        );
        (tmp, fake, s)
    }

    #[test]
    fn open_waits_for_the_complete_file_and_pumps() {
        let files = vec![
            (
                "مجلد/ملف.txt".to_string(),
                b"hello arabic \xd9\x85".to_vec(),
            ),
            ("b.bin".to_string(), vec![7u8; 5000]),
        ];
        let (_tmp, fake, s) = staged(files.clone(), None);
        let mut pumps = 0;
        let mut f = s.open(1, &mut || pumps += 1).unwrap();
        let mut got = Vec::new();
        f.read_to_end(&mut got).unwrap();
        assert_eq!(got, files[1].1);
        assert!(pumps > 0, "the wait must pump messages");
        // The first item is ready too, and begin ran only once for the engine.
        assert!(s.is_ready(0));
        let mut f0 = s.open(0, &mut || {}).unwrap();
        let mut got0 = Vec::new();
        f0.read_to_end(&mut got0).unwrap();
        assert_eq!(got0, files[0].1);
        assert!(fake.begun.load(Ordering::SeqCst));
    }

    #[test]
    fn a_failed_pull_fails_the_wait() {
        let files = vec![
            ("a".to_string(), vec![1; 10]),
            ("b".to_string(), vec![2; 10]),
        ];
        let (_tmp, _fake, s) = staged(files, Some(1));
        // Item 0 arrives, item 1 never does.
        assert!(s.open(0, &mut || {}).is_ok());
        let err = s.open(1, &mut || {}).unwrap_err();
        assert!(err.to_string().contains("boom"), "{err}");
    }

    #[test]
    fn cancel_interrupts_a_waiting_read() {
        let files = vec![("a".to_string(), vec![1; 10])];
        let tmp = tempfile::tempdir().unwrap();
        // A stager that never produces anything and never fails.
        struct Stuck;
        impl Stager for Stuck {
            fn begin(&self) {}
            fn state(&self) -> StageState {
                StageState::Running
            }
            fn cancel(&self) {}
        }
        let s = StagedItems::new(tmp.path().to_path_buf(), items_of(&files), Arc::new(Stuck));
        let s2 = s.clone();
        let t = std::thread::spawn(move || s2.open(0, &mut || {}));
        std::thread::sleep(Duration::from_millis(100));
        s.cancel();
        let err = t.join().unwrap().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn a_pull_that_never_starts_times_out() {
        struct Never;
        impl Stager for Never {
            fn begin(&self) {}
            fn state(&self) -> StageState {
                StageState::Idle
            }
            fn cancel(&self) {}
        }
        let tmp = tempfile::tempdir().unwrap();
        let files = vec![("a".to_string(), vec![1; 3])];
        let s = StagedItems::new(tmp.path().to_path_buf(), items_of(&files), Arc::new(Never))
            .with_start_timeout(Duration::from_millis(80));
        let err = s.open(0, &mut || {}).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn hostile_indexes_and_paths_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        struct Done;
        impl Stager for Done {
            fn begin(&self) {}
            fn state(&self) -> StageState {
                StageState::Finished
            }
            fn cancel(&self) {}
        }
        let items = vec![
            TransferItem {
                rel_path: "../evil".into(),
                size: 1,
                modified_unix_ms: 0,
                is_dir: false,
            },
            TransferItem {
                rel_path: "dir".into(),
                size: 0,
                modified_unix_ms: 0,
                is_dir: true,
            },
        ];
        let s = StagedItems::new(tmp.path().to_path_buf(), items, Arc::new(Done));
        assert!(s.open(0, &mut || {}).is_err());
        assert!(s.open(1, &mut || {}).is_err());
        assert!(s.open(99, &mut || {}).is_err());
    }
}
