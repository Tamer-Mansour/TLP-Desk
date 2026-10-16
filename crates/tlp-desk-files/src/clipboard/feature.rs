//! The session feature: one task per session that syncs the clipboard both ways.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tlp_desk_net::{Feature, FeatureCtx, StreamKind, STREAM_REFUSED};
use tlp_desk_proto::{FrameDecoder, Message, Permissions, Role, MAX_FRAME_LEN};
use tokio::sync::{mpsc, watch};

use super::error::ClipboardError;
use super::sync::{content_hash, OwnWrites, SyncState};
use super::{
    wire, ClipContent, ClipKind, ClipboardBackend, ClipboardConfig, ClipboardEvent, Direction,
    RefusedReason,
};

/// [`Feature::name`] of the clipboard feature and `FeatureEvent::feature` of its events.
pub const FEATURE_NAME: &str = "clipboard";

/// How the file-transfer feature plugs into clipboard sync: it is told when the local
/// clipboard holds files (Explorer copy, `CF_HDROP`) and announces them with
/// `Message::ClipboardFiles` itself (that needs `FILES` as well).
pub trait LocalFilesHook: Send + Sync + 'static {
    /// The local clipboard changed to `paths` while clipboard sync is on and `CLIPBOARD` is
    /// granted for the session of `ctx`. Must not block.
    fn local_files(&self, ctx: &FeatureCtx, paths: Vec<PathBuf>);
}

/// (is_host, session_key): host and viewer session keys come from different counters.
type SessionKey = (bool, u64);

fn map_key(role: Role, key: u64) -> SessionKey {
    (role == Role::Host, key)
}

struct Inner {
    backend: Arc<dyn ClipboardBackend>,
    cfg: ClipboardConfig,
    own: Mutex<OwnWrites>,
    sessions: Mutex<HashMap<SessionKey, watch::Sender<bool>>>,
    default_enabled: AtomicBool,
    files_hook: Mutex<Option<Arc<dyn LocalFilesHook>>>,
    next_stream_id: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Two-way clipboard sync. Register one instance (it is cheap to clone through `Arc`) in both
/// the host's and the viewer's `FeatureSet`; all sessions share its clipboard backend.
pub struct ClipboardFeature {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for ClipboardFeature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardFeature")
            .field("cfg", &self.inner.cfg)
            .field("sessions", &lock(&self.inner.sessions).len())
            .finish_non_exhaustive()
    }
}

impl ClipboardFeature {
    /// A feature syncing `backend` with the limits in `cfg`.
    pub fn new(backend: Arc<dyn ClipboardBackend>, cfg: ClipboardConfig) -> Arc<Self> {
        let default_enabled = AtomicBool::new(cfg.enabled_by_default);
        Arc::new(Self {
            inner: Arc::new(Inner {
                backend,
                cfg,
                own: Mutex::default(),
                sessions: Mutex::default(),
                default_enabled,
                files_hook: Mutex::new(None),
                next_stream_id: AtomicU64::new(1),
            }),
        })
    }

    /// The clipboard this feature syncs.
    pub fn backend(&self) -> &Arc<dyn ClipboardBackend> {
        &self.inner.backend
    }

    /// The limits in use.
    pub fn config(&self) -> &ClipboardConfig {
        &self.inner.cfg
    }

    /// Switches sync on or off for one running session (the viewer's "Sync clipboard"
    /// toggle; also usable on the host). Returns `false` if no such session runs.
    pub fn set_sync_enabled(&self, role: Role, session_key: u64, enabled: bool) -> bool {
        match lock(&self.inner.sessions).get(&map_key(role, session_key)) {
            Some(tx) => {
                tx.send_replace(enabled);
                true
            }
            None => false,
        }
    }

    /// Whether sync is on for a running session (`None` if it does not run).
    pub fn sync_enabled(&self, role: Role, session_key: u64) -> Option<bool> {
        lock(&self.inner.sessions)
            .get(&map_key(role, session_key))
            .map(|tx| *tx.borrow())
    }

    /// Whether sessions started from now on begin with sync on (settings default).
    pub fn set_default_enabled(&self, enabled: bool) {
        self.inner.default_enabled.store(enabled, Ordering::Relaxed);
    }

    /// Installs the file-transfer feature's hook for files on the clipboard.
    pub fn set_files_hook(&self, hook: Arc<dyn LocalFilesHook>) {
        *lock(&self.inner.files_hook) = Some(hook);
    }

    /// Writes `content` to the local clipboard without it being sent back to any peer (for
    /// the file-transfer feature when the user pastes files received from the peer).
    ///
    /// # Errors
    /// [`ClipboardError::Clipboard`] if the clipboard cannot be written.
    pub fn write_local(&self, content: &ClipContent) -> Result<(), ClipboardError> {
        let seq = self.inner.backend.write(content)?;
        lock(&self.inner.own).record(seq);
        Ok(())
    }
}

impl Feature for ClipboardFeature {
    fn name(&self) -> &'static str {
        FEATURE_NAME
    }

    fn handles(&self, msg: &Message) -> bool {
        wire::kind_of(msg).is_some()
    }

    fn start(&self, ctx: FeatureCtx, inbox: mpsc::UnboundedReceiver<Message>) {
        let key = map_key(ctx.role, ctx.session_key);
        let (tx, rx) = watch::channel(self.inner.default_enabled.load(Ordering::Relaxed));
        lock(&self.inner.sessions).insert(key, tx);
        // Subscribe now, not in the task, so a change right after the session starts is seen.
        let mut changes = self.inner.backend.changes();
        if let Some(c) = &mut changes {
            // Only changes from now on; what is already there is pushed below if wanted.
            c.borrow_and_update();
        }
        let task = SessionTask {
            inner: self.inner.clone(),
            ctx,
            enabled: rx,
            changes,
            state: SyncState::default(),
        };
        tokio::spawn(task.run(inbox));
    }
}

struct SessionTask {
    inner: Arc<Inner>,
    ctx: FeatureCtx,
    enabled: watch::Receiver<bool>,
    changes: Option<watch::Receiver<u64>>,
    state: SyncState,
}

/// Waits for the next change notification; never resolves without a notifier.
async fn next_change(rx: &mut Option<watch::Receiver<u64>>) -> bool {
    match rx {
        Some(r) => r.changed().await.is_ok(),
        None => std::future::pending().await,
    }
}

impl SessionTask {
    fn allowed(&self) -> bool {
        self.ctx.allowed(Permissions::CLIPBOARD) && *self.enabled.borrow()
    }

    fn emit(&self, ev: ClipboardEvent) {
        self.ctx.emit(FEATURE_NAME, ev);
    }

    fn refused(&self, direction: Direction, kind: ClipKind, e: &ClipboardError) {
        let reason = match e {
            ClipboardError::TooLarge { .. } => RefusedReason::TooLarge,
            ClipboardError::BadImage(_) | ClipboardError::Proto(_) => RefusedReason::Invalid,
            ClipboardError::Clipboard(_) | ClipboardError::Net(_) => RefusedReason::ClipboardError,
        };
        tracing::info!(session = self.ctx.session_key, ?direction, ?kind, %e, "clipboard not synced");
        self.emit(ClipboardEvent::Refused {
            direction,
            kind,
            reason,
        });
    }

    async fn run(mut self, mut inbox: mpsc::UnboundedReceiver<Message>) {
        let mut changes = self.changes.take();
        let (side_tx, mut side_rx) = mpsc::channel(1);
        let acceptor = tokio::spawn(accept_streams(self.ctx.clone(), side_tx));
        let viewer_push = self.ctx.role == Role::Viewer && self.inner.cfg.push_on_start;
        if viewer_push {
            self.push_local().await;
        }
        loop {
            tokio::select! {
                msg = inbox.recv() => match msg {
                    Some(m) => self.apply_remote(m).await,
                    None => break,
                },
                Some(m) = side_rx.recv() => self.apply_remote(m).await,
                ok = next_change(&mut changes) => {
                    if !ok {
                        changes = None;
                        continue;
                    }
                    // Debounce: let a burst of changes settle, then read once.
                    tokio::time::sleep(self.inner.cfg.debounce).await;
                    let seq = changes.as_mut().map_or(0, |c| *c.borrow_and_update());
                    if lock(&self.inner.own).is_own(seq) {
                        continue;
                    }
                    self.push_local().await;
                }
                r = self.enabled.changed() => {
                    if r.is_err() {
                        break;
                    }
                    if *self.enabled.borrow_and_update() {
                        self.state.reset_sent();
                        if viewer_push {
                            self.push_local().await;
                        }
                    }
                }
            }
        }
        acceptor.abort();
        lock(&self.inner.sessions).remove(&map_key(self.ctx.role, self.ctx.session_key));
    }

    /// Sends the local clipboard to the peer if allowed and new.
    async fn push_local(&mut self) {
        if !self.allowed() {
            return;
        }
        let backend = self.inner.backend.clone();
        let read = tokio::task::spawn_blocking(move || {
            backend.read().map(|c| c.map(|c| (content_hash(&c), c)))
        })
        .await;
        let (hash, content) = match read {
            Ok(Ok(Some(v))) => v,
            Ok(Ok(None)) => return,
            Ok(Err(e)) => {
                tracing::debug!(%e, "cannot read the local clipboard");
                return;
            }
            Err(e) => {
                tracing::warn!(%e, "clipboard read task failed");
                return;
            }
        };
        // Sync may have been switched off or revoked while reading.
        if !self.allowed() {
            return;
        }
        if let ClipContent::Files(paths) = content {
            let hook = lock(&self.inner.files_hook).clone();
            if let Some(h) = hook {
                if self.state.should_send(hash) {
                    h.local_files(&self.ctx, paths);
                }
            }
            return;
        }
        if !self.state.should_send(hash) {
            return;
        }
        let kind = content.kind();
        let size = content.size();
        let cfg = self.inner.cfg.clone();
        // PNG encoding is CPU work: keep it off the async workers.
        let built = tokio::task::spawn_blocking(move || {
            let msg = wire::to_message(&content, &cfg)?;
            msg.map(|m| wire::frame(&m, kind).map(|(f, inline)| (m, f, inline)))
                .transpose()
        })
        .await;
        let (msg, frame, inline) = match built {
            Ok(Ok(Some(v))) => v,
            Ok(Ok(None)) => return,
            Ok(Err(e)) => {
                self.state.reset_sent();
                self.refused(Direction::Sent, kind, &e);
                return;
            }
            Err(e) => {
                tracing::warn!(%e, "clipboard encode task failed");
                return;
            }
        };
        let sent = if inline {
            self.ctx
                .session
                .send(&msg)
                .await
                .map_err(ClipboardError::from)
        } else {
            let id = self.inner.next_stream_id.fetch_add(1, Ordering::Relaxed);
            send_on_stream(&self.ctx, id, &frame).await
        };
        match sent {
            Ok(()) => self.emit(ClipboardEvent::Synced {
                direction: Direction::Sent,
                kind,
                bytes: size,
            }),
            Err(e) => {
                self.state.reset_sent();
                self.refused(Direction::Sent, kind, &e);
            }
        }
    }

    /// Writes content from the peer to the local clipboard if allowed.
    async fn apply_remote(&mut self, msg: Message) {
        let Some(kind) = wire::kind_of(&msg) else {
            return;
        };
        if !self.allowed() {
            tracing::debug!(
                session = self.ctx.session_key,
                ?kind,
                "clipboard from peer dropped (sync off or not permitted)"
            );
            return;
        }
        let cfg = self.inner.cfg.clone();
        let decoded = tokio::task::spawn_blocking(move || wire::from_message(msg, &cfg)).await;
        let content = match decoded {
            Ok(Ok(Some(c))) => c,
            Ok(Ok(None)) => return,
            Ok(Err(e)) => {
                self.refused(Direction::Received, kind, &e);
                return;
            }
            Err(e) => {
                tracing::warn!(%e, "clipboard decode task failed");
                return;
            }
        };
        // Permission may have been revoked while decoding.
        if !self.allowed() {
            return;
        }
        let size = content.size();
        self.state.received(content_hash(&content));
        let backend = self.inner.backend.clone();
        let inner = self.inner.clone();
        let written = tokio::task::spawn_blocking(move || {
            // Record under the lock so a change notification cannot be judged in between.
            let mut own = lock(&inner.own);
            backend.write(&content).map(|seq| own.record(seq))
        })
        .await;
        match written {
            Ok(Ok(())) => self.emit(ClipboardEvent::Synced {
                direction: Direction::Received,
                kind,
                bytes: size,
            }),
            Ok(Err(e)) => self.refused(Direction::Received, kind, &e),
            Err(e) => tracing::warn!(%e, "clipboard write task failed"),
        }
    }
}

/// Sends one framed message on its own `Clipboard` side stream.
async fn send_on_stream(ctx: &FeatureCtx, id: u64, frame: &[u8]) -> Result<(), ClipboardError> {
    let mut s = ctx.session.open_uni(StreamKind::Clipboard, id).await?;
    s.write_all(frame)
        .await
        .map_err(|e| ClipboardError::Clipboard(format!("clipboard stream: {e}")))?;
    s.finish()
        .map_err(|e| ClipboardError::Clipboard(format!("clipboard stream: {e}")))?;
    Ok(())
}

/// Accepts the peer's `Clipboard` side streams one at a time (so at most one frame is
/// buffered), refuses them without `CLIPBOARD`, and hands decoded messages to the session task.
async fn accept_streams(ctx: FeatureCtx, out: mpsc::Sender<Message>) {
    while let Ok(s) = ctx.session.accept_side_stream(StreamKind::Clipboard).await {
        let mut recv = s.recv;
        if !ctx.allowed(Permissions::CLIPBOARD) {
            let _ = recv.stop(STREAM_REFUSED.into());
            continue;
        }
        let body = match recv.read_to_end(MAX_FRAME_LEN.saturating_add(4)).await {
            Ok(b) => b,
            Err(e) => {
                tracing::debug!(%e, "clipboard stream dropped");
                continue;
            }
        };
        let mut dec = FrameDecoder::new();
        dec.push(&body);
        match dec.next_message() {
            Ok(Some(m)) if wire::kind_of(&m).is_some() => {
                if out.send(m).await.is_err() {
                    break;
                }
            }
            Ok(_) => tracing::debug!("clipboard stream without a clipboard message"),
            Err(e) => tracing::debug!(%e, "bad clipboard stream"),
        }
    }
}
