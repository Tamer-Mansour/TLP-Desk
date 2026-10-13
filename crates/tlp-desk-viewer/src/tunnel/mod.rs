//! Viewer side of TCP port forwarding (ROADMAP M4): listen on a local port and carry every
//! accepted TCP connection to a service on (or next to) the host, e.g. the remote PC's local
//! web server.
//!
//! ```text
//! let tunnels = ViewerTunnels::new(TunnelViewerConfig::default());
//! // ViewerConfig { features: FeatureSet::new().with(tunnels.clone()), .. }
//! let status = tunnels.add(session.feature_key(), "8080:127.0.0.1:80".parse()?).await?;
//! // http://127.0.0.1:8080 now reaches port 80 on the host.
//! tunnels.stop(session.feature_key(), status.id);
//! ```
//!
//! * [`ViewerTunnels::add`] binds the local listener first (so a busy port fails at once),
//!   then asks the host with `TunnelOpen` and waits for `TunnelOpened` / `TunnelClose`.
//! * Each accepted TCP connection becomes one `StreamKind::Tunnel` bidi stream
//!   ([`tlp_desk_net::pipe_tcp`]: half-close, idle timeout, QUIC backpressure).
//! * The host can close a tunnel at any time (e.g. `TUNNEL` revoked); the rule disappears
//!   and [`TunnelEvent::Closed`] says why. Losing `TUNNEL` closes all rules locally too.
//! * The listener is loopback-only unless [`ForwardSpec::allow_lan_bind`] is set.

mod spec;

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tlp_desk_net::{
    close_reason, pipe_tcp, sanitize_close_text, Feature, FeatureCtx, NetError, PipeEnd,
    StreamKind, TunnelCounters,
};
use tlp_desk_proto::{Message, Permissions};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch};

use crate::ViewerError;

pub use spec::ForwardSpec;

/// [`Feature::name`] of the tunnel feature (same on the host).
pub const TUNNEL_FEATURE: &str = "tunnel";

/// How often counters are re-published while rules exist.
const STATS_EVERY: Duration = Duration::from_secs(1);

/// Identifies a forwarding rule within its session (the wire tunnel id).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TunnelId(pub u64);

impl fmt::Display for TunnelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Limits of the viewer tunnel feature.
#[derive(Debug, Clone)]
pub struct TunnelViewerConfig {
    /// Rules per session (default 16; the host has its own limit).
    pub max_rules: usize,
    /// Concurrent forwarded connections per session (default 64); more are dropped at once.
    pub max_connections: usize,
    /// Idle connections are closed after this long (default 10 minutes).
    pub idle_timeout: Duration,
    /// How long to wait for the host to answer `TunnelOpen` (default 10 s).
    pub open_timeout: Duration,
}

impl Default for TunnelViewerConfig {
    fn default() -> Self {
        Self {
            max_rules: 16,
            max_connections: 64,
            idle_timeout: Duration::from_secs(600),
            open_timeout: Duration::from_secs(10),
        }
    }
}

/// Why the host refused or closed a tunnel (from `TunnelClose::error`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelCloseReason {
    /// The target is outside the host's network (e.g. an internet address).
    TargetNotAllowed,
    /// The target is malformed (port 0, a name instead of an IP address...).
    BadTarget,
    /// The host's limit of tunnels per session is reached.
    TooManyTunnels,
    /// The tunnel id is already in use.
    DuplicateId,
    /// The session lacks the `TUNNEL` permission.
    PermissionDenied,
    /// The host user revoked `TUNNEL`.
    PermissionRevoked,
    /// The host sent no answer in time.
    NoAnswer,
    /// The host closed the tunnel without giving a reason.
    ClosedByHost,
    /// Any other text from the host (sanitized, at most `MAX_CLOSE_TEXT` characters).
    Other(String),
}

impl TunnelCloseReason {
    /// Maps a `TunnelClose::error` from the host.
    pub fn from_wire(error: &str) -> Self {
        match error {
            close_reason::TARGET_NOT_ALLOWED => Self::TargetNotAllowed,
            close_reason::BAD_TARGET => Self::BadTarget,
            close_reason::TOO_MANY_TUNNELS => Self::TooManyTunnels,
            close_reason::DUPLICATE_ID => Self::DuplicateId,
            close_reason::PERMISSION_DENIED => Self::PermissionDenied,
            close_reason::PERMISSION_REVOKED => Self::PermissionRevoked,
            other => Self::Other(sanitize_close_text(other)),
        }
    }
}

impl fmt::Display for TunnelCloseReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TargetNotAllowed => f.write_str("the target is not on the host's local network"),
            Self::BadTarget => f.write_str("the target must be an IP address and a port"),
            Self::TooManyTunnels => f.write_str("too many tunnels"),
            Self::DuplicateId => f.write_str("tunnel id already in use"),
            Self::PermissionDenied => f.write_str("port forwarding is not allowed"),
            Self::PermissionRevoked => f.write_str("port forwarding was revoked by the host"),
            Self::NoAnswer => f.write_str("the host did not answer"),
            Self::ClosedByHost => f.write_str("the host closed the tunnel"),
            Self::Other(s) => f.write_str(s),
        }
    }
}

/// One active forwarding rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelStatus {
    pub id: TunnelId,
    /// The real local listening address (port filled in when 0 was asked for).
    pub local: SocketAddr,
    /// Target host as sent to the host (`None` = the host itself).
    pub target_host: Option<String>,
    pub target_port: u16,
    /// Forwarded connections open right now.
    pub active: usize,
    /// Connections accepted so far.
    pub connections: u64,
    /// Connections that failed or were refused (host could not reach the target, limits).
    pub failed: u64,
    /// Bytes sent from local clients to the remote service.
    pub bytes_sent: u64,
    /// Bytes received from the remote service.
    pub bytes_received: u64,
}

impl TunnelStatus {
    /// The target as `host:port` (`localhost` = the host itself).
    pub fn target_display(&self) -> String {
        spec::target_display(self.target_host.as_deref(), self.target_port)
    }
}

/// Payload of `ViewerEvent::Feature` events with feature name [`TUNNEL_FEATURE`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelEvent {
    /// All active rules of the session (sent on change and about once per second while
    /// connections move bytes). Empty = no rule.
    Changed(Vec<TunnelStatus>),
    /// A rule ended without the local user stopping it (host closed it, permission revoked).
    Closed {
        id: TunnelId,
        reason: TunnelCloseReason,
    },
}

/// The viewer tunnel feature. Register one in `ViewerConfig::features`; use it with the
/// session's `ViewerSession::feature_key`.
pub struct ViewerTunnels {
    cfg: TunnelViewerConfig,
    sessions: Arc<Mutex<HashMap<u64, Arc<SessionState>>>>,
}

impl fmt::Debug for ViewerTunnels {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ViewerTunnels")
            .field("cfg", &self.cfg)
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

type OpenReply = oneshot::Sender<Result<(), TunnelCloseReason>>;

struct SessionState {
    ctx: FeatureCtx,
    rules: Mutex<HashMap<TunnelId, Arc<Rule>>>,
    /// `TunnelOpen`s waiting for the host's answer.
    pending: Mutex<HashMap<TunnelId, OpenReply>>,
    next_id: AtomicU64,
    /// Open connections over all rules.
    active: AtomicUsize,
}

struct Rule {
    id: TunnelId,
    local: SocketAddr,
    target_host: Option<String>,
    target_port: u16,
    counters: TunnelCounters,
    active: AtomicUsize,
    connections: AtomicU64,
    failed: AtomicU64,
    /// `true` stops the listener and every connection.
    cancel: watch::Sender<bool>,
}

impl Rule {
    fn status(&self) -> TunnelStatus {
        let (sent, received) = self.counters.snapshot();
        TunnelStatus {
            id: self.id,
            local: self.local,
            target_host: self.target_host.clone(),
            target_port: self.target_port,
            active: self.active.load(Ordering::Relaxed),
            connections: self.connections.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            bytes_sent: sent,
            bytes_received: received,
        }
    }

    fn stop(&self) {
        self.cancel.send_replace(true);
    }
}

impl SessionState {
    fn statuses(&self) -> Vec<TunnelStatus> {
        let mut v: Vec<_> = lock(&self.rules).values().map(|r| r.status()).collect();
        v.sort_by_key(|s| s.id);
        v
    }

    fn emit_changed(&self) {
        self.ctx
            .emit(TUNNEL_FEATURE, TunnelEvent::Changed(self.statuses()));
    }

    /// Removes and stops a rule; `true` if it existed.
    fn remove(&self, id: TunnelId) -> bool {
        match lock(&self.rules).remove(&id) {
            Some(r) => {
                r.stop();
                true
            }
            None => false,
        }
    }
}

impl ViewerTunnels {
    pub fn new(cfg: TunnelViewerConfig) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            sessions: Arc::default(),
        })
    }

    fn session(&self, session_key: u64) -> Result<Arc<SessionState>, ViewerError> {
        lock(&self.sessions)
            .get(&session_key)
            .cloned()
            .ok_or(ViewerError::SessionEnded)
    }

    /// Starts forwarding: binds the local listener, asks the host, and returns the rule once
    /// the host agreed. Errors: [`ViewerError::TunnelLanBind`] (non-loopback bind without
    /// confirmation), [`ViewerError::TunnelNotAllowed`], [`ViewerError::TunnelLimit`],
    /// [`ViewerError::TunnelBind`], [`ViewerError::TunnelRefused`],
    /// [`ViewerError::SessionEnded`].
    pub async fn add(
        &self,
        session_key: u64,
        spec: ForwardSpec,
    ) -> Result<TunnelStatus, ViewerError> {
        if spec.binds_beyond_loopback() && !spec.allow_lan_bind {
            return Err(ViewerError::TunnelLanBind(spec.bind));
        }
        if spec.target_port == 0 {
            return Err(ViewerError::TunnelSpec(spec.to_string()));
        }
        let st = self.session(session_key)?;
        if !st.ctx.allowed(Permissions::TUNNEL) {
            return Err(ViewerError::TunnelNotAllowed);
        }
        if lock(&st.rules).len() >= self.cfg.max_rules {
            return Err(ViewerError::TunnelLimit);
        }
        let listener = TcpListener::bind(spec.bind)
            .await
            .map_err(|e| ViewerError::TunnelBind {
                addr: spec.bind,
                reason: e.to_string(),
            })?;
        let local = listener.local_addr().map_err(|e| ViewerError::TunnelBind {
            addr: spec.bind,
            reason: e.to_string(),
        })?;
        let id = TunnelId(st.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        lock(&st.pending).insert(id, tx);
        let open = Message::TunnelOpen {
            id: id.0,
            target_port: spec.target_port,
            target_host: spec.target_host.clone(),
        };
        if st.ctx.session.send(&open).await.is_err() {
            lock(&st.pending).remove(&id);
            return Err(ViewerError::SessionEnded);
        }
        match tokio::time::timeout(self.cfg.open_timeout, rx).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(reason))) => return Err(ViewerError::TunnelRefused(reason)),
            Ok(Err(_)) => return Err(ViewerError::SessionEnded),
            Err(_) => {
                lock(&st.pending).remove(&id);
                // Close it in case the answer is merely late.
                let _ = st
                    .ctx
                    .session
                    .send(&Message::TunnelClose {
                        id: id.0,
                        error: None,
                    })
                    .await;
                return Err(ViewerError::TunnelRefused(TunnelCloseReason::NoAnswer));
            }
        }
        let (cancel, _) = watch::channel(false);
        let rule = Arc::new(Rule {
            id,
            local,
            target_host: spec.target_host,
            target_port: spec.target_port,
            counters: TunnelCounters::default(),
            active: AtomicUsize::new(0),
            connections: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            cancel,
        });
        lock(&st.rules).insert(id, rule.clone());
        tracing::info!(session = session_key, %id, %local, target = %rule.status().target_display(), "port forwarding started");
        tokio::spawn(accept_loop(
            self.cfg.clone(),
            st.clone(),
            rule.clone(),
            listener,
        ));
        st.emit_changed();
        Ok(rule.status())
    }

    /// Stops a rule (closes its listener and connections and tells the host). `false` if it
    /// does not exist.
    pub fn stop(&self, session_key: u64, id: TunnelId) -> bool {
        let Ok(st) = self.session(session_key) else {
            return false;
        };
        if !st.remove(id) {
            return false;
        }
        tracing::info!(session = session_key, %id, "port forwarding stopped");
        st.emit_changed();
        let ctx = st.ctx.clone();
        tokio::spawn(async move {
            let _ = ctx
                .session
                .send(&Message::TunnelClose {
                    id: id.0,
                    error: None,
                })
                .await;
        });
        true
    }

    /// The active rules of a session (empty if none or unknown).
    pub fn list(&self, session_key: u64) -> Vec<TunnelStatus> {
        self.session(session_key)
            .map(|s| s.statuses())
            .unwrap_or_default()
    }
}

impl Feature for ViewerTunnels {
    fn name(&self) -> &'static str {
        TUNNEL_FEATURE
    }

    fn handles(&self, msg: &Message) -> bool {
        matches!(
            msg,
            Message::TunnelOpened { .. } | Message::TunnelClose { .. }
        )
    }

    fn start(&self, ctx: FeatureCtx, inbox: mpsc::UnboundedReceiver<Message>) {
        let key = ctx.session_key;
        let st = Arc::new(SessionState {
            ctx,
            rules: Mutex::default(),
            pending: Mutex::default(),
            next_id: AtomicU64::new(1),
            active: AtomicUsize::new(0),
        });
        lock(&self.sessions).insert(key, st.clone());
        let registry = self.sessions.clone();
        tokio::spawn(async move {
            run(&st, inbox).await;
            lock(&registry).remove(&key);
            for (_, r) in lock(&st.rules).drain() {
                r.stop();
            }
            lock(&st.pending).clear();
        });
    }
}

/// Handles the host's answers, permission changes and periodic counter updates.
async fn run(st: &SessionState, mut inbox: mpsc::UnboundedReceiver<Message>) {
    let mut perms = st.ctx.permissions.clone();
    let mut tick = tokio::time::interval(STATS_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last = Vec::new();
    loop {
        tokio::select! {
            msg = inbox.recv() => match msg {
                Some(Message::TunnelOpened { id }) => {
                    if let Some(reply) = lock(&st.pending).remove(&TunnelId(id)) {
                        let _ = reply.send(Ok(()));
                    }
                }
                Some(Message::TunnelClose { id, error }) => {
                    let id = TunnelId(id);
                    let reason = error
                        .as_deref()
                        .map_or(TunnelCloseReason::ClosedByHost, TunnelCloseReason::from_wire);
                    if let Some(reply) = lock(&st.pending).remove(&id) {
                        let _ = reply.send(Err(reason));
                    } else if st.remove(id) {
                        tracing::info!(%id, %reason, "host closed port forwarding");
                        st.ctx.emit(TUNNEL_FEATURE, TunnelEvent::Closed { id, reason });
                    }
                }
                Some(_) => {}
                None => break,
            },
            changed = perms.changed() => {
                if changed.is_err() {
                    break;
                }
                if !st.ctx.allowed(Permissions::TUNNEL) {
                    let ids: Vec<TunnelId> = lock(&st.rules).keys().copied().collect();
                    for id in ids {
                        if st.remove(id) {
                            st.ctx.emit(TUNNEL_FEATURE, TunnelEvent::Closed {
                                id,
                                reason: TunnelCloseReason::PermissionRevoked,
                            });
                        }
                    }
                }
            }
            _ = tick.tick() => {}
        }
        let now = st.statuses();
        if now != last {
            st.ctx
                .emit(TUNNEL_FEATURE, TunnelEvent::Changed(now.clone()));
            last = now;
        }
    }
}

/// Decrements the active-connection counters when a connection ends.
struct ActiveGuard {
    st: Arc<SessionState>,
    rule: Arc<Rule>,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.st.active.fetch_sub(1, Ordering::AcqRel);
        self.rule.active.fetch_sub(1, Ordering::AcqRel);
    }
}

async fn accept_loop(
    cfg: TunnelViewerConfig,
    st: Arc<SessionState>,
    rule: Arc<Rule>,
    listener: TcpListener,
) {
    let mut cancel = rule.cancel.subscribe();
    loop {
        let accepted = tokio::select! {
            _ = cancel.wait_for(|c| *c) => break,
            a = listener.accept() => a,
        };
        let (tcp, from) = match accepted {
            Ok(a) => a,
            Err(e) => {
                // E.g. out of sockets: do not spin.
                tracing::debug!(%e, id = %rule.id, "accept failed");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        rule.connections.fetch_add(1, Ordering::Relaxed);
        if st.active.fetch_add(1, Ordering::AcqRel) >= cfg.max_connections {
            st.active.fetch_sub(1, Ordering::AcqRel);
            rule.failed.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(id = %rule.id, %from, "tunnel connection limit reached; dropping");
            continue;
        }
        rule.active.fetch_add(1, Ordering::AcqRel);
        let guard = ActiveGuard {
            st: st.clone(),
            rule: rule.clone(),
        };
        tokio::spawn(connection(cfg.idle_timeout, guard, tcp));
    }
    tracing::debug!(id = %rule.id, "tunnel listener closed");
}

async fn connection(idle: Duration, guard: ActiveGuard, tcp: TcpStream) {
    let rule = guard.rule.clone();
    let mut cancel = rule.cancel.subscribe();
    let opened = tokio::select! {
        _ = cancel.wait_for(|c| *c) => return,
        o = guard.st.ctx.session.open_bi(StreamKind::Tunnel, rule.id.0) => o,
    };
    let (send, recv) = match opened {
        Ok(s) => s,
        Err(e) => {
            rule.failed.fetch_add(1, Ordering::Relaxed);
            if !matches!(e, NetError::Closed) {
                tracing::debug!(%e, id = %rule.id, "cannot open tunnel stream");
            }
            return;
        }
    };
    let conn = TunnelCounters::default();
    let cancelled = async move {
        let _ = cancel.wait_for(|c| *c).await;
    };
    let end = pipe_tcp(tcp, send, recv, idle, cancelled, &[&rule.counters, &conn]).await;
    if let PipeEnd::Error(e) = &end {
        // A reset before any reply usually means the host could not reach the target.
        if conn.snapshot().1 == 0 {
            rule.failed.fetch_add(1, Ordering::Relaxed);
        }
        tracing::debug!(id = %rule.id, %e, "tunnel connection failed");
    }
    drop(guard);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_reasons_map_from_the_wire() {
        assert_eq!(
            TunnelCloseReason::from_wire("target-not-allowed"),
            TunnelCloseReason::TargetNotAllowed
        );
        assert_eq!(
            TunnelCloseReason::from_wire("permission-revoked"),
            TunnelCloseReason::PermissionRevoked
        );
        assert_eq!(
            TunnelCloseReason::from_wire("رفض\u{7}"),
            TunnelCloseReason::Other("رفض".into())
        );
    }

    #[test]
    fn only_host_answers_are_handled() {
        let t = ViewerTunnels::new(TunnelViewerConfig::default());
        assert!(t.handles(&Message::TunnelOpened { id: 1 }));
        assert!(t.handles(&Message::TunnelClose { id: 1, error: None }));
        assert!(!t.handles(&Message::TunnelOpen {
            id: 1,
            target_port: 1,
            target_host: None
        }));
        assert!(t.list(1).is_empty());
        assert!(!t.stop(1, TunnelId(1)));
    }

    #[tokio::test]
    async fn add_refuses_unknown_session_and_unconfirmed_lan_bind() {
        let t = ViewerTunnels::new(TunnelViewerConfig::default());
        let lan = ForwardSpec {
            bind: "0.0.0.0:0".parse().unwrap(),
            allow_lan_bind: false,
            target_host: None,
            target_port: 80,
        };
        assert!(matches!(
            t.add(1, lan).await,
            Err(ViewerError::TunnelLanBind(_))
        ));
        assert!(matches!(
            t.add(1, ForwardSpec::local(0, None, 80)).await,
            Err(ViewerError::SessionEnded)
        ));
    }
}
