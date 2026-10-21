//! The relay server (`tlp-desk-relay` binary, and in-process for tests): a control socket
//! that answers `Register` / `Connect`, and one UDP session port per live session that
//! copies packets between the two punched addresses. See the module docs of
//! [`super`](crate::relay) for the exchange.
//!
//! Limits (every byte here comes from the internet): control frames over [`MAX_FRAME`]
//! bytes are dropped before parsing, a session port forwards at most [`MAX_PACKET`] bytes
//! per datagram, registrations expire after [`REGISTER_TTL`], a session that is not punched
//! by both sides within [`PUNCH_WINDOW`] or idle for [`IDLE`] gives its port back, and one
//! address may ask for at most [`CONNECTS_PER_MINUTE`] sessions a minute.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::ops::RangeInclusive;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::{timeout, Instant};

use super::{decode, encode, now_ts, Payload, Role, Token, MAX_FRAME};

/// A registration older than this is gone (hosts re-register every 15 s).
pub const REGISTER_TTL: Duration = Duration::from_secs(45);
/// A session port that both sides have not punched within this is freed.
pub const PUNCH_WINDOW: Duration = Duration::from_secs(30);
/// A session port with no traffic for this long is freed.
pub const IDLE: Duration = Duration::from_secs(60);
/// Largest datagram a session port forwards (QUIC keeps under 1500).
pub const MAX_PACKET: usize = 2048;
/// `Connect` requests one address may make per minute.
pub const CONNECTS_PER_MINUTE: u32 = 20;
/// Registered devices kept at most (the oldest are dropped past this).
pub const MAX_HOSTS: usize = 10_000;

/// Relay settings.
#[derive(Clone, Debug)]
pub struct RelayServerConfig {
    /// Control socket, e.g. `0.0.0.0:47402`.
    pub listen: SocketAddr,
    /// Session ports (same IP as `listen`); one per live session.
    pub ports: RangeInclusive<u16>,
    pub token: Token,
}

struct Registration {
    addr: SocketAddr,
    seen: Instant,
}

struct State {
    hosts: HashMap<u32, Registration>,
    free_ports: Vec<u16>,
    live: usize,
    connects: HashMap<IpAddr, (Instant, u32)>,
    next_session: u64,
}

/// A running relay. Drop it (or abort [`RelayServer::run`]) to stop.
pub struct RelayServer {
    control: Arc<UdpSocket>,
    cfg: RelayServerConfig,
    state: Arc<Mutex<State>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl RelayServer {
    /// Binds the control socket.
    pub async fn bind(cfg: RelayServerConfig) -> std::io::Result<Self> {
        let control = Arc::new(UdpSocket::bind(cfg.listen).await?);
        let mut free_ports: Vec<u16> = cfg.ports.clone().collect();
        free_ports.reverse(); // pop() hands out the lowest first
        let seed = now_ts().wrapping_mul(0x9E37_79B9_7F4A_7C15);
        Ok(Self {
            control,
            cfg,
            state: Arc::new(Mutex::new(State {
                hosts: HashMap::new(),
                free_ports,
                live: 0,
                connects: HashMap::new(),
                next_session: seed,
            })),
        })
    }

    /// The control socket's address (the port is useful after binding to port 0).
    pub fn control_addr(&self) -> std::io::Result<SocketAddr> {
        self.control.local_addr()
    }

    /// Registered devices right now (for status output and tests).
    pub fn registered(&self) -> usize {
        let mut s = lock(&self.state);
        expire(&mut s);
        s.hosts.len()
    }

    /// Live sessions right now.
    pub fn live_sessions(&self) -> usize {
        lock(&self.state).live
    }

    /// Serves until aborted.
    pub async fn run(&self) {
        let mut buf = vec![0u8; MAX_FRAME + 1];
        loop {
            let (n, from) = match self.control.recv_from(&mut buf).await {
                Ok(x) => x,
                Err(e) => {
                    tracing::warn!(%e, "relay: control socket error");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            };
            let Some(msg) = buf
                .get(..n)
                .and_then(|b| decode(&self.cfg.token, b, now_ts()))
            else {
                continue;
            };
            match msg {
                Payload::Register { device_id } => self.on_register(device_id, from).await,
                Payload::Connect { device_id } => self.on_connect(device_id, from).await,
                _ => {}
            }
        }
    }

    async fn on_register(&self, device_id: u32, from: SocketAddr) {
        {
            let mut s = lock(&self.state);
            expire(&mut s);
            if s.hosts.len() >= MAX_HOSTS && !s.hosts.contains_key(&device_id) {
                tracing::warn!("relay: registration table full");
                return;
            }
            let fresh = s
                .hosts
                .insert(
                    device_id,
                    Registration {
                        addr: from,
                        seen: Instant::now(),
                    },
                )
                .is_none();
            if fresh {
                tracing::info!(device_id, %from, "relay: device registered");
            }
        }
        self.send(from, &Payload::Registered).await;
    }

    async fn on_connect(&self, device_id: u32, from: SocketAddr) {
        enum Decision {
            Dropped,
            Offline,
            NoPort,
            Go {
                host: SocketAddr,
                port: u16,
                session: u64,
            },
        }
        let decision = {
            let mut s = lock(&self.state);
            expire(&mut s);
            if !allow_connect(&mut s, from.ip()) {
                Decision::Dropped
            } else if let Some(host) = s.hosts.get(&device_id).map(|r| r.addr) {
                if let Some(port) = s.free_ports.pop() {
                    s.next_session = s.next_session.wrapping_add(0x2545_F491_4F6C_DD1D);
                    s.live += 1;
                    Decision::Go {
                        host,
                        port,
                        session: s.next_session,
                    }
                } else {
                    Decision::NoPort
                }
            } else {
                Decision::Offline
            }
        };
        let (host, port, session) = match decision {
            Decision::Dropped => {
                tracing::warn!(%from, "relay: too many connect requests; dropped");
                return;
            }
            Decision::Offline => {
                tracing::info!(device_id, %from, "relay: device offline");
                self.send(from, &Payload::Offline).await;
                return;
            }
            Decision::NoPort => {
                tracing::warn!(%from, "relay: no free session port");
                self.send(from, &Payload::Offline).await;
                return;
            }
            Decision::Go {
                host,
                port,
                session,
            } => (host, port, session),
        };
        let bind = SocketAddr::new(self.cfg.listen.ip(), port);
        let sock = match UdpSocket::bind(bind).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(%e, %bind, "relay: session port unusable");
                // Not returned to the pool: something else owns it.
                {
                    let mut st = lock(&self.state);
                    st.live = st.live.saturating_sub(1);
                }
                self.send(from, &Payload::Offline).await;
                return;
            }
        };
        tracing::info!(device_id, session, port, viewer = %from, "relay: session allocated");
        let task = SessionTask {
            sock,
            session,
            viewer_control: from,
            control: self.control.clone(),
            token: self.cfg.token.clone(),
            state: self.state.clone(),
            port,
        };
        tokio::spawn(task.run());
        self.send(from, &Payload::Allocated { session, port }).await;
        self.send(host, &Payload::Incoming { session, port }).await;
    }

    async fn send(&self, to: SocketAddr, p: &Payload) {
        if let Err(e) = self.control.send_to(&encode(&self.cfg.token, p), to).await {
            tracing::debug!(%e, %to, "relay: control send failed");
        }
    }
}

fn expire(s: &mut State) {
    let now = Instant::now();
    s.hosts
        .retain(|_, r| now.saturating_duration_since(r.seen) < REGISTER_TTL);
    s.connects
        .retain(|_, (t, _)| now.saturating_duration_since(*t) < Duration::from_secs(60));
}

fn allow_connect(s: &mut State, ip: IpAddr) -> bool {
    let now = Instant::now();
    let e = s.connects.entry(ip).or_insert((now, 0));
    if now.saturating_duration_since(e.0) >= Duration::from_secs(60) {
        *e = (now, 0);
    }
    e.1 = e.1.saturating_add(1);
    e.1 <= CONNECTS_PER_MINUTE
}

struct SessionTask {
    sock: UdpSocket,
    session: u64,
    viewer_control: SocketAddr,
    control: Arc<UdpSocket>,
    token: Token,
    state: Arc<Mutex<State>>,
    port: u16,
}

impl SessionTask {
    async fn run(self) {
        let mut host: Option<SocketAddr> = None;
        let mut viewer: Option<SocketAddr> = None;
        let mut ready = false;
        let mut buf = vec![0u8; MAX_PACKET];
        let started = Instant::now();
        let mut last = Instant::now();
        let (mut up, mut down) = (0u64, 0u64);
        loop {
            let wait = if ready { IDLE } else { PUNCH_WINDOW };
            let left = wait.saturating_sub(last.elapsed());
            let Ok(Ok((n, from))) = timeout(left, self.sock.recv_from(&mut buf)).await else {
                break;
            };
            let Some(pkt) = buf.get(..n) else { continue };
            last = Instant::now();
            // Forwarding first: once both sides are known, every packet from them is theirs.
            if ready {
                let to = if Some(from) == host {
                    viewer
                } else if Some(from) == viewer {
                    host
                } else {
                    None
                };
                if let Some(to) = to {
                    if Some(from) == host {
                        down = down.saturating_add(n as u64);
                    } else {
                        up = up.saturating_add(n as u64);
                    }
                    if let Err(e) = self.sock.send_to(pkt, to).await {
                        tracing::debug!(%e, "relay: forward failed");
                    }
                }
                continue;
            }
            match decode(&self.token, pkt, now_ts()) {
                Some(Payload::Punch { session, role }) if session == self.session => {
                    match role {
                        Role::Host => host = Some(from),
                        Role::Viewer => viewer = Some(from),
                    }
                    if host.is_some() && viewer.is_some() {
                        ready = true;
                        tracing::info!(
                            session = self.session,
                            ?host,
                            ?viewer,
                            "relay: session ready"
                        );
                        let msg = encode(
                            &self.token,
                            &Payload::Ready {
                                session: self.session,
                            },
                        );
                        let _ = self.control.send_to(&msg, self.viewer_control).await;
                    }
                }
                _ => {
                    // Before both punches, nothing is forwarded (a QUIC Initial arriving
                    // early is retransmitted by the viewer).
                }
            }
        }
        let mut s = lock(&self.state);
        s.free_ports.push(self.port);
        s.live = s.live.saturating_sub(1);
        drop(s);
        tracing::info!(
            session = self.session,
            port = self.port,
            ready,
            secs = started.elapsed().as_secs(),
            up_bytes = up,
            down_bytes = down,
            "relay: session closed"
        );
    }
}
