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
//! **Viewer -> host** (copy here, paste there): only the viewer may start transfers, so the app
//! reads [`local_file_list`] and uploads those files into a folder of the remote PC.
//!
//! Limitation: the pasted files are not offered to Explorer as a "virtual" clipboard entry
//! (that needs delayed rendering through COM `IDataObject`); the user pastes with the app's
//! "paste files from the remote PC" action instead, which downloads into a chosen folder.

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

/// The clipboard file sharing feature. Register one instance for host and viewer sessions.
pub struct ClipFilesFeature {
    shared: Arc<Shared>,
}

struct Shared {
    watcher: Watcher,
    next_id: AtomicU64,
    announced: Mutex<HashMap<u64, AnnouncedFiles>>,
    remote: Mutex<HashMap<u64, RemoteClipboardFiles>>,
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
            }),
        })
    }

    /// Host: the set announced in session `session_key` under `transfer_id`, if it is the
    /// latest one (older announcements are forgotten when the clipboard changes).
    pub fn announced(&self, session_key: u64, transfer_id: u64) -> Option<AnnouncedFiles> {
        lock(&self.shared.announced)
            .get(&session_key)
            .filter(|a| a.transfer_id == transfer_id)
            .cloned()
    }

    /// Viewer: what the remote PC currently has on its clipboard, if files.
    pub fn remote_files(&self, session_key: u64) -> Option<RemoteClipboardFiles> {
        lock(&self.shared.remote).get(&session_key).cloned()
    }
}

impl Shared {
    fn next_transfer_id(&self) -> u64 {
        CLIPBOARD_ID_BIT | (self.next_id.fetch_add(1, Ordering::Relaxed) & !CLIPBOARD_ID_BIT)
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
        match ctx.role {
            Role::Host => {
                rt.spawn(host_loop(self.shared.clone(), ctx, inbox));
            }
            Role::Viewer => {
                rt.spawn(viewer_loop(self.shared.clone(), ctx, inbox));
            }
        }
    }
}

/// Host: announce clipboard changes until the session ends.
async fn host_loop(me: Arc<Shared>, ctx: FeatureCtx, mut inbox: mpsc::UnboundedReceiver<Message>) {
    let mut clip = me.watcher.subscribe();
    let mut perms = ctx.permissions.clone();
    let mut current = clip.borrow_and_update().clone();
    // The list the viewer currently holds from us (`None` = nothing to paste).
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
        tokio::select! {
            changed = clip.changed() => {
                if changed.is_err() {
                    break;
                }
                current = clip.borrow_and_update().clone();
            }
            // Re-evaluated at the top: a revoke clears the viewer's entry, a grant shows what
            // is copied right now.
            changed = perms.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            msg = inbox.recv() => match msg {
                Some(Message::ClipboardFiles { .. }) => {
                    // The host cannot pull from the viewer (transfers are viewer-initiated);
                    // the viewer uploads instead.
                    tracing::debug!(session = ctx.session_key, "viewer announced clipboard files; ignored on the host");
                }
                Some(_) => {}
                None => break,
            },
        }
    }
    lock(&me.announced).remove(&ctx.session_key);
}

/// Sends the announcement for `paths` (empty = clear). `Ok(true)` if the viewer now holds a
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
    let transfer_id = me.next_transfer_id();
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

/// Viewer: remember the latest announcement and tell the UI.
async fn viewer_loop(
    me: Arc<Shared>,
    ctx: FeatureCtx,
    mut inbox: mpsc::UnboundedReceiver<Message>,
) {
    let mut perms = ctx.permissions.clone();
    loop {
        let msg = tokio::select! {
            msg = inbox.recv() => match msg {
                Some(msg) => msg,
                None => break,
            },
            changed = perms.changed() => {
                if changed.is_err() {
                    break;
                }
                if !ctx.allowed(REQUIRED) {
                    // Without the permission a paste would be refused: forget the offer.
                    clear_remote(&me, &ctx);
                }
                continue;
            }
        };
        let Message::ClipboardFiles { transfer_id, items } = msg else {
            continue;
        };
        if items.is_empty() {
            clear_remote(&me, &ctx);
            continue;
        }
        if !ctx.allowed(REQUIRED) {
            tracing::debug!("clipboard files without CLIPBOARD+FILES; ignored");
            continue;
        }
        if items.len() > MAX_ITEMS || !items.iter().all(|i| is_safe_rel_path(&i.rel_path)) {
            tracing::warn!(
                transfer_id,
                "host announced unsafe clipboard file paths; ignored"
            );
            continue;
        }
        let files = RemoteClipboardFiles { transfer_id, items };
        lock(&me.remote).insert(ctx.session_key, files.clone());
        ctx.emit(FEATURE_NAME, files);
    }
    lock(&me.remote).remove(&ctx.session_key);
}

/// Viewer: nothing to paste any more; tells the UI with an empty set.
fn clear_remote(me: &Shared, ctx: &FeatureCtx) {
    if let Some(old) = lock(&me.remote).remove(&ctx.session_key) {
        ctx.emit(
            FEATURE_NAME,
            RemoteClipboardFiles {
                transfer_id: old.transfer_id,
                items: Vec::new(),
            },
        );
    }
}
