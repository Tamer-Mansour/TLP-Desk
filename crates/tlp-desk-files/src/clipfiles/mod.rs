//! Explorer copy/paste of files between the devices (protocol v4 `ClipboardFiles`).
//!
//! **Host -> viewer** (copy in Explorer on the remote PC, paste on this one): the host watches
//! its clipboard for copied files (CF_HDROP) and, while the session holds `CLIPBOARD` and
//! `FILES`, announces them with [`Message::ClipboardFiles`] under a fresh transfer id (always
//! with [`CLIPBOARD_ID_BIT`] set, so it never collides with the viewer's own transfer ids).
//! An empty item list means "the clipboard no longer holds files". The viewer keeps the latest
//! announcement ([`ClipFilesFeature::remote_files`]) and reports it to the UI as a
//! [`RemoteClipboardFiles`] feature event. Pasting pulls the bytes with a `TransferOffer`
//! (`Download`) carrying the announced id; the host's transfer code finds what to send with
//! [`ClipFilesFeature::announced`].
//!
//! **Viewer -> host** (copy here, paste there): the viewer watches its own clipboard exactly
//! like the host and announces copied files under an id with [`VIEWER_ID_BIT`] set. Only the
//! viewer can start transfers, so when the host's user pastes, the host *echoes* the announcement
//! back (a `ClipboardFiles` with the viewer's id and the items); the viewer answers by
//! uploading the files ([`ClipFilesFeature::set_upload_handler`]) into a staging folder the
//! host chose ([`FileTransfer::set_announced_dest`](crate::FileTransfer::set_announced_dest)).
//!
//! Both sides report the peer's current announcement to a [`RemoteHandler`]
//! ([`ClipFilesFeature::set_remote_handler`]); the app uses it to put the files on the local
//! clipboard as virtual Explorer files ([`crate::shellclip`]), so Ctrl+V in Explorer pastes
//! them. The "Paste N files" button and Ctrl+V in the session window stay as a fallback.

mod items;
mod source;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tlp_desk_net::{Feature, FeatureCtx};
use tlp_desk_proto::{Message, Permissions, Role, TransferItem};
use tokio::sync::mpsc;

pub use items::{build, is_safe_rel_path, CopiedSet, MAX_DEPTH, MAX_ITEMS};
pub use source::{
    local_file_list, system_source, ClipboardFileSource, ManualClipboard, SourceFactory,
};

use source::{Snapshot, Watcher};

/// [`Feature::name`] of clipboard file sharing.
pub const FEATURE_NAME: &str = "clipfiles";

/// Set in every transfer id the host picks for an announcement. Viewer-chosen transfer ids
/// must keep this bit clear.
pub const CLIPBOARD_ID_BIT: u64 = 1 << 63;

/// Set in every transfer id the viewer picks for an announcement. Disjoint from the ids the
/// transfer engine allocates (small counters) and from [`CLIPBOARD_ID_BIT`].
pub const VIEWER_ID_BIT: u64 = 1 << 61;

/// Permissions the session needs for clipboard file sharing.
pub const REQUIRED: Permissions = Permissions::CLIPBOARD.union(Permissions::FILES);

/// Settings of [`ClipFilesFeature`].
#[derive(Clone)]
pub struct ClipFilesConfig {
    /// Clipboard reader (default: the system clipboard).
    pub source: SourceFactory,
    /// How often the host looks at its clipboard while a session is connected.
    pub poll: Duration,
}

impl Default for ClipFilesConfig {
    fn default() -> Self {
        Self {
            source: system_source(),
            poll: Duration::from_millis(750),
        }
    }
}

impl std::fmt::Debug for ClipFilesConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipFilesConfig")
            .field("poll", &self.poll)
            .finish_non_exhaustive()
    }
}

/// Host side: a set announced to the viewer, as the transfer code needs it to serve the paste.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnouncedFiles {
    /// The id the viewer will use in its `TransferOffer`.
    pub transfer_id: u64,
    /// The announced items and the folder they are relative to.
    pub set: CopiedSet,
}

/// Viewer side: files copied on the remote PC, ready to paste. Also the payload of this
/// feature's `FeatureEvent`s (an empty `items` = nothing to paste any more).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteClipboardFiles {
    /// Id to put in the `TransferOffer` that pulls these files.
    pub transfer_id: u64,
    /// The copied files and folders (relative, `/`-separated, validated).
    pub items: Vec<TransferItem>,
}

impl RemoteClipboardFiles {
    /// Number of top-level entries (what the user copied, not counting folder contents).
    pub fn top_level_count(&self) -> usize {
        self.items
            .iter()
            .filter(|i| !i.rel_path.contains('/'))
            .count()
    }

    /// Sum of all file sizes.
    pub fn total_bytes(&self) -> u64 {
        self.items
            .iter()
            .fold(0u64, |acc, i| acc.saturating_add(i.size))
    }
}

/// What a [`RemoteHandler`] is told when the peer's clipboard files change.
#[derive(Clone)]
pub struct RemoteUpdate {
    /// The session (host: `SessionId`, viewer: the viewer feature key).
    pub session_key: u64,
    /// Our role in the session.
    pub role: Role,
    /// The peer's files, or `None` when there is nothing to paste any more (cleared, permission
    /// revoked, session ended).
    pub files: Option<RemoteClipboardFiles>,
    /// Host only: asks the viewer to upload the announced files (the host's pull; idempotent
    /// on the viewer side because it only answers for ids it announced).
    pub pull: Option<PullFn>,
}

impl std::fmt::Debug for RemoteUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteUpdate")
            .field("session_key", &self.session_key)
            .field("role", &self.role)
            .field("files", &self.files)
            .finish_non_exhaustive()
    }
}

/// Sends the pull request of a host (see [`RemoteUpdate::pull`]).
pub type PullFn = Arc<dyn Fn() + Send + Sync>;
/// Called on every change of the peer's announced files.
pub type RemoteHandler = Arc<dyn Fn(RemoteUpdate) + Send + Sync>;
/// Viewer: called when the host asks for files this PC announced: `(session_key, files)`.
pub type UploadHandler = Arc<dyn Fn(u64, AnnouncedFiles) + Send + Sync>;

/// The clipboard file sharing feature. Register one instance for host and viewer sessions.
pub struct ClipFilesFeature {
    shared: Arc<Shared>,
}

struct Shared {
    watcher: Watcher,
    next_id: AtomicU64,
    announced: Mutex<HashMap<u64, AnnouncedFiles>>,
    remote: Mutex<HashMap<u64, RemoteClipboardFiles>>,
    remote_handler: Mutex<Option<RemoteHandler>>,
    upload_handler: Mutex<Option<UploadHandler>>,
}

impl std::fmt::Debug for ClipFilesFeature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipFilesFeature").finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ClipFilesFeature {
    /// A new feature with `cfg`.
    pub fn new(cfg: ClipFilesConfig) -> Arc<Self> {
        Arc::new(Self {
            shared: Arc::new(Shared {
                watcher: Watcher::new(cfg.source, cfg.poll),
                next_id: AtomicU64::new(1),
                announced: Mutex::default(),
                remote: Mutex::default(),
                remote_handler: Mutex::default(),
                upload_handler: Mutex::default(),
            }),
        })
    }

    /// Registers the receiver of the peer's announcements (both roles).
    pub fn set_remote_handler(&self, handler: RemoteHandler) {
        *lock(&self.shared.remote_handler) = Some(handler);
    }

    /// Viewer: registers what to do when the host's user pastes files this PC announced.
    pub fn set_upload_handler(&self, handler: UploadHandler) {
        *lock(&self.shared.upload_handler) = Some(handler);
    }

    /// Host: the set announced in session `session_key` under `transfer_id`, if it is the
    /// latest one (older announcements are forgotten when the clipboard changes).
    pub fn announced(&self, session_key: u64, transfer_id: u64) -> Option<AnnouncedFiles> {
        lock(&self.shared.announced)
            .get(&session_key)
            .filter(|a| a.transfer_id == transfer_id)
            .cloned()
    }

    /// What the peer currently has on its clipboard, if files.
    pub fn remote_files(&self, session_key: u64) -> Option<RemoteClipboardFiles> {
        lock(&self.shared.remote).get(&session_key).cloned()
    }
}

impl Shared {
    fn next_transfer_id(&self, role: Role) -> u64 {
        let bit = match role {
            Role::Host => CLIPBOARD_ID_BIT,
            Role::Viewer => VIEWER_ID_BIT,
        };
        bit | (self.next_id.fetch_add(1, Ordering::Relaxed) & (VIEWER_ID_BIT - 1))
    }

    fn notify(&self, update: RemoteUpdate) {
        let handler = lock(&self.remote_handler).clone();
        if let Some(h) = handler {
            h(update);
        }
    }
}

impl Feature for ClipFilesFeature {
    fn name(&self) -> &'static str {
        FEATURE_NAME
    }

    fn handles(&self, msg: &Message) -> bool {
        matches!(msg, Message::ClipboardFiles { .. })
    }

    fn start(&self, ctx: FeatureCtx, inbox: mpsc::UnboundedReceiver<Message>) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            tracing::error!("clipfiles feature started outside a tokio runtime; disabled");
            return;
        };
        rt.spawn(peer_loop(self.shared.clone(), ctx, inbox));
    }
}

/// Both roles: announce local clipboard changes and track the peer's announcements until the
/// session ends.
async fn peer_loop(me: Arc<Shared>, ctx: FeatureCtx, mut inbox: mpsc::UnboundedReceiver<Message>) {
    let mut clip = me.watcher.subscribe();
    let mut perms = ctx.permissions.clone();
    let mut current = clip.borrow_and_update().clone();
    // The list the peer currently holds from us (`None` = nothing to paste).
    let mut shown: Option<Snapshot> = None;
    loop {
        let target = (ctx.allowed(REQUIRED) && !current.is_empty()).then(|| current.clone());
        if target.as_deref() != shown.as_deref() {
            let paths = target.as_deref().map_or(&[][..], Vec::as_slice);
            match announce(&me, &ctx, paths).await {
                Ok(true) => shown = target,
                Ok(false) => shown = None,
                Err(()) => break,
            }
        }
        if !ctx.allowed(REQUIRED) {
            // Without the permission a paste would be refused: forget the peer's offer.
            clear_remote(&me, &ctx);
        }
        tokio::select! {
            changed = clip.changed() => {
                if changed.is_err() {
                    break;
                }
                current = clip.borrow_and_update().clone();
            }
            // Re-evaluated at the top: a revoke clears both directions, a grant shows what is
            // copied right now.
            changed = perms.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            msg = inbox.recv() => match msg {
                Some(Message::ClipboardFiles { transfer_id, items }) => {
                    on_peer_message(&me, &ctx, transfer_id, items);
                }
                Some(_) => {}
                None => break,
            },
        }
    }
    lock(&me.announced).remove(&ctx.session_key);
    clear_remote(&me, &ctx);
}

/// A `ClipboardFiles` from the peer: either the host's pull request for files we announced
/// (viewer only) or the peer's new announcement.
fn on_peer_message(me: &Shared, ctx: &FeatureCtx, transfer_id: u64, items: Vec<TransferItem>) {
    if ctx.role == Role::Viewer && transfer_id & CLIPBOARD_ID_BIT == 0 {
        // The host's user pasted files we announced: upload them. Only ids we announced are
        // honoured and the files come from our own announcement, never from the message.
        if items.is_empty() || !ctx.allowed(REQUIRED) {
            return;
        }
        let Some(files) = lock(&me.announced)
            .get(&ctx.session_key)
            .filter(|a| a.transfer_id == transfer_id)
            .cloned()
        else {
            tracing::debug!(transfer_id, "pull request for files we no longer announce");
            return;
        };
        let handler = lock(&me.upload_handler).clone();
        if let Some(h) = handler {
            h(ctx.session_key, files);
        }
        return;
    }
    if items.is_empty() {
        clear_remote(me, ctx);
        return;
    }
    if !ctx.allowed(REQUIRED) {
        tracing::debug!("clipboard files without CLIPBOARD+FILES; ignored");
        return;
    }
    if items.len() > MAX_ITEMS || !items.iter().all(|i| is_safe_rel_path(&i.rel_path)) {
        tracing::warn!(
            transfer_id,
            "peer announced unsafe clipboard file paths; ignored"
        );
        return;
    }
    let files = RemoteClipboardFiles { transfer_id, items };
    lock(&me.remote).insert(ctx.session_key, files.clone());
    let pull = (ctx.role == Role::Host).then(|| pull_fn(ctx, &files));
    me.notify(RemoteUpdate {
        session_key: ctx.session_key,
        role: ctx.role,
        files: Some(files.clone()),
        pull,
    });
    if ctx.role == Role::Viewer {
        ctx.emit(FEATURE_NAME, files);
    }
}

/// The host's pull: echoes the announcement so the viewer uploads the files.
fn pull_fn(ctx: &FeatureCtx, files: &RemoteClipboardFiles) -> PullFn {
    let session = ctx.session.clone();
    let msg = Message::ClipboardFiles {
        transfer_id: files.transfer_id,
        items: files.items.clone(),
    };
    let rt = tokio::runtime::Handle::try_current().ok();
    Arc::new(move || {
        let Some(rt) = &rt else { return };
        let (session, msg) = (session.clone(), msg.clone());
        rt.spawn(async move {
            if let Err(e) = session.send(&msg).await {
                tracing::debug!(%e, "cannot ask the viewer for the copied files");
            }
        });
    })
}

/// Sends the announcement for `paths` (empty = clear). `Ok(true)` if the peer now holds a
/// non-empty set, `Err` if the session is gone.
async fn announce(me: &Shared, ctx: &FeatureCtx, paths: &[PathBuf]) -> Result<bool, ()> {
    let set = if paths.is_empty() {
        None
    } else {
        let paths = paths.to_vec();
        tokio::task::spawn_blocking(move || build(&paths))
            .await
            .ok()
            .flatten()
    };
    let transfer_id = me.next_transfer_id(ctx.role);
    let items = set.as_ref().map(|s| s.items.clone()).unwrap_or_default();
    let shown = !items.is_empty();
    {
        let mut announced = lock(&me.announced);
        match set {
            Some(set) => {
                announced.insert(ctx.session_key, AnnouncedFiles { transfer_id, set });
            }
            None => {
                announced.remove(&ctx.session_key);
            }
        }
    }
    tracing::info!(
        session = ctx.session_key,
        transfer_id,
        items = items.len(),
        "announcing clipboard files"
    );
    let msg = Message::ClipboardFiles { transfer_id, items };
    ctx.session.send(&msg).await.map_err(|e| {
        tracing::debug!(%e, "cannot announce clipboard files");
    })?;
    Ok(shown)
}

/// Nothing to paste from the peer any more; tells the UI (viewer) and the handler.
fn clear_remote(me: &Shared, ctx: &FeatureCtx) {
    if let Some(old) = lock(&me.remote).remove(&ctx.session_key) {
        if ctx.role == Role::Viewer {
            ctx.emit(
                FEATURE_NAME,
                RemoteClipboardFiles {
                    transfer_id: old.transfer_id,
                    items: Vec::new(),
                },
            );
        }
        me.notify(RemoteUpdate {
            session_key: ctx.session_key,
            role: ctx.role,
            files: None,
            pull: None,
        });
    }
}
