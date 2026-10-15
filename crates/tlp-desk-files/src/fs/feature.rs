//! The session feature: serves `FsRequest`s on the host, routes `FsResponse`s to the
//! [`FsClient`] on the viewer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use tlp_desk_net::{Feature, FeatureCtx};
use tlp_desk_proto::{FsError, FsOp, FsReply, Message, Permissions, Role};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use super::client::FsClient;
use super::{execute, FsConfig};

/// [`Feature::name`] of the remote file system.
pub const FEATURE_NAME: &str = "fs";

/// Requests one session may have running on the host at the same time. More wait in the
/// inbox, so a viewer cannot flood the host's blocking thread pool.
const MAX_PARALLEL: usize = 4;

/// Remote file-system feature. Register one instance (shared by host and viewer sessions) in
/// `HostConfig::features` and `ViewerConfig::features`.
pub struct FsFeature {
    cfg: Arc<RwLock<FsConfig>>,
    clients: Arc<Mutex<HashMap<u64, FsClient>>>,
}

impl std::fmt::Debug for FsFeature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsFeature")
            .field(
                "cfg",
                &*self.cfg.read().unwrap_or_else(PoisonError::into_inner),
            )
            .finish_non_exhaustive()
    }
}

impl FsFeature {
    /// A feature serving the host's disks under `cfg`.
    pub fn new(cfg: FsConfig) -> Arc<Self> {
        Arc::new(Self {
            cfg: Arc::new(RwLock::new(cfg)),
            clients: Arc::default(),
        })
    }

    /// Replaces the host settings (e.g. the allow-list changed); applies to the next request.
    pub fn set_config(&self, cfg: FsConfig) {
        *self.cfg.write().unwrap_or_else(PoisonError::into_inner) = cfg;
    }

    /// Viewer side: the client of the session with this `FeatureCtx::session_key`
    /// (`ViewerSession::feature_key`). `None` before the session started or after it ended.
    pub fn client(&self, session_key: u64) -> Option<FsClient> {
        lock(&self.clients).get(&session_key).cloned()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Feature for FsFeature {
    fn name(&self) -> &'static str {
        FEATURE_NAME
    }

    fn handles(&self, msg: &Message) -> bool {
        matches!(msg, Message::FsRequest { .. } | Message::FsResponse { .. })
    }

    fn start(&self, ctx: FeatureCtx, inbox: mpsc::UnboundedReceiver<Message>) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            tracing::error!("fs feature started outside a tokio runtime; disabled");
            return;
        };
        match ctx.role {
            Role::Host => {
                rt.spawn(serve(ctx, inbox, self.cfg.clone()));
            }
            Role::Viewer => {
                let client = FsClient::new(ctx.session.clone(), ctx.permissions.clone());
                lock(&self.clients).insert(ctx.session_key, client.clone());
                rt.spawn(viewer_loop(ctx, inbox, client, self.clients.clone()));
            }
        }
    }
}

/// Host: answer every request of one session until it ends.
async fn serve(
    ctx: FeatureCtx,
    mut inbox: mpsc::UnboundedReceiver<Message>,
    cfg: Arc<RwLock<FsConfig>>,
) {
    let mut running = JoinSet::new();
    loop {
        while running.len() >= MAX_PARALLEL {
            let _ = running.join_next().await;
        }
        // Reap finished requests without waiting.
        while running.try_join_next().is_some() {}
        let Some(msg) = inbox.recv().await else {
            break;
        };
        match msg {
            Message::FsRequest { req_id, op } => {
                let cfg = cfg.read().unwrap_or_else(PoisonError::into_inner).clone();
                running.spawn(answer(ctx.clone(), cfg, req_id, op));
            }
            Message::FsResponse { req_id, .. } => {
                // The host never sends requests, so this answers nothing.
                tracing::debug!(
                    session = ctx.session_key,
                    req_id,
                    "unexpected fs response from the viewer"
                );
            }
            _ => {}
        }
    }
    // Session over: let running operations finish (they cannot be interrupted mid-syscall),
    // their answers fail to send and are dropped.
    while running.join_next().await.is_some() {}
}

async fn answer(ctx: FeatureCtx, cfg: FsConfig, req_id: u64, op: FsOp) {
    let result = if ctx.allowed(Permissions::FILES) {
        log_op(ctx.session_key, &op);
        tokio::task::spawn_blocking(move || execute(&op, &cfg))
            .await
            .unwrap_or_else(|e| {
                tracing::error!(%e, "fs operation panicked or was cancelled");
                Err(FsError::Other("internal error".into()))
            })
    } else {
        Err(FsError::PermissionDenied)
    };
    // Revoked while the operation ran: whatever it read is withheld.
    let result = if ctx.allowed(Permissions::FILES) {
        result
    } else {
        Err(FsError::PermissionDenied)
    };
    if let Err(e) = ctx
        .session
        .send(&Message::FsResponse { req_id, result })
        .await
    {
        tracing::debug!(%e, req_id, "cannot send fs response");
    }
}

/// Changes are logged at `info` (an audit trail of what a viewer did to this PC's files);
/// reads only at `debug`. Paths are not secret; file contents never pass through here.
fn log_op(session: u64, op: &FsOp) {
    match op {
        FsOp::MakeDir { path } => tracing::info!(session, %path, "viewer creates a folder"),
        FsOp::Rename { from, to } => tracing::info!(session, %from, %to, "viewer renames"),
        FsOp::Delete { path, recursive } => {
            tracing::info!(session, %path, recursive, "viewer deletes");
        }
        FsOp::ListDir { path } | FsOp::Stat { path } => {
            tracing::debug!(session, %path, "viewer reads");
        }
        FsOp::Roots => tracing::debug!(session, "viewer lists roots"),
    }
}

/// Viewer: deliver answers to the client; refuse requests from the host.
async fn viewer_loop(
    ctx: FeatureCtx,
    mut inbox: mpsc::UnboundedReceiver<Message>,
    client: FsClient,
    clients: Arc<Mutex<HashMap<u64, FsClient>>>,
) {
    while let Some(msg) = inbox.recv().await {
        match msg {
            Message::FsResponse { req_id, result } => client.resolve(req_id, result),
            Message::FsRequest { req_id, .. } => {
                // A viewer never serves its own disks to the host it controls.
                tracing::warn!(
                    session = ctx.session_key,
                    "host asked to browse this viewer's files; refused"
                );
                let refusal = Message::FsResponse {
                    req_id,
                    result: Err::<FsReply, _>(FsError::PermissionDenied),
                };
                let _ = ctx.session.send(&refusal).await;
            }
            _ => {}
        }
    }
    client.close();
    lock(&clients).remove(&ctx.session_key);
}
