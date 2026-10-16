//! Drag-out: files of the remote PC dragged from the file browser into Explorer, the desktop or
//! any program that accepts virtual files.
//!
//! The same virtual-files data object as the Explorer paste ([`super`]) serves the drag: it
//! describes every file and folder (`CFSTR_FILEDESCRIPTORW`) and hands out the bytes only when
//! the drop target reads them (`CFSTR_FILECONTENTS`, delayed rendering, with
//! `IDataObjectAsyncCapability` so Explorer copies on its own thread with its own progress
//! dialog). The difference is where the items come from: a drag starts with just the selected
//! names, so [`DragStager`] lists the remote folders through the transfer engine while the
//! pointer is still moving ([`DragOut::prepare`], called when the drag starts inside the
//! window), and downloads into a staging folder when the target asks for the first file.
//!
//! * [`DragOut`]: one prepared drag. [`DragOut::run`] enters the OS drag loop (Windows:
//!   `DoDragDrop`, see `windows`); [`DragOut::abandon`] forgets it (released inside the app).
//! * [`DragEngine`]: what a drag needs from the transfer engine ([`FileTransfer`] implements
//!   it; tests use a fake).
//! * [`next_step`]: the `IDropSource::QueryContinueDrag` decision, pure and tested.
//!
//! The staging folder is deleted when the drop target releases the data object (it finished
//! copying, or the drag was cancelled), and stale ones from a crashed run are purged at the next
//! drag. Needs `FILES` on the session, checked by the engine on every listing and download.
//!
//! References: Microsoft Learn, "DoDragDrop function", "IDropSource interface",
//! "Shell Clipboard Formats", "IDataObjectAsyncCapability", "CoMarshalInterThreadInterfaceInStream".

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tlp_desk_proto::TransferItem;
use tokio::runtime::Handle;
use tokio::sync::watch;

use super::staged::{StageState, StagedItems, Stager};
use crate::{ConflictPolicy, FileTransfer, TransferId};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Staging folders of drags live under `<temp>/TLP-Desk/drag`.
pub fn default_drag_root() -> PathBuf {
    std::env::temp_dir().join("TLP-Desk").join("drag")
}

/// A boxed engine call.
pub type EngineFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

/// What a drag-out needs from the transfer engine. [`FileTransfer`] is the real one.
pub trait DragEngine: Send + Sync + 'static {
    /// Lists `remote` (absolute host paths in one folder) recursively: the folder and the items.
    fn list(
        &self,
        session_key: u64,
        remote: Vec<String>,
    ) -> EngineFuture<'_, (String, Vec<TransferItem>)>;
    /// Downloads `items` of the host folder `source_dir` into the local folder `dest`.
    fn download(
        &self,
        session_key: u64,
        source_dir: String,
        items: Vec<TransferItem>,
        dest: PathBuf,
    ) -> EngineFuture<'_, TransferId>;
    /// How far download `id` has come.
    fn stage_state(&self, id: TransferId) -> StageState;
    /// Stops download `id`.
    fn cancel(&self, id: TransferId);
}

impl DragEngine for FileTransfer {
    fn list(
        &self,
        session_key: u64,
        remote: Vec<String>,
    ) -> EngineFuture<'_, (String, Vec<TransferItem>)> {
        Box::pin(async move {
            self.list_remote_items(session_key, remote)
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn download(
        &self,
        session_key: u64,
        source_dir: String,
        items: Vec<TransferItem>,
        dest: PathBuf,
    ) -> EngineFuture<'_, TransferId> {
        Box::pin(async move {
            // The staging folder is new and private: nothing there to keep.
            self.start_download_items(
                session_key,
                None,
                source_dir,
                items,
                dest,
                ConflictPolicy::Overwrite,
            )
            .await
            .map_err(|e| e.to_string())
        })
    }

    fn stage_state(&self, id: TransferId) -> StageState {
        self.snapshot(id)
            .map_or(StageState::Failed("transfer gone".into()), |s| {
                super::engine::stage_state(&s)
            })
    }

    fn cancel(&self, id: TransferId) {
        FileTransfer::cancel(self, id);
    }
}

/// Result of the remote listing: the host folder and the items.
type Listing = Option<Result<(String, Vec<TransferItem>), String>>;

/// Lists the dragged remote selection at once, and downloads it into the staging folder when
/// the drop target reads the first file.
pub struct DragStager {
    engine: Arc<dyn DragEngine>,
    rt: Handle,
    session_key: u64,
    dir: PathBuf,
    listing: watch::Receiver<Listing>,
    started: AtomicBool,
    cancelled: Arc<AtomicBool>,
    transfer: Arc<Mutex<Option<Result<TransferId, String>>>>,
}

impl DragStager {
    /// Starts listing `remote` (absolute host paths sharing one folder) of session
    /// `session_key` on `rt`; the files will be staged under `dir`.
    pub fn start(
        engine: Arc<dyn DragEngine>,
        rt: &Handle,
        session_key: u64,
        remote: Vec<String>,
        dir: PathBuf,
    ) -> Arc<Self> {
        let (tx, rx) = watch::channel(None);
        let lister = engine.clone();
        rt.spawn(async move {
            let listed = lister.list(session_key, remote).await;
            if let Err(e) = &listed {
                tracing::warn!(%e, "cannot list the dragged remote files");
            }
            let _ = tx.send(Some(listed));
        });
        Arc::new(Self {
            engine,
            rt: rt.clone(),
            session_key,
            dir,
            listing: rx,
            started: AtomicBool::new(false),
            cancelled: Arc::default(),
            transfer: Arc::default(),
        })
    }
}

impl Stager for DragStager {
    fn listing(&self) -> Option<Result<Vec<TransferItem>, String>> {
        self.listing.borrow().as_ref().map(|r| {
            r.as_ref()
                .map(|(_, items)| items.clone())
                .map_err(Clone::clone)
        })
    }

    fn begin(&self) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let (engine, key, dir) = (self.engine.clone(), self.session_key, self.dir.clone());
        let (mut listing, cancelled, transfer) = (
            self.listing.clone(),
            self.cancelled.clone(),
            self.transfer.clone(),
        );
        self.rt.spawn(async move {
            // Copy out of the watch before awaiting anything else.
            let listed = match listing.wait_for(Option::is_some).await {
                Ok(v) => v.clone(),
                Err(_) => None,
            };
            let result = match listed {
                Some(Ok((source_dir, items))) if !cancelled.load(Ordering::SeqCst) => {
                    engine.download(key, source_dir, items, dir).await
                }
                Some(Ok(_)) => Err("cancelled".into()),
                Some(Err(e)) => Err(e),
                None => Err("the listing stopped".into()),
            };
            if let Ok(id) = &result {
                if cancelled.load(Ordering::SeqCst) {
                    engine.cancel(*id);
                }
            }
            *lock(&transfer) = Some(result);
        });
    }

    fn state(&self) -> StageState {
        match &*lock(&self.transfer) {
            None => StageState::Idle,
            Some(Err(e)) => StageState::Failed(e.clone()),
            Some(Ok(id)) => self.engine.stage_state(*id),
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(Ok(id)) = &*lock(&self.transfer) {
            self.engine.cancel(*id);
        }
    }
}

/// How a drag out of the window ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DragOutcome {
    /// Dropped on a target that accepted the files; it copies them now (Explorer shows its
    /// own progress).
    Dropped,
    /// Cancelled (Esc, dropped where nothing accepts files).
    Cancelled,
    /// This platform has no drag-out.
    Unsupported,
    /// The OS drag could not start (details for the log).
    Failed(String),
}

/// One drag of remote files, prepared when the pointer drag starts in the file browser.
pub struct DragOut {
    staged: Arc<StagedItems>,
}

impl std::fmt::Debug for DragOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DragOut")
            .field("dir", self.staged.dir())
            .finish_non_exhaustive()
    }
}

impl DragOut {
    /// Starts listing the remote selection `remote` (absolute host paths in one folder) of
    /// viewer session `session_key`, staged in a new folder under `root`.
    ///
    /// # Errors
    /// The staging folder cannot be created.
    pub fn prepare(
        engine: Arc<dyn DragEngine>,
        rt: &Handle,
        session_key: u64,
        remote: Vec<String>,
        root: &Path,
    ) -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        super::purge_stale_staging(root);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = root.join(format!("{session_key:x}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let stager = DragStager::start(engine, rt, session_key, remote, dir.clone());
        Ok(Self {
            staged: StagedItems::listed_later(dir, stager),
        })
    }

    /// The items being dragged and their staging folder.
    pub fn staged(&self) -> &Arc<StagedItems> {
        &self.staged
    }

    /// Enters the OS drag loop with the files and returns when they were dropped or the drag
    /// was cancelled. The files are copied afterwards, in the background, by the drop target.
    ///
    /// Windows: call it on the GUI thread while the mouse button is still down. `DoDragDrop`
    /// runs its own modal loop there (the window does not repaint until the drop); the data
    /// object lives on a separate thread so the target's reads never block the GUI.
    pub fn run(self) -> DragOutcome {
        #[cfg(windows)]
        {
            windows::run(self.staged)
        }
        #[cfg(not(windows))]
        {
            self.abandon();
            DragOutcome::Unsupported
        }
    }

    /// Forgets the drag (released inside the app): stops the listing and removes the folder.
    pub fn abandon(self) {
        super::discard(&self.staged);
    }
}

/// Where the mouse pointer is on the screen, in physical pixels (`None` when unknown, and on
/// platforms without drag-out). While another program drags files over a window, winit
/// reports no pointer position, so a drop onto the file browser is hit-tested with this.
pub fn cursor_screen_position() -> Option<(i32, i32)> {
    #[cfg(windows)]
    {
        windows::cursor_screen_position()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// What the drag loop does next (`IDropSource::QueryContinueDrag`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragStep {
    /// Keep dragging.
    Continue,
    /// The button was released: drop here.
    Drop,
    /// Esc was pressed: cancel.
    Cancel,
}

/// The drag-loop decision: Esc cancels, releasing the left button drops, anything else
/// continues (Microsoft Learn, "IDropSource::QueryContinueDrag").
pub fn next_step(escape_pressed: bool, left_button_down: bool) -> DragStep {
    if escape_pressed {
        DragStep::Cancel
    } else if !left_button_down {
        DragStep::Drop
    } else {
        DragStep::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::Read;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    /// A transfer engine on the local disk: "lists" a fixed selection and "downloads" by
    /// writing the files the way the real engine does (`.tlpdesk-part`, then rename).
    #[derive(Default)]
    struct FakeEngine {
        files: Vec<(String, Vec<u8>)>,
        list_error: Option<String>,
        list_delay: Duration,
        listed: Mutex<Vec<Vec<String>>>,
        downloads: AtomicUsize,
        states: Arc<Mutex<HashMap<u64, StageState>>>,
        cancelled: Mutex<Vec<TransferId>>,
    }

    impl FakeEngine {
        fn items(&self) -> Vec<TransferItem> {
            let mut items = vec![TransferItem {
                rel_path: "مجلد".into(),
                size: 0,
                modified_unix_ms: 0,
                is_dir: true,
            }];
            items.extend(self.files.iter().map(|(rel, data)| TransferItem {
                rel_path: rel.clone(),
                size: data.len() as u64,
                modified_unix_ms: 1_700_000_000_000,
                is_dir: false,
            }));
            items
        }
    }

    const SOURCE: &str = "D:\\مشترك";

    impl DragEngine for FakeEngine {
        fn list(
            &self,
            _key: u64,
            remote: Vec<String>,
        ) -> EngineFuture<'_, (String, Vec<TransferItem>)> {
            lock(&self.listed).push(remote);
            Box::pin(async move {
                tokio::time::sleep(self.list_delay).await;
                match &self.list_error {
                    Some(e) => Err(e.clone()),
                    None => Ok((SOURCE.to_string(), self.items())),
                }
            })
        }

        fn download(
            &self,
            _key: u64,
            source_dir: String,
            items: Vec<TransferItem>,
            dest: PathBuf,
        ) -> EngineFuture<'_, TransferId> {
            let n = self.downloads.fetch_add(1, Ordering::SeqCst) as u64 + 1;
            assert_eq!(source_dir, SOURCE);
            assert_eq!(items.len(), self.files.len() + 1);
            let states = self.states.clone();
            lock(&states).insert(n, StageState::Running);
            let files = self.files.clone();
            std::thread::spawn(move || {
                for (rel, data) in files {
                    let mut p = dest.clone();
                    p.extend(rel.split('/'));
                    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                    let part = p.with_extension("tlpdesk-part");
                    std::fs::write(&part, &data).unwrap();
                    std::thread::sleep(Duration::from_millis(30));
                    std::fs::rename(&part, &p).unwrap();
                }
                lock(&states).insert(n, StageState::Finished);
            });
            Box::pin(async move { Ok(TransferId(n)) })
        }

        fn stage_state(&self, id: TransferId) -> StageState {
            lock(&self.states)
                .get(&id.0)
                .cloned()
                .unwrap_or(StageState::Failed("unknown".into()))
        }

        fn cancel(&self, id: TransferId) {
            lock(&self.cancelled).push(id);
        }
    }

    fn engine(list_error: Option<&str>) -> Arc<FakeEngine> {
        Arc::new(FakeEngine {
            files: vec![
                (
                    "مجلد/تقرير.txt".into(),
                    "تقرير الربع الأول".as_bytes().to_vec(),
                ),
                ("صورة.bin".into(), vec![0xA5; 70_000]),
            ],
            list_error: list_error.map(str::to_owned),
            list_delay: Duration::from_millis(40),
            ..FakeEngine::default()
        })
    }

    fn remote(name: &str) -> String {
        format!("{SOURCE}\\{name}")
    }

    fn read(staged: &StagedItems, rel: &str) -> std::io::Result<Vec<u8>> {
        staged.wait_items(&mut || {})?;
        let idx = staged
            .items()
            .iter()
            .position(|i| i.rel_path == rel)
            .expect("listed");
        let mut buf = Vec::new();
        staged.open(idx, &mut || {})?.read_to_end(&mut buf)?;
        Ok(buf)
    }

    fn gone(dir: &Path) -> bool {
        for _ in 0..150 {
            if !dir.exists() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn a_drag_lists_at_once_and_downloads_only_when_the_target_reads() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let root = tempfile::tempdir().unwrap();
        let fake = engine(None);
        let selection = vec![remote("مجلد"), remote("صورة.bin")];
        let drag =
            DragOut::prepare(fake.clone(), rt.handle(), 7, selection.clone(), root.path()).unwrap();
        let staged = drag.staged().clone();
        assert!(staged.dir().starts_with(root.path()));
        assert!(staged.dir().is_dir(), "the staging folder exists at once");

        // The listing runs as soon as the drag starts...
        let items = staged.wait_items(&mut || {}).unwrap().to_vec();
        assert_eq!(*lock(&fake.listed), vec![selection]);
        assert!(items.iter().any(|i| i.rel_path == "مجلد" && i.is_dir));
        // ... but no byte moves before the drop target asks (delayed rendering).
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(fake.downloads.load(Ordering::SeqCst), 0);
        assert_eq!(staged.items().len(), 3);

        assert_eq!(
            read(&staged, "مجلد/تقرير.txt").unwrap(),
            "تقرير الربع الأول".as_bytes()
        );
        assert_eq!(read(&staged, "صورة.bin").unwrap(), vec![0xA5; 70_000]);
        assert_eq!(
            fake.downloads.load(Ordering::SeqCst),
            1,
            "one download serves every file of the drag"
        );

        // The target released the data object: the staging folder goes away.
        let dir = staged.dir().clone();
        drag.abandon();
        assert!(staged.is_cancelled());
        assert!(gone(&dir), "staging folder left behind");
        assert_eq!(*lock(&fake.cancelled), vec![TransferId(1)]);
    }

    #[test]
    fn a_drag_released_inside_the_app_never_downloads() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let root = tempfile::tempdir().unwrap();
        let fake = engine(None);
        let drag = DragOut::prepare(
            fake.clone(),
            rt.handle(),
            1,
            vec![remote("صورة.bin")],
            root.path(),
        )
        .unwrap();
        let staged = drag.staged().clone();
        let dir = staged.dir().clone();
        drag.abandon();
        assert!(gone(&dir));
        // A read that still comes in (a slow target) is refused instead of downloading.
        assert!(staged.open(0, &mut || {}).is_err());
        staged.begin();
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(fake.downloads.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_failed_listing_fails_the_reads() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let root = tempfile::tempdir().unwrap();
        let fake = engine(Some("no FILES permission"));
        let drag = DragOut::prepare(
            fake.clone(),
            rt.handle(),
            3,
            vec![remote("مجلد")],
            root.path(),
        )
        .unwrap();
        let err = drag.staged().wait_items(&mut || {}).unwrap_err();
        assert!(err.to_string().contains("FILES"), "{err}");
        assert!(drag.staged().open(0, &mut || {}).is_err());
        assert_eq!(fake.downloads.load(Ordering::SeqCst), 0);
        drag.abandon();
    }

    #[test]
    fn staging_folders_are_unique_per_drag() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let root = tempfile::tempdir().unwrap();
        let fake = engine(None);
        let a = DragOut::prepare(fake.clone(), rt.handle(), 9, vec![], root.path()).unwrap();
        let b = DragOut::prepare(fake, rt.handle(), 9, vec![], root.path()).unwrap();
        assert_ne!(a.staged().dir(), b.staged().dir());
        a.abandon();
        b.abandon();
    }

    #[test]
    fn the_drag_loop_drops_on_release_and_cancels_on_escape() {
        assert_eq!(next_step(false, true), DragStep::Continue);
        assert_eq!(next_step(false, false), DragStep::Drop);
        assert_eq!(next_step(true, true), DragStep::Cancel);
        assert_eq!(next_step(true, false), DragStep::Cancel, "Esc wins");
    }
}
