//! Host side of TCP port forwarding (ROADMAP M4): the viewer asks with
//! `Message::TunnelOpen { id, target_port, target_host }`, the host checks the `TUNNEL`
//! permission and the target ([`tlp_desk_net::resolve_target`]: loopback and LAN only, never
//! the internet) and answers `TunnelOpened` or `TunnelClose { error }`. Every TCP connection
//! the viewer accepts then arrives as one `StreamKind::Tunnel` bidi stream with the tunnel id;
//! the host connects to the target and copies bytes both ways ([`tlp_desk_net::pipe_tcp`]).
//!
//! * **Permission:** `TUNNEL` is off by default ([`crate::DEFAULT_PERMISSIONS`]); the host user
//!   must grant it. Revoking it live closes every tunnel and connection of the session and
//!   tells the viewer (`TunnelClose { error: "permission-revoked" }`).
//! * **Limits:** [`TunnelHostConfig`] caps tunnels and concurrent connections per session,
//!   the connect time and idle connections.
//! * **UI:** [`HostTunnelEvent`]s arrive as `HostEvent::Feature` with feature name
//!   [`TUNNEL_FEATURE`]; [`HostTunnels::tunnels`] is the current list.
//!
//! Register one [`HostTunnels`] in `HostConfig::features`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tlp_desk_net::{
    close_reason, pipe_tcp, resolve_target, AllowList, Feature, FeatureCtx, PipeEnd, SideStream,
    StreamKind, TunnelCounters, STREAM_REFUSED,
};
use tlp_desk_proto::{Message, Permissions};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};

use crate::SessionId;

/// [`Feature::name`] of the tunnel feature (host and viewer use the same name).
pub const TUNNEL_FEATURE: &str = "tunnel";

/// How often the counters are re-published while tunnels exist.
const STATS_EVERY: Duration = Duration::from_secs(1);

/// Limits of the host tunnel feature.
#[derive(Debug, Clone)]
pub struct TunnelHostConfig {
    /// Where tunnels may lead (default: loopback, RFC 1918, link-local, IPv6 ULA; the same
    /// rules as the session allow-list). Public addresses are never reachable by default.
    pub allow_list: AllowList,
    /// Open tunnels per session (default 16).
    pub max_tunnels: usize,
    /// Concurrent forwarded connections per session, over all its tunnels (default 64).
    pub max_connections: usize,
    /// A connection without traffic in either direction for this long is closed (default
    /// 10 minutes).
    pub idle_timeout: Duration,
    /// How long the host tries to reach the target per connection (default 5 s).
    pub connect_timeout: Duration,
}

impl Default for TunnelHostConfig {
    fn default() -> Self {
        Self {
            allow_list: AllowList::lan_default(),
            max_tunnels: 16,
            max_connections: 64,
            idle_timeout: Duration::from_secs(600),
            connect_timeout: Duration::from_secs(5),
        }
    }
}

/// One open tunnel as the host user sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostTunnelInfo {
    /// Tunnel id chosen by the viewer.
    pub id: u64,
    /// Where its connections go (as seen from the host).
    pub target: SocketAddr,
    /// Connections open right now.
    pub active: usize,
    /// Connections served so far (including failed ones).
    pub connections: u64,
    /// Connections the host could not establish to the target.
    pub failed: u64,
    /// Bytes from the viewer to the target.
    pub bytes_to_target: u64,
    /// Bytes from the target to the viewer.
    pub bytes_from_target: u64,
}

/// Payload of `HostEvent::Feature` events with feature name [`TUNNEL_FEATURE`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostTunnelEvent {
    /// The session's tunnels or their counters changed (sent on change, at most about once
    /// per second for counters). An empty list means no tunnel is open.
    Changed(Vec<HostTunnelInfo>),
    /// A `TunnelOpen` was refused; `reason` is a [`close_reason`] value.
    Refused {
        id: u64,
        target_host: Option<String>,
        target_port: u16,
        reason: &'static str,
    },
}

/// The host tunnel feature. Cheap to share; register it once in `HostConfig::features`.
pub struct HostTunnels {
    cfg: TunnelHostConfig,
    sessions: Arc<Mutex<HashMap<u64, Arc<SessionTunnels>>>>,
}

impl std::fmt::Debug for HostTunnels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostTunnels")
            .field("cfg", &self.cfg)
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl HostTunnels {
    pub fn new(cfg: TunnelHostConfig) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            sessions: Arc::default(),
        })
    }

    /// The open tunnels of a session (empty if none or the session is unknown).
    pub fn tunnels(&self, session: SessionId) -> Vec<HostTunnelInfo> {
        lock(&self.sessions)
            .get(&session.0)
            .map(|s| s.infos())
            .unwrap_or_default()
    }
}

impl Feature for HostTunnels {
    fn name(&self) -> &'static str {
        TUNNEL_FEATURE
    }

    fn handles(&self, msg: &Message) -> bool {
        matches!(
            msg,
            Message::TunnelOpen { .. } | Message::TunnelClose { .. }
        )
    }

    fn start(&self, ctx: FeatureCtx, inbox: mpsc::UnboundedReceiver<Message>) {
        let st = Arc::new(SessionTunnels::default());
        lock(&self.sessions).insert(ctx.session_key, st.clone());
        let registry = self.sessions.clone();
        let cfg = self.cfg.clone();
        tokio::spawn(async move {
            let key = ctx.session_key;
            run(cfg, ctx, inbox, st).await;
            lock(&registry).remove(&key);
        });
    }
}

/// One open tunnel.
struct Tunnel {
    id: u64,
    target: SocketAddr,
    counters: TunnelCounters,
    active: AtomicUsize,
    connections: AtomicU64,
    failed: AtomicU64,
    /// `true` closes every connection of the tunnel.
    cancel: watch::Sender<bool>,
}

impl Tunnel {
    fn info(&self) -> HostTunnelInfo {
        let (to, from) = self.counters.snapshot();
        HostTunnelInfo {
            id: self.id,
            target: self.target,
            active: self.active.load(Ordering::Relaxed),
            connections: self.connections.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            bytes_to_target: to,
            bytes_from_target: from,
        }
    }

    fn close(&self) {
        self.cancel.send_replace(true);
    }
}

#[derive(Default)]
struct SessionTunnels {
    tunnels: Mutex<HashMap<u64, Arc<Tunnel>>>,
    /// Open connections over all tunnels of the session.
    active: AtomicUsize,
}

impl SessionTunnels {
    fn infos(&self) -> Vec<HostTunnelInfo> {
        let mut v: Vec<_> = lock(&self.tunnels).values().map(|t| t.info()).collect();
        v.sort_by_key(|t| t.id);
        v
    }

    fn close_all(&self) -> Vec<u64> {
        let drained: Vec<_> = lock(&self.tunnels).drain().collect();
        drained
            .into_iter()
            .map(|(id, t)| {
                t.close();
                id
            })
            .collect()
    }
}

/// Decrements the active-connection counters when a connection ends, however it ends.
struct ActiveGuard {
    st: Arc<SessionTunnels>,
    tunnel: Arc<Tunnel>,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.st.active.fetch_sub(1, Ordering::AcqRel);
        self.tunnel.active.fetch_sub(1, Ordering::AcqRel);
    }
}

async fn run(
    cfg: TunnelHostConfig,
    ctx: FeatureCtx,
    mut inbox: mpsc::UnboundedReceiver<Message>,
    st: Arc<SessionTunnels>,
) {
    let session = ctx.session.clone();
    let mut perms = ctx.permissions.clone();
    let mut tick = tokio::time::interval(STATS_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last: Vec<HostTunnelInfo> = Vec::new();
    loop {
        tokio::select! {
            msg = inbox.recv() => match msg {
                Some(Message::TunnelOpen { id, target_port, target_host }) => {
                    open(&cfg, &ctx, &st, id, target_port, target_host).await;
                }
                Some(Message::TunnelClose { id, .. }) => {
                    if let Some(t) = lock(&st.tunnels).remove(&id) {
                        tracing::info!(session = ctx.session_key, id, target = %t.target, "viewer closed tunnel");
                        t.close();
                    }
                }
                Some(_) => {}
                None => break,
            },
            changed = perms.changed() => {
                if changed.is_err() {
                    break;
                }
                if !ctx.allowed(Permissions::TUNNEL) {
                    revoke_all(&ctx, &st).await;
                }
            }
            stream = session.accept_side_stream(StreamKind::Tunnel) => match stream {
                Ok(s) => serve_stream(&cfg, &ctx, &st, s),
                Err(_) => break,
            },
            _ = tick.tick() => {}
        }
        let now = st.infos();
        if now != last {
            ctx.emit(TUNNEL_FEATURE, HostTunnelEvent::Changed(now.clone()));
            last = now;
        }
    }
    let closed = st.close_all();
    if !closed.is_empty() {
        tracing::info!(
            session = ctx.session_key,
            tunnels = closed.len(),
            "session ended; tunnels closed"
        );
    }
    if !last.is_empty() {
        ctx.emit(TUNNEL_FEATURE, HostTunnelEvent::Changed(Vec::new()));
    }
}

async fn open(
    cfg: &TunnelHostConfig,
    ctx: &FeatureCtx,
    st: &SessionTunnels,
    id: u64,
    target_port: u16,
    target_host: Option<String>,
) {
    let peer = ctx.session.peer().addr;
    let result = if !ctx.allowed(Permissions::TUNNEL) {
        Err(close_reason::PERMISSION_DENIED)
    } else {
        match resolve_target(target_host.as_deref(), target_port, &cfg.allow_list) {
            Err(e) => Err(e.reason()),
            Ok(target) => {
                let mut tunnels = lock(&st.tunnels);
                if tunnels.contains_key(&id) {
                    Err(close_reason::DUPLICATE_ID)
                } else if tunnels.len() >= cfg.max_tunnels {
                    Err(close_reason::TOO_MANY_TUNNELS)
                } else {
                    let (cancel, _) = watch::channel(false);
                    tunnels.insert(
                        id,
                        Arc::new(Tunnel {
                            id,
                            target,
                            counters: TunnelCounters::default(),
                            active: AtomicUsize::new(0),
                            connections: AtomicU64::new(0),
                            failed: AtomicU64::new(0),
                            cancel,
                        }),
                    );
                    Ok(target)
                }
            }
        }
    };
    let reply = match result {
        Ok(target) => {
            tracing::info!(session = ctx.session_key, %peer, id, %target, "tunnel opened");
            Message::TunnelOpened { id }
        }
        Err(reason) => {
            // The requested host is peer data: log it with Debug quoting, never raw.
            tracing::warn!(session = ctx.session_key, %peer, id, target_host = ?target_host, target_port, reason, "tunnel refused");
            ctx.emit(
                TUNNEL_FEATURE,
                HostTunnelEvent::Refused {
                    id,
                    target_host,
                    target_port,
                    reason,
                },
            );
            Message::TunnelClose {
                id,
                error: Some(reason.to_owned()),
            }
        }
    };
    if let Err(e) = ctx.session.send(&reply).await {
        tracing::debug!(%e, id, "could not answer TunnelOpen");
    }
}

/// `TUNNEL` was revoked: close everything and tell the viewer per tunnel.
async fn revoke_all(ctx: &FeatureCtx, st: &SessionTunnels) {
    let closed = st.close_all();
    if closed.is_empty() {
        return;
    }
    tracing::info!(
        session = ctx.session_key,
        tunnels = closed.len(),
        "TUNNEL revoked; closing all tunnels"
    );
    for id in closed {
        let msg = Message::TunnelClose {
            id,
            error: Some(close_reason::PERMISSION_REVOKED.to_owned()),
        };
        if let Err(e) = ctx.session.send(&msg).await {
            tracing::debug!(%e, id, "could not send TunnelClose");
            return;
        }
    }
}

fn refuse(mut s: SideStream, why: &str) {
    tracing::debug!(id = s.id, why, "refusing tunnel stream");
    let _ = s.recv.stop(STREAM_REFUSED.into());
    if let Some(send) = s.send.as_mut() {
        let _ = send.reset(STREAM_REFUSED.into());
    }
}

/// One forwarded TCP connection: check, then connect and pipe on its own task.
fn serve_stream(cfg: &TunnelHostConfig, ctx: &FeatureCtx, st: &Arc<SessionTunnels>, s: SideStream) {
    if !ctx.allowed(Permissions::TUNNEL) {
        return refuse(s, "no TUNNEL permission");
    }
    let Some(tunnel) = lock(&st.tunnels).get(&s.id).cloned() else {
        return refuse(s, "unknown or closed tunnel");
    };
    if s.send.is_none() {
        return refuse(s, "tunnel streams are bidirectional");
    }
    // Reserve a slot first, give it back if that went over the limit.
    if st.active.fetch_add(1, Ordering::AcqRel) >= cfg.max_connections {
        st.active.fetch_sub(1, Ordering::AcqRel);
        tracing::warn!(
            session = ctx.session_key,
            id = s.id,
            "tunnel connection limit reached"
        );
        return refuse(s, "connection limit");
    }
    tunnel.active.fetch_add(1, Ordering::AcqRel);
    tunnel.connections.fetch_add(1, Ordering::Relaxed);
    let guard = ActiveGuard {
        st: st.clone(),
        tunnel: tunnel.clone(),
    };
    let idle = cfg.idle_timeout;
    let connect_timeout = cfg.connect_timeout;
    let session_key = ctx.session_key;
    tokio::spawn(async move {
        let _guard = guard;
        let mut cancel = tunnel.cancel.subscribe();
        let SideStream {
            id,
            send: Some(send),
            recv,
            ..
        } = s
        else {
            return;
        };
        let connect = tokio::time::timeout(connect_timeout, TcpStream::connect(tunnel.target));
        let tcp = tokio::select! {
            r = connect => match r {
                Ok(Ok(tcp)) => tcp,
                Ok(Err(e)) => {
                    tunnel.failed.fetch_add(1, Ordering::Relaxed);
                    tracing::info!(session = session_key, id, target = %tunnel.target, %e, "tunnel target unreachable");
                    return refuse(SideStream { kind: StreamKind::Tunnel, id, send: Some(send), recv }, "connect failed");
                }
                Err(_) => {
                    tunnel.failed.fetch_add(1, Ordering::Relaxed);
                    tracing::info!(session = session_key, id, target = %tunnel.target, "tunnel target timed out");
                    return refuse(SideStream { kind: StreamKind::Tunnel, id, send: Some(send), recv }, "connect timed out");
                }
            },
            _ = cancel.wait_for(|c| *c) => {
                return refuse(SideStream { kind: StreamKind::Tunnel, id, send: Some(send), recv }, "tunnel closed");
            }
        };
        let cancelled = async move {
            let _ = cancel.wait_for(|c| *c).await;
        };
        let end = pipe_tcp(tcp, send, recv, idle, cancelled, &[&tunnel.counters]).await;
        match end {
            PipeEnd::Done | PipeEnd::Cancelled => {
                tracing::debug!(session = session_key, id, ?end, "tunnel connection ended")
            }
            PipeEnd::Idle => {
                tracing::info!(session = session_key, id, "idle tunnel connection closed")
            }
            PipeEnd::Error(e) => {
                tracing::debug!(session = session_key, id, %e, "tunnel connection failed")
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tunnel_requests_are_handled() {
        let t = HostTunnels::new(TunnelHostConfig::default());
        assert!(t.handles(&Message::TunnelOpen {
            id: 1,
            target_port: 80,
            target_host: None
        }));
        assert!(t.handles(&Message::TunnelClose { id: 1, error: None }));
        assert!(!t.handles(&Message::TunnelOpened { id: 1 }));
        assert!(!t.handles(&Message::RequestKeyframe));
        assert_eq!(t.name(), TUNNEL_FEATURE);
        assert!(t.tunnels(SessionId(7)).is_empty());
    }

    #[test]
    fn tunnel_is_off_by_default() {
        assert!(!crate::DEFAULT_PERMISSIONS.contains(Permissions::TUNNEL));
    }
}
