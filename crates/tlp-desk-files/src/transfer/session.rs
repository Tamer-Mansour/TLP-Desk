//! Per-session loop: routes control messages, accepts `FileData` streams, ticks progress,
//! enforces the `FILES` permission live and stops everything when the session ends.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tlp_desk_net::{FeatureCtx, StreamKind, STREAM_REFUSED};
use tlp_desk_proto::{
    FsError, FsOp, FsReply, Message, Permissions, Role, TransferDirection, TransferItem,
    TransferOffer,
};
use tokio::sync::{mpsc, oneshot};

use super::conflict;
use super::error::FilesError;
use super::path::display_path;
use super::receiver::{self, Receiving};
use super::scan;
use super::sender::{self, Sending};
use super::state::{lock, Side, Transfer};
use super::types::{TransferEvent, TransferId, TransferInfo, TransferRole, TransferState};
use super::{Inner, FEATURE_NAME, TRANSFER_REQ_ID_BIT};

/// How often progress is reported (UI events and `TransferProgress` to the peer).
const TICK: Duration = Duration::from_millis(250);

/// One session as seen by the transfer engine.
pub struct SessionState {
    /// The feature context of the session.
    pub ctx: FeatureCtx,
    /// The engine (configuration, ids, event fan-out).
    pub inner: Arc<Inner>,
    /// Transfers by wire id.
    pub transfers: Mutex<HashMap<u64, Arc<Transfer>>>,
    /// Pending `FsRequest`s of ours, by request id.
    fs_waiters: Mutex<HashMap<u64, oneshot::Sender<Result<FsReply, FsError>>>>,
    next_req: AtomicU64,
    out: mpsc::UnboundedSender<Message>,
}

impl SessionState {
    /// Creates the state and the writer task that sends queued control messages in order.
    pub fn new(ctx: FeatureCtx, inner: Arc<Inner>) -> Arc<Self> {
        let (out, mut rx) = mpsc::unbounded_channel::<Message>();
        let session = ctx.session.clone();
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let Err(e) = session.send(&msg).await {
                    tracing::debug!(%e, "transfer control message not sent");
                    break;
                }
            }
        });
        Arc::new(Self {
            ctx,
            inner,
            transfers: Mutex::new(HashMap::new()),
            fs_waiters: Mutex::new(HashMap::new()),
            next_req: AtomicU64::new(1),
            out,
        })
    }

    /// Queues a control message for the peer (never blocks).
    pub fn send(&self, msg: Message) {
        let _ = self.out.send(msg);
    }

    /// Sends an event to the UI and to [`crate::FileTransfer::subscribe`] listeners.
    pub fn emit(&self, ev: TransferEvent) {
        let _ = self.inner.events.send((self.ctx.session_key, ev.clone()));
        self.ctx.emit(FEATURE_NAME, ev);
    }

    /// Whether the session currently holds `FILES`.
    pub fn files_allowed(&self) -> bool {
        self.ctx.allowed(Permissions::FILES)
    }

    /// Registers a transfer and announces it.
    pub fn register(&self, t: Arc<Transfer>) -> Result<(), FilesError> {
        {
            let mut map = lock(&self.transfers);
            if map.get(&t.wire).is_some_and(|old| !old.state().is_final()) {
                return Err(FilesError::Protocol(format!(
                    "transfer id {} in use",
                    t.wire
                )));
            }
            map.insert(t.wire, t.clone());
        }
        self.emit(TransferEvent::Started(t.info.clone()));
        Ok(())
    }

    /// The transfer with this wire id.
    pub fn get(&self, wire: u64) -> Option<Arc<Transfer>> {
        lock(&self.transfers).get(&wire).cloned()
    }

    /// The transfer with this local id.
    pub fn find(&self, id: TransferId) -> Option<Arc<Transfer>> {
        lock(&self.transfers).values().find(|t| t.id == id).cloned()
    }

    /// Stops a transfer locally (first stop wins), saves resume records, tells the UI.
    pub fn end(&self, t: &Arc<Transfer>, state: TransferState) {
        if !t.finish(state.clone()) {
            return;
        }
        if let Side::Recv(rx) = &t.side {
            let rx = rx.clone();
            let complete = state == TransferState::Completed;
            tokio::task::spawn_blocking(move || rx.save_partials(complete));
        }
        tracing::info!(transfer = t.id.0, ?state, "transfer finished");
        self.emit(self.progress_event(t));
        self.emit(TransferEvent::Finished {
            id: t.id,
            state,
            failed_items: t.failed.load(Ordering::Relaxed),
            local_dest: t.local_dest.clone(),
        });
    }

    /// Stops a transfer and tells the peer why.
    pub fn abort(&self, t: &Arc<Transfer>, state: TransferState) {
        if t.state().is_final() {
            return;
        }
        let msg = match &state {
            TransferState::Failed(e) => Message::TransferError {
                id: t.wire,
                item: None,
                error: e.clone(),
            },
            _ => Message::TransferCancel { id: t.wire },
        };
        self.send(msg);
        self.end(t, state);
    }

    fn progress_event(&self, t: &Transfer) -> TransferEvent {
        TransferEvent::Progress {
            id: t.id,
            bytes_done: t.bytes_done.load(Ordering::Relaxed),
            total_bytes: t.info.total_bytes,
            files_done: t.files_done.load(Ordering::Relaxed),
            total_files: t.info.total_files,
            bytes_per_sec: t.rate.load(Ordering::Relaxed),
        }
    }

    /// Asks the peer for one file-system operation (used to list a download selection).
    pub async fn fs_request(&self, op: FsOp) -> Result<FsReply, FilesError> {
        let req_id = TRANSFER_REQ_ID_BIT | self.next_req.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        lock(&self.fs_waiters).insert(req_id, tx);
        self.send(Message::FsRequest { req_id, op });
        let res = tokio::time::timeout(Duration::from_secs(30), rx).await;
        lock(&self.fs_waiters).remove(&req_id);
        match res {
            Ok(Ok(Ok(reply))) => Ok(reply),
            Ok(Ok(Err(e))) => Err(FilesError::Remote(e)),
            Ok(Err(_)) => Err(FilesError::NoSession(self.ctx.session_key)),
            Err(_) => Err(FilesError::Remote(FsError::Other("no answer".into()))),
        }
    }
}

/// Runs the engine for one session until its inbox closes.
pub async fn run(state: Arc<SessionState>, mut inbox: mpsc::UnboundedReceiver<Message>) {
    let accept = tokio::spawn(accept_loop(state.clone()));
    let ticker = tokio::spawn(tick_loop(state.clone()));
    let mut perms = state.ctx.permissions.clone();
    loop {
        tokio::select! {
            msg = inbox.recv() => match msg {
                Some(m) => handle(&state, m),
                None => break,
            },
            changed = perms.changed() => {
                if changed.is_err() {
                    break;
                }
                if !state.files_allowed() {
                    revoke_all(&state);
                }
            }
        }
    }
    accept.abort();
    ticker.abort();
    state.inner.pending.drop_session(state.ctx.session_key);
    let all: Vec<_> = lock(&state.transfers).values().cloned().collect();
    for t in all {
        state.end(&t, TransferState::Disconnected);
    }
    state.inner.remove_session(state.ctx.session_key);
}

/// `FILES` was revoked: stop every transfer of the session at once.
fn revoke_all(state: &Arc<SessionState>) {
    let all: Vec<_> = lock(&state.transfers).values().cloned().collect();
    for t in all {
        if !t.state().is_final() {
            tracing::warn!(
                transfer = t.id.0,
                "files permission revoked; stopping transfer"
            );
            state.abort(&t, TransferState::Failed(FsError::PermissionDenied));
        }
    }
}

fn handle(state: &Arc<SessionState>, msg: Message) {
    match msg {
        Message::TransferOffer(offer) => {
            if state.ctx.role != Role::Host {
                tracing::debug!("ignoring a transfer offer sent to a viewer");
                return;
            }
            let st = state.clone();
            tokio::spawn(async move { on_offer(st, offer).await });
        }
        Message::TransferAccept { id, resume } => {
            if let Some(Side::Send(s)) = state.get(id).as_ref().map(|t| &t.side) {
                s.accepted(Ok(resume));
            }
        }
        Message::TransferReject { id, error } => {
            if let Some(t) = state.get(id) {
                if let Side::Send(s) = &t.side {
                    s.accepted(Err(error.clone()));
                }
                state.end(&t, TransferState::Failed(error));
            }
        }
        Message::TransferProgress { id, bytes_done } => {
            if let Some(t) = state.get(id) {
                if let Side::Send(s) = &t.side {
                    s.peer_progress(state, &t, bytes_done);
                }
            }
        }
        Message::TransferCancel { id } => {
            if let Some(t) = state.get(id) {
                state.end(&t, TransferState::Cancelled);
            }
        }
        Message::TransferDone { id, item, blake3 } => {
            if let Some(t) = state.get(id) {
                if let Side::Recv(rx) = &t.side {
                    receiver::on_done(state, &t, rx, item, blake3);
                }
            }
        }
        Message::TransferError { id, item, error } => {
            let Some(t) = state.get(id) else { return };
            match (item, &t.side) {
                (None, _) => state.end(&t, TransferState::Failed(error)),
                (Some(i), Side::Recv(rx)) => {
                    receiver::on_sender_item_error(state, &t, rx, i, error)
                }
                (Some(i), Side::Send(s)) => s.on_receiver_item_error(state, &t, i, error),
            }
        }
        Message::FsRequest { req_id, op } => {
            if state.ctx.role != Role::Host {
                return;
            }
            let st = state.clone();
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || scan::fs_reply(&op))
                    .await
                    .unwrap_or_else(|_| Err(FsError::Other("listing failed".into())));
                st.send(Message::FsResponse { req_id, result });
            });
        }
        Message::FsResponse { req_id, result } => {
            if let Some(tx) = lock(&state.fs_waiters).remove(&req_id) {
                let _ = tx.send(result);
            }
        }
        other => {
            tracing::debug!(msg = ?std::mem::discriminant(&other), "unexpected message for the transfer engine")
        }
    }
}

/// Host: the viewer wants to upload or download.
async fn on_offer(state: Arc<SessionState>, offer: TransferOffer) {
    let wire = offer.id;
    if !state.files_allowed() {
        state.send(Message::TransferReject {
            id: wire,
            error: FsError::PermissionDenied,
        });
        return;
    }
    let res = match offer.direction {
        TransferDirection::Upload => host_receive(&state, offer).await,
        TransferDirection::Download => host_send(&state, offer).await,
    };
    if let Err(e) = res {
        tracing::warn!(%e, "transfer offer refused");
        state.send(Message::TransferReject {
            id: wire,
            error: e.to_fs_error(),
        });
    }
}

/// Host side of an upload: plan, accept, receive.
async fn host_receive(state: &Arc<SessionState>, offer: TransferOffer) -> Result<(), FilesError> {
    check_offer(&offer, state.inner.cfg.max_items)?;
    let mut dest_dir = offer.dest_dir.clone();
    if dest_dir.is_empty() {
        let lookup = lock(&state.inner.announced_dest).clone();
        if let Some(dir) = lookup.and_then(|f| f(state.ctx.session_key, offer.id)) {
            dest_dir = dir;
        }
    }
    let dest = upload_dest(&state.inner, &dest_dir)?;
    let policy = state.inner.cfg.conflict;
    let id = state.inner.alloc_id();
    // The host user may be asked (policy `Ask`); declining declines the viewer's upload.
    let decisions = conflict::decide(state, id, &dest, &offer.items, policy, false)
        .await
        .map_err(|e| match e {
            FilesError::Cancelled => FilesError::Remote(FsError::Declined),
            other => other,
        })?;
    let items = offer.items.clone();
    let rx = tokio::task::spawn_blocking(move || Receiving::prepare(&dest, items, &decisions))
        .await
        .map_err(|_| FilesError::Protocol("planning task failed".into()))??;
    let info = info_for(
        state,
        id,
        &offer,
        TransferRole::Receiving,
        false,
        display_path(&rx.root),
    );
    let rx = Arc::new(rx);
    let t = Arc::new(Transfer::new(
        id,
        offer.id,
        info,
        Some(rx.root.clone()),
        Side::Recv(rx.clone()),
    ));
    rx.init_progress(&t);
    state.register(t.clone())?;
    let resume = rx.resume.clone();
    t.set_running();
    state.send(Message::TransferAccept {
        id: offer.id,
        resume,
    });
    receiver::check_complete(state, &t, &rx);
    Ok(())
}

/// Host side of a download: check the sources, accept, send.
async fn host_send(state: &Arc<SessionState>, offer: TransferOffer) -> Result<(), FilesError> {
    check_offer(&offer, state.inner.cfg.max_items)?;
    let mut src = offer.source_dir.clone();
    if src.is_empty() {
        let lookup = lock(&state.inner.announced_source).clone();
        if let Some(dir) = lookup.and_then(|f| f(state.ctx.session_key, offer.id)) {
            src = dir;
        }
    }
    let items = offer.items.clone();
    let paths = tokio::task::spawn_blocking(move || sender::resolve_sources(&src, &items))
        .await
        .map_err(|_| FilesError::Protocol("source check failed".into()))??;
    let id = state.inner.alloc_id();
    let info = info_for(
        state,
        id,
        &offer,
        TransferRole::Sending,
        false,
        offer.dest_dir.clone(),
    );
    let sending = Arc::new(Sending::new(offer.items.clone(), paths, None));
    let t = Arc::new(Transfer::new(
        id,
        offer.id,
        info,
        None,
        Side::Send(sending.clone()),
    ));
    state.register(t.clone())?;
    state.send(Message::TransferAccept {
        id: offer.id,
        resume: Vec::new(),
    });
    t.set_running();
    sender::spawn(state.clone(), t, sending, offer.resume);
    Ok(())
}

/// Sanity checks every offer gets before any disk access.
fn check_offer(offer: &TransferOffer, max_items: usize) -> Result<(), FilesError> {
    if offer.items.len() > max_items {
        return Err(FilesError::TooManyItems {
            count: offer.items.len(),
            max: max_items,
        });
    }
    let sum = offer
        .items
        .iter()
        .filter(|i| !i.is_dir)
        .try_fold(0u64, |acc, i| acc.checked_add(i.size));
    if sum != Some(offer.total_bytes) {
        return Err(FilesError::Protocol(
            "total_bytes does not match the items".into(),
        ));
    }
    if offer.items.iter().any(|i| i.is_dir && i.size != 0) {
        return Err(FilesError::Protocol("a folder with a size".into()));
    }
    Ok(())
}

/// Where an upload goes on the host: the viewer's choice, or the configured default
/// (Desktop, else home).
fn upload_dest(inner: &Inner, dest_dir: &str) -> Result<PathBuf, FilesError> {
    if !dest_dir.is_empty() {
        return scan::checked_abs(dest_dir).map_err(FilesError::Remote);
    }
    if let Some(d) = &inner.cfg.default_upload_dir {
        return Ok(d.clone());
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .ok_or(FilesError::Remote(FsError::NotFound))?;
    let desktop = home.join("Desktop");
    Ok(if desktop.is_dir() { desktop } else { home })
}

/// Describes an offer for the UI.
pub fn info_for(
    state: &SessionState,
    id: TransferId,
    offer: &TransferOffer,
    role: TransferRole,
    initiated_locally: bool,
    dest_dir: String,
) -> TransferInfo {
    let top: Vec<&TransferItem> = offer
        .items
        .iter()
        .filter(|i| !i.rel_path.contains('/'))
        .collect();
    TransferInfo {
        id,
        session_key: state.ctx.session_key,
        role,
        direction: offer.direction,
        initiated_locally,
        label: top.first().map(|i| i.rel_path.clone()).unwrap_or_default(),
        top_level_items: u32::try_from(top.len()).unwrap_or(u32::MAX),
        source_dir: offer.source_dir.clone(),
        dest_dir,
        total_bytes: offer.total_bytes,
        total_files: count(offer, false),
        total_dirs: count(offer, true),
        started_unix_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
    }
}

fn count(offer: &TransferOffer, dirs: bool) -> u32 {
    u32::try_from(offer.items.iter().filter(|i| i.is_dir == dirs).count()).unwrap_or(u32::MAX)
}

/// Accepts every `FileData` stream of the session and hands it to its transfer.
async fn accept_loop(state: Arc<SessionState>) {
    while let Ok(mut s) = state
        .ctx
        .session
        .accept_side_stream(StreamKind::FileData)
        .await
    {
        let target = state.get(s.id).filter(|t| !t.stopped());
        let rx = match target.as_ref().map(|t| &t.side) {
            Some(Side::Recv(rx)) if state.files_allowed() => rx.clone(),
            _ => {
                tracing::debug!(id = s.id, "refusing a file data stream");
                let _ = s.recv.stop(STREAM_REFUSED.into());
                continue;
            }
        };
        let Some(t) = target else { continue };
        let st = state.clone();
        tokio::spawn(async move { receiver::handle_stream(st, t, rx, s.recv).await });
    }
}

/// Progress for the UI and the peer, ~4 times per second, only while something changes.
async fn tick_loop(state: Arc<SessionState>) {
    let mut last: HashMap<u64, (u64, u32)> = HashMap::new();
    let mut iv = tokio::time::interval(TICK);
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        iv.tick().await;
        let running: Vec<_> = lock(&state.transfers)
            .values()
            .filter(|t| t.state() == TransferState::Running)
            .cloned()
            .collect();
        for t in running {
            t.tick_rate();
            let now = (
                t.bytes_done.load(Ordering::Relaxed),
                t.files_done.load(Ordering::Relaxed),
            );
            if last.get(&t.wire) == Some(&now) {
                continue;
            }
            last.insert(t.wire, now);
            state.emit(state.progress_event(&t));
            if let Side::Recv(_) = t.side {
                // Never report the total before every file is verified: the sender takes
                // `bytes_done == total_bytes` as "complete".
                let done = now.0.min(t.info.total_bytes.saturating_sub(1));
                state.send(Message::TransferProgress {
                    id: t.wire,
                    bytes_done: done,
                });
            }
        }
    }
}
