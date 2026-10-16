//! Fast file and folder transfer between the viewer and the host.
//!
//! # Using it
//! Create one [`FileTransfer`] per process, register it in `HostConfig::features` and
//! `ViewerConfig::features` (it is a `tlp_desk_net::Feature`), then on the viewer call
//! [`FileTransfer::start_upload`] / [`FileTransfer::start_download`] with the session's
//! feature key. Progress arrives as [`TransferEvent`]s, both as the payload of
//! `FeatureEvent`s named [`FEATURE_NAME`] and on [`FileTransfer::subscribe`]. The host accepts
//! offers automatically while the session holds `FILES`; revoking `FILES` stops every transfer
//! of the session at once and further data streams are refused.
//!
//! # Wire usage (protocol v4)
//! * Control: `TransferOffer` (viewer) -> `TransferAccept { resume }` / `TransferReject`;
//!   `TransferDone { item, blake3 }` (sender, per file); `TransferProgress` (receiver, ~4/s);
//!   `TransferCancel` / `TransferError` (either). The receiver never reports
//!   `bytes_done == total_bytes` before every file is verified, then reports it exactly once:
//!   that is the sender's "complete" signal.
//! * Data: one `StreamKind::FileData` uni stream per 16 MiB piece (`FileDataHeader` + bytes),
//!   several in parallel. Resume offsets are piece-aligned.
//! * Listing a download selection: `FsRequest`/`FsResponse` with `Stat`/`ListDir` and request
//!   ids that have [`TRANSFER_REQ_ID_BIT`] set; this feature answers those on the host, the
//!   file-browser feature keeps every other id.
//!
//! # Safety on the receiving side
//! Every relative path is validated ([`validate_rel_path`]): no `..`, absolute paths, `\`,
//! `:` (drives, alternate data streams), device names or trailing dots/spaces; the final path
//! must resolve inside the chosen destination even through junctions. Files are written to
//! `name.tlpdesk-part`, verified with BLAKE3 and renamed into place, so a file with its real
//! name is always complete.

mod conflict;
mod disk;
mod error;
mod hash;
mod path;
mod plan;
mod receiver;
mod scan;
mod sender;
mod session;
mod state;
mod types;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use tlp_desk_net::{Feature, FeatureCtx};
use tlp_desk_proto::{
    FsOp, FsReply, Message, Permissions, Role, TransferDirection, TransferItem, TransferOffer,
};
use tokio::sync::{broadcast, mpsc, oneshot};

pub use conflict::{
    detect_conflicts, ConflictChoice, ConflictRequest, ConflictResolution, FileConflict,
    MAX_LISTED_CONFLICTS,
};
pub use error::{FilesError, PathIssue};
pub use hash::PIECE;
pub use path::{display_path, validate_component, validate_rel_path, MAX_DEPTH};
pub use plan::{Decisions, META_SUFFIX, PART_SUFFIX};
pub use types::{
    ConflictPolicy, FileRow, FileState, TransferConfig, TransferEvent, TransferId, TransferInfo,
    TransferRole, TransferSnapshot, TransferState,
};

use receiver::Receiving;
use sender::Sending;
use session::SessionState;
use state::{lock, Side, Transfer};

/// [`Feature::name`] of the transfer engine; UI events carry it in `FeatureEvent::feature`.
pub const FEATURE_NAME: &str = "files-transfer";

/// `FsRequest` / `FsResponse` ids with this bit belong to the transfer engine.
pub const TRANSFER_REQ_ID_BIT: u64 = 1 << 62;

/// Hooks for tests; they never weaken authentication or permissions.
#[derive(Default)]
pub(crate) struct TestHooks {
    /// Flip one byte of the next data buffer after it was hashed.
    pub corrupt_next: AtomicBool,
    /// Limit sending to this many bytes per second (0 = no limit).
    pub rate_limit: AtomicU64,
}

/// What [`FileTransfer::resume`] needs to start a transfer again.
#[derive(Clone)]
enum Spec {
    Upload {
        local: Vec<PathBuf>,
        remote_dest: String,
    },
    Download {
        source_dir: String,
        items: Vec<TransferItem>,
        local_dest: PathBuf,
        policy: ConflictPolicy,
    },
}

/// What a transfer started on this side was asked to copy (for "send again").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferOrigin {
    /// `true` for an upload (local files to the host), `false` for a download.
    pub upload: bool,
    /// Uploads: the local files and folders that were sent.
    pub local_paths: Vec<PathBuf>,
    /// Uploads: the host folder they went to (empty = the host's default folder).
    pub remote_dest: String,
    /// Downloads: the host files and folders that were fetched.
    pub remote_paths: Vec<String>,
    /// Downloads: the local folder they went to.
    pub local_dest: Option<PathBuf>,
}

/// Engine state shared by all sessions.
pub(crate) struct Inner {
    pub cfg: TransferConfig,
    sessions: Mutex<HashMap<u64, Arc<SessionState>>>,
    next_id: AtomicU64,
    pub events: broadcast::Sender<(u64, TransferEvent)>,
    pub hooks: TestHooks,
    specs: Mutex<HashMap<TransferId, (u64, Spec)>>,
    /// Open [`ConflictPolicy::Ask`] prompts.
    pub(crate) pending: conflict::Pending,
    /// Policy for uploads from viewers, changeable while running (starts as `cfg.conflict`).
    incoming_conflict: AtomicU8,
    /// Host: finds the source folder of an offer that names none (a paste of files the user
    /// copied in Explorer), from `(session_key, offer id)`.
    pub announced_source: Mutex<Option<SourceLookup>>,
    /// Host: where an upload offer with an empty `dest_dir` goes when its id was announced by
    /// the viewer's Explorer copy (the staging folder of the virtual clipboard files).
    pub announced_dest: Mutex<Option<SourceLookup>>,
}

/// See [`FileTransfer::set_announced_source`].
pub type SourceLookup = Arc<dyn Fn(u64, u64) -> Option<String> + Send + Sync>;

impl Inner {
    /// A new process-unique id (also used as wire id for offers we send).
    pub fn alloc_id(&self) -> TransferId {
        TransferId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// The policy for files a viewer sends to this host right now.
    pub fn incoming_conflict(&self) -> ConflictPolicy {
        ConflictPolicy::from_u8(self.incoming_conflict.load(Ordering::Relaxed))
    }

    pub fn remove_session(&self, key: u64) {
        lock(&self.sessions).remove(&key);
    }
}

/// The file-transfer feature (see the module docs). Cheap to share: wrap it in an `Arc` and
/// register the same instance for host and viewer sessions.
pub struct FileTransfer {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for FileTransfer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileTransfer")
            .field("cfg", &self.inner.cfg)
            .finish_non_exhaustive()
    }
}

impl FileTransfer {
    /// Creates the engine.
    pub fn new(cfg: TransferConfig) -> Arc<Self> {
        let (events, _) = broadcast::channel(1024);
        let incoming = cfg.conflict as u8;
        Arc::new(Self {
            inner: Arc::new(Inner {
                cfg,
                sessions: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                events,
                hooks: TestHooks::default(),
                specs: Mutex::new(HashMap::new()),
                pending: conflict::Pending::default(),
                incoming_conflict: AtomicU8::new(incoming),
                announced_source: Mutex::new(None),
                announced_dest: Mutex::new(None),
            }),
        })
    }

    /// Host: lets viewers download files by an id announced elsewhere (Explorer copy/paste).
    /// `lookup(session_key, offer_id)` returns the folder the announced items are relative to;
    /// it is consulted only for download offers with an empty `source_dir`.
    pub fn set_announced_source(&self, lookup: SourceLookup) {
        *lock(&self.inner.announced_source) = Some(lookup);
    }

    /// Host: lets the viewer upload files its Explorer copy announced into a folder chosen by
    /// this PC. `lookup(session_key, offer_id)` returns the destination folder; it is consulted
    /// only for upload offers with an empty `dest_dir`.
    pub fn set_announced_dest(&self, lookup: SourceLookup) {
        *lock(&self.inner.announced_dest) = Some(lookup);
    }

    /// Every event of every session, as `(session_key, event)`.
    pub fn subscribe(&self) -> broadcast::Receiver<(u64, TransferEvent)> {
        self.inner.events.subscribe()
    }

    /// All transfers of live sessions (running and finished).
    pub fn transfers(&self) -> Vec<TransferSnapshot> {
        let sessions: Vec<_> = lock(&self.inner.sessions).values().cloned().collect();
        let mut out: Vec<_> = sessions
            .iter()
            .flat_map(|s| {
                lock(&s.transfers)
                    .values()
                    .map(|t| t.snapshot())
                    .collect::<Vec<_>>()
            })
            .collect();
        out.sort_by_key(|s| s.info.id);
        out
    }

    /// The files of a transfer (folders are left out), `limit` of them from the `offset`-th,
    /// with their progress. Empty if the transfer is unknown or its session ended.
    pub fn file_rows(&self, id: TransferId, offset: usize, limit: usize) -> Vec<FileRow> {
        self.find(id)
            .map(|(_, t)| t.file_rows(offset, limit))
            .unwrap_or_default()
    }

    /// What a transfer started on this side copies, even after its session ended.
    pub fn origin(&self, id: TransferId) -> Option<TransferOrigin> {
        let (_, spec) = lock(&self.inner.specs).get(&id).cloned()?;
        Some(match spec {
            Spec::Upload { local, remote_dest } => TransferOrigin {
                upload: true,
                local_paths: local,
                remote_dest,
                remote_paths: Vec::new(),
                local_dest: None,
            },
            Spec::Download {
                source_dir,
                items,
                local_dest,
                ..
            } => TransferOrigin {
                upload: false,
                local_paths: Vec::new(),
                remote_dest: String::new(),
                remote_paths: items
                    .iter()
                    .filter(|i| !i.rel_path.contains('/'))
                    .map(|i| scan::join_remote(&source_dir, &i.rel_path))
                    .collect(),
                local_dest: Some(local_dest),
            },
        })
    }

    /// One transfer, if its session is still live.
    pub fn snapshot(&self, id: TransferId) -> Option<TransferSnapshot> {
        self.find(id).map(|(_, t)| t.snapshot())
    }

    /// Whether a session with this key is running the feature.
    pub fn has_session(&self, session_key: u64) -> bool {
        lock(&self.inner.sessions).contains_key(&session_key)
    }

    fn session(&self, key: u64) -> Result<Arc<SessionState>, FilesError> {
        lock(&self.inner.sessions)
            .get(&key)
            .cloned()
            .ok_or(FilesError::NoSession(key))
    }

    fn find(&self, id: TransferId) -> Option<(Arc<SessionState>, Arc<Transfer>)> {
        let sessions: Vec<_> = lock(&self.inner.sessions).values().cloned().collect();
        sessions
            .into_iter()
            .find_map(|s| s.find(id).map(|t| (s.clone(), t)))
    }

    /// Viewer: sends local files and folders (recursively) into `remote_dest` on the host
    /// (an absolute host path; empty = the host's default folder, usually its Desktop).
    /// Returns once the offer is sent; progress follows as events.
    ///
    /// # Errors
    /// No such session, not a viewer, no `FILES` permission, unreadable or untransferable
    /// local paths.
    pub async fn start_upload(
        &self,
        session_key: u64,
        local_paths: Vec<PathBuf>,
        remote_dest: String,
    ) -> Result<TransferId, FilesError> {
        let spec = Spec::Upload {
            local: local_paths,
            remote_dest,
        };
        self.start(session_key, spec, None, None).await
    }

    /// Viewer: like [`FileTransfer::start_upload`] with a chosen wire id (e.g. the id a
    /// `ClipboardFiles` announcement used).
    ///
    /// # Errors
    /// See [`FileTransfer::start_upload`].
    pub async fn start_upload_with_id(
        &self,
        session_key: u64,
        wire_id: u64,
        local_paths: Vec<PathBuf>,
        remote_dest: String,
    ) -> Result<TransferId, FilesError> {
        let spec = Spec::Upload {
            local: local_paths,
            remote_dest,
        };
        self.start(session_key, spec, None, Some(wire_id)).await
    }

    /// Viewer: copies host files and folders (absolute host paths, all in one folder) into
    /// the local folder `local_dest`, using [`TransferConfig::download_conflict`].
    ///
    /// # Errors
    /// No such session, not a viewer, no `FILES`, remote paths that do not exist or do not
    /// share one parent folder, an invalid local destination.
    pub async fn start_download(
        &self,
        session_key: u64,
        remote_paths: Vec<String>,
        local_dest: PathBuf,
    ) -> Result<TransferId, FilesError> {
        let policy = self.inner.cfg.download_conflict;
        let s = self.session(session_key)?;
        check_viewer(&s)?;
        let (source_dir, items) = list_remote(&s, &remote_paths, self.inner.cfg.max_items).await?;
        let spec = Spec::Download {
            source_dir,
            items,
            local_dest,
            policy,
        };
        self.start(session_key, spec, None, None).await
    }

    /// Viewer: downloads already-listed items (e.g. from a `ClipboardFiles` announcement)
    /// from `source_dir` on the host into `local_dest`, under `wire_id` if given.
    ///
    /// # Errors
    /// See [`FileTransfer::start_download`].
    pub async fn start_download_items(
        &self,
        session_key: u64,
        wire_id: Option<u64>,
        source_dir: String,
        items: Vec<TransferItem>,
        local_dest: PathBuf,
        policy: ConflictPolicy,
    ) -> Result<TransferId, FilesError> {
        let spec = Spec::Download {
            source_dir,
            items,
            local_dest,
            policy,
        };
        self.start(session_key, spec, None, wire_id).await
    }

    /// Changes what happens to files a viewer sends that already exist here, for uploads that
    /// start from now on (the host's "ask before replacing" setting).
    pub fn set_incoming_conflict(&self, policy: ConflictPolicy) {
        self.inner
            .incoming_conflict
            .store(policy as u8, Ordering::Relaxed);
    }

    /// Answers a [`TransferEvent::ConflictsFound`] prompt: `Some(resolution)` continues the
    /// transfer accordingly, `None` cancels it (on the host the viewer's upload is declined).
    /// `false` if no prompt waits for `id` (already answered, timed out, session ended).
    pub fn resolve_conflicts(&self, id: TransferId, answer: Option<ConflictResolution>) -> bool {
        self.inner.pending.answer(id, answer)
    }

    /// Cancels a transfer (either side). Partial files are kept so a new transfer of the same
    /// files resumes. `false` if unknown or already finished.
    pub fn cancel(&self, id: TransferId) -> bool {
        self.stop(id, TransferState::Cancelled)
    }

    /// Pauses a transfer started on this side; [`FileTransfer::resume`] continues it.
    pub fn pause(&self, id: TransferId) -> bool {
        self.stop(id, TransferState::Paused)
    }

    fn stop(&self, id: TransferId, state: TransferState) -> bool {
        match self.find(id) {
            Some((s, t)) if !t.state().is_final() => {
                s.abort(&t, state);
                true
            }
            _ => false,
        }
    }

    /// Starts a paused, cancelled, failed or disconnected transfer again in `session_key`
    /// (the same or a new session to the same host). Already transferred files and pieces are
    /// not sent again. Only transfers started on this side can be resumed.
    ///
    /// # Errors
    /// Unknown id, still running, or the errors of starting a transfer.
    pub async fn resume(&self, id: TransferId, session_key: u64) -> Result<(), FilesError> {
        if let Some((_, t)) = self.find(id) {
            if !t.state().is_final() {
                return Err(FilesError::Protocol("the transfer is still running".into()));
            }
        }
        let (_, spec) = lock(&self.inner.specs)
            .get(&id)
            .cloned()
            .ok_or(FilesError::NoTransfer(id.0))?;
        if let Some((s, t)) = self.find(id) {
            lock(&s.transfers).remove(&t.wire);
        }
        self.start(session_key, spec, Some(id), None)
            .await
            .map(|_| ())
    }

    async fn start(
        &self,
        session_key: u64,
        spec: Spec,
        reuse: Option<TransferId>,
        wire: Option<u64>,
    ) -> Result<TransferId, FilesError> {
        let s = self.session(session_key)?;
        check_viewer(&s)?;
        let id = reuse.unwrap_or_else(|| self.inner.alloc_id());
        let wire = wire.unwrap_or_else(|| self.inner.alloc_id().0);
        lock(&self.inner.specs).insert(id, (session_key, spec.clone()));
        match spec {
            Spec::Upload { local, remote_dest } => upload(&s, id, wire, local, remote_dest).await,
            Spec::Download {
                source_dir,
                items,
                local_dest,
                policy,
            } => download(&s, id, wire, source_dir, items, local_dest, policy).await,
        }?;
        Ok(id)
    }

    /// Test hook: corrupts the next data buffer sent (after hashing), to exercise hash checks.
    #[doc(hidden)]
    pub fn test_corrupt_next_buffer(&self) {
        self.inner.hooks.corrupt_next.store(true, Ordering::Release);
    }

    /// Test hook: limits sending to `bytes_per_sec` (0 = unlimited).
    #[doc(hidden)]
    pub fn test_set_rate_limit(&self, bytes_per_sec: u64) {
        self.inner
            .hooks
            .rate_limit
            .store(bytes_per_sec, Ordering::Release);
    }
}

fn check_viewer(s: &SessionState) -> Result<(), FilesError> {
    if s.ctx.role != Role::Viewer {
        return Err(FilesError::Protocol(
            "only the viewer starts transfers".into(),
        ));
    }
    if !s.files_allowed() {
        return Err(FilesError::PermissionDenied);
    }
    Ok(())
}

fn offer_of(
    wire: u64,
    direction: TransferDirection,
    source_dir: String,
    dest_dir: String,
    items: Vec<TransferItem>,
    resume: Vec<(u32, u64)>,
) -> TransferOffer {
    let total_bytes = items.iter().filter(|i| !i.is_dir).map(|i| i.size).sum();
    TransferOffer {
        id: wire,
        direction,
        source_dir,
        dest_dir,
        items,
        total_bytes,
        resume,
    }
}

async fn upload(
    s: &Arc<SessionState>,
    id: TransferId,
    wire: u64,
    local: Vec<PathBuf>,
    remote_dest: String,
) -> Result<(), FilesError> {
    let max = s.inner.cfg.max_items;
    let sel = tokio::task::spawn_blocking(move || scan::collect_local(&local, max))
        .await
        .map_err(|_| FilesError::Protocol("listing task failed".into()))??;
    let offer = offer_of(
        wire,
        TransferDirection::Upload,
        sel.source_dir.clone(),
        remote_dest.clone(),
        sel.items.clone(),
        Vec::new(),
    );
    let info = session::info_for(s, id, &offer, TransferRole::Sending, true, remote_dest);
    let (tx, rx) = oneshot::channel();
    let paths = sel.paths.into_iter().map(Some).collect();
    let sending = Arc::new(Sending::new(sel.items, paths, Some(tx)));
    let t = Arc::new(Transfer::new(
        id,
        wire,
        info,
        None,
        Side::Send(sending.clone()),
    ));
    s.register(t.clone())?;
    s.send(Message::TransferOffer(offer));
    let st = s.clone();
    tokio::spawn(async move {
        let answer = tokio::select! {
            a = rx => a,
            () = t.wait_stop() => return,
        };
        match answer {
            Ok(Ok(resume)) => {
                t.set_running();
                sender::spawn(st, t, sending, resume);
            }
            Ok(Err(e)) => st.end(&t, TransferState::Failed(e)),
            Err(_) => st.end(&t, TransferState::Disconnected),
        }
    });
    Ok(())
}

async fn download(
    s: &Arc<SessionState>,
    id: TransferId,
    wire: u64,
    source_dir: String,
    items: Vec<TransferItem>,
    local_dest: PathBuf,
    policy: ConflictPolicy,
) -> Result<(), FilesError> {
    let dest = local_dest.clone();
    let plan_items = items.clone();
    let decisions = conflict::decide(s, id, &local_dest, &items, policy, true).await?;
    let rx = tokio::task::spawn_blocking(move || Receiving::prepare(&dest, plan_items, &decisions))
        .await
        .map_err(|_| FilesError::Protocol("planning task failed".into()))??;
    let offer = offer_of(
        wire,
        TransferDirection::Download,
        source_dir,
        path::display_path(&rx.root),
        items,
        rx.resume.clone(),
    );
    let info = session::info_for(
        s,
        id,
        &offer,
        TransferRole::Receiving,
        true,
        path::display_path(&rx.root),
    );
    let rx = Arc::new(rx);
    let t = Arc::new(Transfer::new(
        id,
        wire,
        info,
        Some(rx.root.clone()),
        Side::Recv(rx.clone()),
    ));
    rx.init_progress(&t);
    s.register(t.clone())?;
    s.send(Message::TransferOffer(offer));
    t.set_running();
    Ok(())
}

/// Lists a remote selection (files and folders sharing one parent) into transfer items.
async fn list_remote(
    s: &SessionState,
    remote: &[String],
    max_items: usize,
) -> Result<(String, Vec<TransferItem>), FilesError> {
    let mut parent: Option<String> = None;
    let mut items = Vec::new();
    for p in remote {
        let bad = |issue| FilesError::InvalidPath {
            path: p.clone(),
            issue,
        };
        let (dir, name) = scan::split_remote(p).ok_or_else(|| bad(PathIssue::Empty))?;
        validate_component(name).map_err(bad)?;
        match &parent {
            None => parent = Some(dir.to_string()),
            Some(d) if path::same_name(d, dir) => {}
            Some(_) => return Err(bad(PathIssue::Escapes)),
        }
        let FsReply::Entry(e) = s.fs_request(FsOp::Stat { path: p.clone() }).await? else {
            return Err(FilesError::Protocol("unexpected stat reply".into()));
        };
        items.push(TransferItem {
            rel_path: name.to_string(),
            size: if e.is_dir { 0 } else { e.size },
            modified_unix_ms: e.modified_unix_ms,
            is_dir: e.is_dir,
        });
        if e.is_dir {
            list_remote_dir(s, p, name, &mut items, max_items).await?;
        }
    }
    Ok((parent.unwrap_or_default(), items))
}

/// Breadth-first remote listing with depth and count limits.
async fn list_remote_dir(
    s: &SessionState,
    remote_dir: &str,
    rel: &str,
    items: &mut Vec<TransferItem>,
    max_items: usize,
) -> Result<(), FilesError> {
    let mut queue =
        std::collections::VecDeque::from([(remote_dir.to_string(), rel.to_string(), 1usize)]);
    while let Some((dir, rel, depth)) = queue.pop_front() {
        let FsReply::Entries(mut entries) =
            s.fs_request(FsOp::ListDir { path: dir.clone() }).await?
        else {
            return Err(FilesError::Protocol("unexpected list reply".into()));
        };
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        for e in entries {
            if validate_component(&e.name).is_err() {
                tracing::warn!("skipping a remote file whose name cannot be transferred");
                continue;
            }
            if items.len() >= max_items {
                return Err(FilesError::TooManyItems {
                    count: items.len() + 1,
                    max: max_items,
                });
            }
            let child_rel = format!("{rel}/{}", e.name);
            if e.is_dir {
                if depth + 1 >= MAX_DEPTH {
                    return Err(FilesError::InvalidPath {
                        path: child_rel,
                        issue: PathIssue::TooDeep,
                    });
                }
                queue.push_back((
                    scan::join_remote(&dir, &e.name),
                    child_rel.clone(),
                    depth + 1,
                ));
            }
            items.push(TransferItem {
                rel_path: child_rel,
                size: if e.is_dir { 0 } else { e.size },
                modified_unix_ms: e.modified_unix_ms,
                is_dir: e.is_dir,
            });
        }
    }
    Ok(())
}

impl Feature for FileTransfer {
    fn name(&self) -> &'static str {
        FEATURE_NAME
    }

    fn handles(&self, msg: &Message) -> bool {
        match msg {
            Message::TransferOffer(_)
            | Message::TransferAccept { .. }
            | Message::TransferReject { .. }
            | Message::TransferProgress { .. }
            | Message::TransferCancel { .. }
            | Message::TransferDone { .. }
            | Message::TransferError { .. } => true,
            Message::FsRequest { req_id, .. } | Message::FsResponse { req_id, .. } => {
                req_id & TRANSFER_REQ_ID_BIT != 0
            }
            _ => false,
        }
    }

    fn start(&self, ctx: FeatureCtx, inbox: mpsc::UnboundedReceiver<Message>) {
        let key = ctx.session_key;
        let state = SessionState::new(ctx, self.inner.clone());
        lock(&self.inner.sessions).insert(key, state.clone());
        tokio::spawn(session::run(state, inbox));
    }
}

/// Benchmark aid: the piece-parallel BLAKE3 of a file as the engine computes it from disk.
#[doc(hidden)]
pub fn hash_file_for_bench(file: &std::fs::File, size: u64) -> [u8; 32] {
    hash::hash_file(file, size).unwrap_or_default()
}

/// `true` if `perms` allow file transfers.
pub fn files_permitted(perms: Permissions) -> bool {
    perms.contains(Permissions::FILES)
}
