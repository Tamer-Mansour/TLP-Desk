//! Glue between the clipboard announcements ([`crate::clipfiles`]), the transfer engine and the
//! virtual clipboard ([`super::ShellClipboard`]).
//!
//! * The peer announces files: [`install`]'s remote handler stages them in a fresh folder and
//!   offers them on the clipboard. Nothing is transferred until Explorer asks for the bytes.
//! * Explorer asks (first `GetData(FILECONTENTS)`): the viewer starts a download of the
//!   announced id into the staging folder ([`ViewerStager`]); the host asks the viewer to
//!   upload ([`HostStager`], the pull echo) and receives into its staging folder, which the
//!   engine learns through `set_announced_dest`.
//! * The peer clears its clipboard, the permission is revoked or the session ends: the offer is
//!   taken off the clipboard and the staging folder deleted.
//! * The viewer's side of a host paste: the upload handler sends the files this PC announced.
//!
//! Needs `CLIPBOARD` and `FILES` on the session (checked by the clipfiles feature on every
//! announcement and by the transfer engine on every offer), and both are live-revocable.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::runtime::Handle;

use super::staged::{StageState, StagedItems, Stager};
use super::ShellClipboard;
use crate::clipfiles::{AnnouncedFiles, ClipFilesFeature, PullFn, RemoteClipboardFiles};
use crate::{
    ConflictPolicy, FileTransfer, TransferId, TransferRole, TransferSnapshot, TransferState,
};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Staging folders live under `<temp>/TLP-Desk/paste`.
pub fn default_staging_root() -> PathBuf {
    std::env::temp_dir().join("TLP-Desk").join("paste")
}

/// Maps the engine's state of a transfer to what a waiting read needs.
pub fn stage_state(snapshot: &TransferSnapshot) -> StageState {
    match &snapshot.state {
        TransferState::Preparing | TransferState::Running | TransferState::Paused => {
            StageState::Running
        }
        TransferState::Completed => StageState::Finished,
        TransferState::Cancelled => StageState::Failed("cancelled".into()),
        TransferState::Failed(e) => StageState::Failed(format!("{e:?}")),
        TransferState::Disconnected => StageState::Failed("session ended".into()),
    }
}

/// Viewer: pulls the host's announced files with a download into the staging folder.
pub struct ViewerStager {
    engine: Arc<FileTransfer>,
    rt: Handle,
    session_key: u64,
    files: RemoteClipboardFiles,
    dir: PathBuf,
    policy: ConflictPolicy,
    started: AtomicBool,
    outcome: Arc<Mutex<Option<Result<TransferId, String>>>>,
}

impl ViewerStager {
    /// A stager for `files` announced in session `session_key`, staged under `dir`.
    pub fn new(
        engine: Arc<FileTransfer>,
        rt: Handle,
        session_key: u64,
        files: RemoteClipboardFiles,
        dir: PathBuf,
        policy: ConflictPolicy,
    ) -> Arc<Self> {
        Arc::new(Self {
            engine,
            rt,
            session_key,
            files,
            dir,
            policy,
            started: AtomicBool::new(false),
            outcome: Arc::default(),
        })
    }
}

impl Stager for ViewerStager {
    fn begin(&self) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let (engine, key, files, dir, policy) = (
            self.engine.clone(),
            self.session_key,
            self.files.clone(),
            self.dir.clone(),
            self.policy,
        );
        let outcome = self.outcome.clone();
        self.rt.spawn(async move {
            // The host finds the source folder from the announced id (empty `source_dir`).
            let res = engine
                .start_download_items(
                    key,
                    Some(files.transfer_id),
                    String::new(),
                    files.items,
                    dir,
                    policy,
                )
                .await;
            *lock(&outcome) = Some(res.map_err(|e| e.to_string()));
        });
    }

    fn state(&self) -> StageState {
        match &*lock(&self.outcome) {
            None => StageState::Idle,
            Some(Err(e)) => StageState::Failed(e.clone()),
            Some(Ok(id)) => self
                .engine
                .snapshot(*id)
                .map_or(StageState::Failed("transfer gone".into()), |s| {
                    stage_state(&s)
                }),
        }
    }

    fn cancel(&self) {
        if let Some(Ok(id)) = &*lock(&self.outcome) {
            self.engine.cancel(*id);
        }
    }
}

/// Host: asks the viewer to upload (the pull echo) and watches the upload arrive.
pub struct HostStager {
    engine: Arc<FileTransfer>,
    session_key: u64,
    pull: PullFn,
    started: AtomicBool,
    /// Transfers of the session that existed when the pull was sent.
    before: Mutex<Vec<TransferId>>,
    mine: Mutex<Option<TransferId>>,
}

impl HostStager {
    /// A stager for the announcement of session `session_key`; `pull` is
    /// [`crate::clipfiles::RemoteUpdate::pull`].
    pub fn new(engine: Arc<FileTransfer>, session_key: u64, pull: PullFn) -> Arc<Self> {
        Arc::new(Self {
            engine,
            session_key,
            pull,
            started: AtomicBool::new(false),
            before: Mutex::default(),
            mine: Mutex::default(),
        })
    }

    fn receiving(&self) -> impl Iterator<Item = TransferSnapshot> + '_ {
        self.engine.transfers().into_iter().filter(|s| {
            s.info.session_key == self.session_key && s.info.role == TransferRole::Receiving
        })
    }
}

impl Stager for HostStager {
    fn begin(&self) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        *lock(&self.before) = self.receiving().map(|s| s.info.id).collect();
        (self.pull)();
    }

    fn state(&self) -> StageState {
        let mut mine = lock(&self.mine);
        if mine.is_none() {
            let before = lock(&self.before);
            *mine = self
                .receiving()
                .map(|s| s.info.id)
                .find(|id| !before.contains(id));
        }
        match *mine {
            None => StageState::Idle,
            Some(id) => self
                .engine
                .snapshot(id)
                .map_or(StageState::Failed("transfer gone".into()), |s| {
                    stage_state(&s)
                }),
        }
    }

    fn cancel(&self) {
        if let Some(id) = *lock(&self.mine) {
            self.engine.cancel(id);
        }
    }
}

/// Wires `clip` (announcements), `engine` (transfers) and `shell` (the clipboard) together.
/// Staging folders are created under `root`.
pub fn install(
    clip: &Arc<ClipFilesFeature>,
    engine: &Arc<FileTransfer>,
    shell: Arc<ShellClipboard>,
    policy: ConflictPolicy,
    root: PathBuf,
) {
    super::purge_stale_staging(&root);
    // Host: serves a viewer's pull of the files announced by this PC's own clipboard.
    {
        let clip = clip.clone();
        engine.set_announced_source(Arc::new(move |key, id| {
            let a = clip.announced(key, id)?;
            Some(a.set.source_dir.to_string_lossy().into_owned())
        }));
    }
    // Host: where the viewer's upload for an announced id goes.
    let dests: Arc<Mutex<HashMap<(u64, u64), PathBuf>>> = Arc::default();
    {
        let dests = dests.clone();
        engine.set_announced_dest(Arc::new(move |key, id| {
            lock(&dests)
                .get(&(key, id))
                .map(|p| p.to_string_lossy().into_owned())
        }));
    }

    let (engine_r, shell_r, root_r) = (engine.clone(), shell, root);
    clip.set_remote_handler(Arc::new(move |update| {
        let key = update.session_key;
        let Some(files) = update.files else {
            lock(&dests).retain(|(k, _), _| *k != key);
            shell_r.clear(key);
            return;
        };
        let dir = root_r.join(format!("{key:x}-{:x}", files.transfer_id));
        let stager: Arc<dyn Stager> = match (update.pull, Handle::try_current()) {
            (Some(pull), _) => {
                lock(&dests).insert((key, files.transfer_id), dir.clone());
                HostStager::new(engine_r.clone(), key, pull)
            }
            (None, Ok(rt)) => ViewerStager::new(
                engine_r.clone(),
                rt,
                key,
                files.clone(),
                dir.clone(),
                policy,
            ),
            (None, Err(_)) => return,
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(%e, "cannot create the paste staging folder");
            return;
        }
        shell_r.offer(key, StagedItems::new(dir, files.items, stager));
    }));

    // Viewer: the host's user pasted files this PC announced: send them.
    let engine_u = engine.clone();
    clip.set_upload_handler(Arc::new(move |key, announced: AnnouncedFiles| {
        let Ok(rt) = Handle::try_current() else {
            return;
        };
        let paths: Vec<PathBuf> = announced
            .set
            .items
            .iter()
            .filter(|i| !i.rel_path.contains('/'))
            .map(|i| announced.set.source_dir.join(&i.rel_path))
            .collect();
        let engine = engine_u.clone();
        rt.spawn(async move {
            // An empty destination: the host puts the files in its staging folder.
            if let Err(e) = engine
                .start_upload_with_id(key, announced.transfer_id, paths, String::new())
                .await
            {
                tracing::warn!(%e, "cannot upload the files the host asked for");
            }
        });
    }));
}
