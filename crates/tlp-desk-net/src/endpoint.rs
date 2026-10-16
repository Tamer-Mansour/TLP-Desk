use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use tlp_desk_crypto::pake::{PakeContext, PakeMethod, PakeRole, PakeSession};
use tlp_desk_crypto::{
    DeviceId, Fingerprint, Identity, OneTimeCode, PasswordVerifier, RateLimitConfig, RateLimiter,
};
use tlp_desk_proto::{
    AuthRequest, AuthResult, Hello, Message, Permissions, RejectReason, Role, PROTOCOL_VERSION,
};
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::allow::{normalize, AllowList};
use crate::session::{write_msg, FramedRecv, PeerInfo, Session, CLOSE_PROTOCOL, CLOSE_REJECTED};
use crate::tls::{self, Pin};
use crate::{NetError, DEFAULT_PORT};

/// How the viewer wants to be authorized.
#[derive(Clone, PartialEq, Eq)]
pub enum AuthMethod {
    /// Ask the user at the host to click Accept.
    AcceptPrompt,
    /// The host's one-time 6-digit code (spaces allowed, e.g. `"042 917"`).
    OneTimeCode(String),
    /// The host's permanent unattended-access password.
    Password(String),
    /// A single-use reconnect ticket the host sent earlier (`Message::ReconnectTicket`), e.g.
    /// to get back in after the host restarted. Only works from the same viewer identity.
    ReconnectTicket(Vec<u8>),
}

impl std::fmt::Debug for AuthMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // never log secrets
        f.write_str(match self {
            AuthMethod::AcceptPrompt => "AcceptPrompt",
            AuthMethod::OneTimeCode(_) => "OneTimeCode(..)",
            AuthMethod::Password(_) => "Password(..)",
            AuthMethod::ReconnectTicket(_) => "ReconnectTicket(..)",
        })
    }
}

/// Viewer-side progress callback: called once the host has received an
/// [`AuthMethod::AcceptPrompt`] request, i.e. the host user is now being asked to accept.
/// Local-only (no wire change); may be called from any tokio worker.
pub type AwaitingAcceptFn = Arc<dyn Fn() + Send + Sync>;

/// What a connecting viewer asked for (host side).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind {
    AcceptPrompt,
    OneTimeCode,
    Password,
    /// A reconnect ticket (see [`PendingSession::redeem_ticket`]).
    ReconnectTicket,
}

/// Endpoint settings.
#[derive(Debug, Clone)]
pub struct EndpointConfig {
    /// UDP bind address. Default `[::]:47400` (dual-stack; falls back to `0.0.0.0:47400`).
    /// Use port 0 for an ephemeral port (tests, viewer-only).
    pub bind: SocketAddr,
    /// Name sent in `Hello`.
    pub device_name: String,
    /// Peers allowed to connect to us (host side). Default: LAN ranges only.
    pub allow_list: AllowList,
    /// Code/password attempt limits per peer IP.
    pub rate_limit: RateLimitConfig,
    /// Accept incoming sessions (host role). `false` = dial-only.
    pub listen: bool,
    /// Limit for the QUIC + `Hello` + `AuthRequest` phase and for key confirmation.
    pub handshake_timeout: Duration,
}

impl Default for EndpointConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), DEFAULT_PORT),
            device_name: default_device_name(),
            allow_list: AllowList::default(),
            rate_limit: RateLimitConfig::default(),
            listen: true,
            handshake_timeout: Duration::from_secs(10),
        }
    }
}

/// The computer name (`COMPUTERNAME` / `HOSTNAME`), or `"tlp-desk"`.
pub fn default_device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "tlp-desk".to_owned())
}

struct Shared {
    identity: Arc<Identity>,
    device_name: String,
    allow: RwLock<AllowList>,
    limiter: Mutex<RateLimiter>,
    handshake_timeout: Duration,
}

impl Shared {
    fn hello(&self, role: Role) -> Message {
        Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: self.identity.device_id().as_u32(),
            device_name: self.device_name.clone(),
            role,
        })
    }
}

/// A QUIC endpoint on one UDP socket that can both accept sessions (host) and dial (viewer).
///
/// Must be created inside a tokio runtime.
pub struct Endpoint {
    ep: quinn::Endpoint,
    shared: Arc<Shared>,
    incoming: tokio::sync::Mutex<mpsc::Receiver<PendingSession>>,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("device_id", &self.shared.identity.device_id())
            .field("local_addr", &self.ep.local_addr().ok())
            .finish_non_exhaustive()
    }
}

fn bind_socket(addr: SocketAddr) -> std::io::Result<std::net::UdpSocket> {
    if let IpAddr::V6(v6) = addr.ip() {
        if v6.is_unspecified() {
            let try_dual = || -> std::io::Result<std::net::UdpSocket> {
                let s = socket2::Socket::new(
                    socket2::Domain::IPV6,
                    socket2::Type::DGRAM,
                    Some(socket2::Protocol::UDP),
                )?;
                s.set_only_v6(false)?;
                s.bind(&addr.into())?;
                Ok(s.into())
            };
            match try_dual() {
                Ok(s) => return Ok(s),
                // The port is taken (e.g. another instance). Do not fall back to IPv4: on
                // Windows `0.0.0.0:port` can still bind next to a dual-stack `[::]:port`,
                // and two instances would silently share the port.
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => return Err(e),
                Err(e) => {
                    tracing::debug!(%e, "dual-stack bind failed, falling back to IPv4");
                    return std::net::UdpSocket::bind((
                        std::net::Ipv4Addr::UNSPECIFIED,
                        addr.port(),
                    ));
                }
            }
        }
    }
    std::net::UdpSocket::bind(addr)
}

impl Endpoint {
    /// Binds the UDP socket and, if `cfg.listen`, starts accepting sessions in the background.
    pub fn bind(identity: Arc<Identity>, cfg: EndpointConfig) -> Result<Self, NetError> {
        let socket = bind_socket(cfg.bind)?;
        let server = if cfg.listen {
            Some(tls::server_config(&identity)?)
        } else {
            None
        };
        let ep = quinn::Endpoint::new(
            quinn::EndpointConfig::default(),
            server,
            socket,
            Arc::new(quinn::TokioRuntime),
        )?;
        let shared = Arc::new(Shared {
            identity,
            device_name: cfg.device_name,
            allow: RwLock::new(cfg.allow_list),
            limiter: Mutex::new(RateLimiter::new(cfg.rate_limit)),
            handshake_timeout: cfg.handshake_timeout,
        });
        let (tx, rx) = mpsc::channel(16);
        if cfg.listen {
            tokio::spawn(accept_loop(ep.clone(), shared.clone(), tx));
        }
        tracing::info!(
            device_id = %shared.identity.device_id(),
            addr = ?ep.local_addr().ok(),
            listen = cfg.listen,
            "endpoint bound"
        );
        Ok(Self {
            ep,
            shared,
            incoming: tokio::sync::Mutex::new(rx),
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        Ok(self.ep.local_addr()?)
    }

    pub fn device_id(&self) -> DeviceId {
        self.shared.identity.device_id()
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.shared.identity.fingerprint()
    }

    /// Replaces the allow-list at runtime (applies to new connections).
    pub fn set_allow_list(&self, list: AllowList) {
        *self.shared.allow.write().unwrap_or_else(|p| p.into_inner()) = list;
    }

    /// Host: next viewer that completed TLS + `Hello` + `AuthRequest` and now awaits a
    /// decision. `None` once the endpoint is closed (or it was bound with `listen: false`).
    pub async fn accept(&self) -> Option<PendingSession> {
        self.incoming.lock().await.recv().await
    }

    /// Viewer: dial `addr`. With `expected_id`, the host key must hash to that device ID
    /// (reject on mismatch); without it, trust-on-first-use (check
    /// `session.peer().fingerprint` against `KnownHosts`). See [`Endpoint::connect_pinned`].
    pub async fn connect(
        &self,
        addr: SocketAddr,
        expected_id: Option<DeviceId>,
        auth: AuthMethod,
    ) -> Result<Session, NetError> {
        self.connect_pinned(addr, Pin::from(expected_id), auth)
            .await
    }

    /// Viewer: dial `addr` with an explicit [`Pin`] (use `Pin::Fingerprint` when the
    /// known-hosts store has the full key; it is much stronger than the 9-digit ID).
    pub async fn connect_pinned(
        &self,
        addr: SocketAddr,
        pin: Pin,
        auth: AuthMethod,
    ) -> Result<Session, NetError> {
        self.connect_pinned_with_progress(addr, pin, auth, None)
            .await
    }

    /// [`Endpoint::connect_pinned`] with a progress hook: `on_awaiting_accept` runs right
    /// after the accept-prompt request was sent over the authenticated connection (only for
    /// [`AuthMethod::AcceptPrompt`]), so a UI can switch to "waiting for the other side".
    pub async fn connect_pinned_with_progress(
        &self,
        addr: SocketAddr,
        pin: Pin,
        auth: AuthMethod,
        on_awaiting_accept: Option<AwaitingAcceptFn>,
    ) -> Result<Session, NetError> {
        let (cfg, observed) = tls::client_config(&self.shared.identity, pin)?;
        let connecting = self.ep.connect_with(cfg, addr, "tlp-desk")?;
        let conn = match connecting.await {
            Ok(c) => c,
            Err(e) => {
                let seen = *observed.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(fp) = seen.filter(|fp| !pin.matches(fp)) {
                    return Err(NetError::PinMismatch {
                        expected: pin.describe(),
                        actual_id: fp.device_id(),
                        actual_fp: fp,
                    });
                }
                return Err(e.into());
            }
        };
        let host_fp = tls::peer_fingerprint(&conn)?;
        if !pin.matches(&host_fp) {
            conn.close(CLOSE_REJECTED.into(), b"pin");
            return Err(NetError::PinMismatch {
                expected: pin.describe(),
                actual_id: host_fp.device_id(),
                actual_fp: host_fp,
            });
        }
        let res = self
            .viewer_handshake(&conn, host_fp, auth, on_awaiting_accept)
            .await;
        if let Err(e) = &res {
            let code = match e {
                NetError::Rejected(_) => CLOSE_REJECTED,
                _ => CLOSE_PROTOCOL,
            };
            conn.close(code.into(), b"");
        }
        res
    }

    async fn viewer_handshake(
        &self,
        conn: &quinn::Connection,
        host_fp: Fingerprint,
        auth: AuthMethod,
        on_awaiting_accept: Option<AwaitingAcceptFn>,
    ) -> Result<Session, NetError> {
        let t = self.shared.handshake_timeout;
        let (mut send, recv) = conn.open_bi().await?;
        let mut rx = FramedRecv::new(recv);
        write_msg(&mut send, &self.shared.hello(Role::Viewer)).await?;
        let hello = match timeout(t, rx.next()).await?? {
            Message::Hello(h) => h,
            Message::AuthResult(AuthResult::Rejected { reason }) => {
                return Err(NetError::Rejected(reason))
            }
            other => return Err(unexpected(&other)),
        };
        if hello.protocol_version != PROTOCOL_VERSION {
            return Err(NetError::VersionMismatch {
                ours: PROTOCOL_VERSION,
                theirs: hello.protocol_version,
            });
        }
        if hello.device_id != host_fp.device_id().as_u32() || hello.role != Role::Host {
            return Err(NetError::Unexpected(
                "host Hello does not match its certificate".into(),
            ));
        }
        let peer = PeerInfo {
            addr: SocketAddr::new(
                normalize(conn.remote_address().ip()),
                conn.remote_address().port(),
            ),
            device_id: host_fp.device_id(),
            fingerprint: host_fp,
            name: sanitize_peer_name(&hello.device_name),
        };

        // Only a human answering a prompt may take long; everything else is bounded.
        let human_decides = matches!(auth, AuthMethod::AcceptPrompt);
        let pake = match auth {
            AuthMethod::ReconnectTicket(token) => {
                write_msg(
                    &mut send,
                    &Message::AuthRequest(AuthRequest::ReconnectTicket { token }),
                )
                .await?;
                None
            }
            AuthMethod::AcceptPrompt => {
                write_msg(&mut send, &Message::AuthRequest(AuthRequest::AcceptPrompt)).await?;
                if let Some(f) = &on_awaiting_accept {
                    f();
                }
                None
            }
            AuthMethod::OneTimeCode(code) => {
                let digits: String = code.chars().filter(|c| !c.is_whitespace()).collect();
                Some((PakeMethod::OneTimeCode, digits.into_bytes()))
            }
            AuthMethod::Password(pw) => {
                let v =
                    tokio::task::spawn_blocking(move || PasswordVerifier::derive(&pw, &host_fp))
                        .await
                        .map_err(|e| NetError::Unexpected(e.to_string()))??;
                Some((PakeMethod::Password, v.as_secret().to_vec()))
            }
        };

        if let Some((method, secret)) = pake {
            let ctx = PakeContext {
                method,
                viewer_fp: self.shared.identity.fingerprint(),
                host_fp,
                channel_binding: tls::channel_binding(conn)?,
            };
            let (state, msg) = PakeSession::start(PakeRole::Viewer, &secret, ctx);
            let req = match method {
                PakeMethod::OneTimeCode => AuthRequest::OneTimeCode { spake2_msg: msg },
                PakeMethod::Password => AuthRequest::Password { spake2_msg: msg },
            };
            write_msg(&mut send, &Message::AuthRequest(req)).await?;
            // The host may wait on nothing here, but bound it anyway.
            let reply = match timeout(t, rx.next()).await?? {
                Message::AuthResult(AuthResult::Spake2Reply { spake2_msg }) => spake2_msg,
                Message::AuthResult(AuthResult::Rejected { reason }) => {
                    return Err(NetError::Rejected(reason))
                }
                other => return Err(unexpected(&other)),
            };
            let keys = state
                .finish(&reply)
                .map_err(|e| NetError::Unexpected(e.to_string()))?;
            write_msg(
                &mut send,
                &Message::AuthRequest(AuthRequest::KeyConfirm {
                    mac: keys.my_confirmation(),
                }),
            )
            .await?;
            match timeout(t, rx.next()).await?? {
                Message::AuthResult(AuthResult::KeyConfirm { mac }) => {
                    if keys.verify_peer(&mac).is_err() {
                        tracing::warn!(host = %peer.device_id, "host failed key confirmation");
                        return Err(NetError::HostAuthFailed);
                    }
                }
                Message::AuthResult(AuthResult::Rejected { reason }) => {
                    tracing::info!(host = %peer.device_id, ?reason, "authentication rejected");
                    return Err(NetError::Rejected(reason));
                }
                other => return Err(unexpected(&other)),
            }
        }

        // Accept prompt: wait for the human, no timeout (drop the future to cancel).
        let answer = if human_decides {
            rx.next().await?
        } else {
            timeout(t, rx.next()).await??
        };
        let permissions = match answer {
            Message::AuthResult(AuthResult::Accepted { permissions }) => permissions,
            Message::AuthResult(AuthResult::Rejected { reason }) => {
                return Err(NetError::Rejected(reason))
            }
            other => return Err(unexpected(&other)),
        };
        tracing::info!(host = %peer.device_id, addr = %peer.addr, ?permissions, "session established (viewer)");
        Ok(Session::new(
            conn.clone(),
            Role::Viewer,
            peer,
            permissions,
            send,
            rx,
        ))
    }

    /// Test/fuzzing aid: dials `addr` and completes only the mutually authenticated TLS
    /// handshake (with `pin`), returning the raw QUIC connection before any `Hello`. Lets
    /// tests play a peer that speaks the protocol wrong.
    #[doc(hidden)]
    pub async fn dial_raw(
        &self,
        addr: SocketAddr,
        pin: Pin,
    ) -> Result<quinn::Connection, NetError> {
        let (cfg, _) = tls::client_config(&self.shared.identity, pin)?;
        Ok(self.ep.connect_with(cfg, addr, "tlp-desk")?.await?)
    }

    /// Test aid: QUIC connections currently open on this endpoint (handshaking, pending,
    /// in a session, or draining). Leak tests check it returns to zero.
    #[doc(hidden)]
    pub fn open_connections(&self) -> usize {
        self.ep.open_connections()
    }

    /// Stops accepting and closes all connections.
    pub fn close(&self) {
        self.ep.close(0u32.into(), b"shutdown");
    }

    /// Waits until all connections are cleanly shut down (call after [`Endpoint::close`]).
    pub async fn wait_idle(&self) {
        self.ep.wait_idle().await;
    }
}

/// Longest self-reported peer name (`Hello::device_name`) kept, in characters; the rest is
/// cut before the name reaches logs, prompts or the "who is connected" banner.
pub const MAX_PEER_NAME_CHARS: usize = 64;

/// Makes a peer's self-reported name safe to show: control characters (newlines, NUL, bell,
/// escapes) are dropped, the length is capped at [`MAX_PEER_NAME_CHARS`] and the ends are
/// trimmed. Letters of any script and bidi marks (RLM, ZWJ) are kept exactly.
pub fn sanitize_peer_name(name: &str) -> String {
    let cut: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_PEER_NAME_CHARS)
        .collect();
    cut.trim().to_owned()
}

/// `NetError::Unexpected` naming the message, formatted lazily and cut at 80 characters, so a
/// hostile 16 MiB message is never rendered in full.
fn unexpected(m: &Message) -> NetError {
    use std::fmt::Write as _;
    /// Collects at most `left` characters, then stops the formatter.
    struct Capped {
        out: String,
        left: usize,
    }
    impl std::fmt::Write for Capped {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            for c in s.chars() {
                if self.left == 0 {
                    return Err(std::fmt::Error);
                }
                self.out.push(c);
                self.left -= 1;
            }
            Ok(())
        }
    }
    let mut w = Capped {
        out: String::new(),
        left: 80,
    };
    let _ = write!(w, "{m:?}");
    NetError::Unexpected(w.out)
}

async fn accept_loop(ep: quinn::Endpoint, shared: Arc<Shared>, tx: mpsc::Sender<PendingSession>) {
    while let Some(incoming) = ep.accept().await {
        let addr = incoming.remote_address();
        let allowed = shared
            .allow
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .is_allowed(addr.ip());
        if !allowed {
            tracing::warn!(peer = %addr, "connection refused: peer not in allow-list");
            incoming.refuse();
            continue;
        }
        let shared = shared.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            match timeout(shared.handshake_timeout, host_handshake(incoming, &shared)).await {
                Ok(Ok(Some(p))) => {
                    let _ = tx.send(p).await;
                }
                Ok(Ok(None)) => {}
                Ok(Err(e)) => tracing::debug!(peer = %addr, %e, "incoming handshake failed"),
                Err(_) => tracing::debug!(peer = %addr, "incoming handshake timed out"),
            }
        });
    }
}

/// Sends `Rejected`, then gives the viewer a moment to read it and close before we close.
async fn reject_and_close(
    conn: &quinn::Connection,
    send: &mut quinn::SendStream,
    reason: RejectReason,
) {
    let _ = write_msg(send, &Message::AuthResult(AuthResult::Rejected { reason })).await;
    let _ = send.finish();
    let _ = timeout(Duration::from_secs(3), conn.closed()).await;
    conn.close(CLOSE_REJECTED.into(), b"rejected");
}

async fn host_handshake(
    incoming: quinn::Incoming,
    shared: &Arc<Shared>,
) -> Result<Option<PendingSession>, NetError> {
    let conn = incoming.await?;
    let viewer_fp = tls::peer_fingerprint(&conn)?;
    let remote = conn.remote_address();
    let addr = SocketAddr::new(normalize(remote.ip()), remote.port());
    let (mut send, recv) = conn.accept_bi().await?;
    let mut rx = FramedRecv::new(recv);
    let hello = match rx.next().await {
        Ok(Message::Hello(h)) => h,
        Ok(other) => {
            conn.close(CLOSE_PROTOCOL.into(), b"expected Hello");
            return Err(unexpected(&other));
        }
        Err(e) => {
            // Malformed or oversized frame (or the peer left): close explicitly instead of
            // leaving it to the idle timeout.
            conn.close(CLOSE_PROTOCOL.into(), b"bad Hello");
            return Err(e);
        }
    };
    if hello.protocol_version != PROTOCOL_VERSION {
        tracing::warn!(peer = %addr, theirs = hello.protocol_version, ours = PROTOCOL_VERSION, "protocol version mismatch");
        reject_and_close(&conn, &mut send, RejectReason::VersionMismatch).await;
        return Ok(None);
    }
    if hello.device_id != viewer_fp.device_id().as_u32() || hello.role != Role::Viewer {
        tracing::warn!(peer = %addr, "viewer Hello does not match its certificate");
        reject_and_close(&conn, &mut send, RejectReason::NotAllowed).await;
        return Ok(None);
    }
    write_msg(&mut send, &shared.hello(Role::Host)).await?;
    // `payload`: the SPAKE2 message, or the reconnect ticket token.
    let request = match rx.next().await {
        Ok(m) => m,
        Err(e) => {
            conn.close(CLOSE_PROTOCOL.into(), b"bad AuthRequest");
            return Err(e);
        }
    };
    let (kind, payload) = match request {
        Message::AuthRequest(AuthRequest::AcceptPrompt) => (AuthKind::AcceptPrompt, Vec::new()),
        Message::AuthRequest(AuthRequest::OneTimeCode { spake2_msg }) => {
            (AuthKind::OneTimeCode, spake2_msg)
        }
        Message::AuthRequest(AuthRequest::Password { spake2_msg }) => {
            (AuthKind::Password, spake2_msg)
        }
        // Verified by the host app against its ticket store (`PendingSession::redeem_ticket`).
        Message::AuthRequest(AuthRequest::ReconnectTicket { token }) => {
            (AuthKind::ReconnectTicket, token)
        }
        other => {
            conn.close(CLOSE_PROTOCOL.into(), b"expected AuthRequest");
            return Err(unexpected(&other));
        }
    };
    if kind != AuthKind::AcceptPrompt {
        let locked = shared
            .limiter
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .locked_for(&addr.ip(), Instant::now());
        if let Some(left) = locked {
            tracing::warn!(peer = %addr, device_id = %viewer_fp.device_id(), ?kind, ?left, "auth attempt refused: locked out");
            reject_and_close(&conn, &mut send, RejectReason::RateLimited).await;
            return Ok(None);
        }
    }
    let peer = PeerInfo {
        addr,
        device_id: viewer_fp.device_id(),
        fingerprint: viewer_fp,
        name: sanitize_peer_name(&hello.device_name),
    };
    tracing::info!(peer = %peer.addr, device_id = %peer.device_id, name = %peer.name, ?kind, "incoming session request");
    Ok(Some(PendingSession {
        conn,
        send,
        rx,
        peer,
        kind,
        payload,
        shared: shared.clone(),
    }))
}

/// Host side: a viewer that is authenticated at the TLS layer (its device ID is genuine) and
/// waits for authorization. Dropping it closes the connection.
pub struct PendingSession {
    conn: quinn::Connection,
    send: quinn::SendStream,
    rx: FramedRecv,
    peer: PeerInfo,
    kind: AuthKind,
    /// SPAKE2 message (code/password) or ticket token; never logged.
    payload: Vec<u8>,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for PendingSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingSession")
            .field("peer", &self.peer)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy)]
enum Secret<'a> {
    Code(&'a OneTimeCode),
    Password(&'a PasswordVerifier),
}

impl PendingSession {
    pub fn peer(&self) -> &PeerInfo {
        &self.peer
    }

    /// What the viewer asked for; decides which method to call next.
    pub fn auth_kind(&self) -> AuthKind {
        self.kind
    }

    /// Resolves if the viewer disconnects while waiting (dismiss the Accept prompt).
    pub async fn cancelled(&self) {
        let _ = self.conn.closed().await;
    }

    /// The user clicked **Accept** (only for [`AuthKind::AcceptPrompt`]).
    pub async fn accept(self, permissions: Permissions) -> Result<Session, NetError> {
        if self.kind != AuthKind::AcceptPrompt {
            return Err(NetError::WrongAuthKind);
        }
        tracing::info!(peer = %self.peer.addr, device_id = %self.peer.device_id, "accept prompt: accepted");
        self.finish_accept(permissions).await
    }

    /// The user clicked **Reject** (or the host policy refuses).
    pub async fn reject(mut self, reason: RejectReason) {
        tracing::info!(peer = %self.peer.addr, device_id = %self.peer.device_id, ?reason, "session rejected");
        reject_and_close(&self.conn, &mut self.send, reason).await;
    }

    /// Runs SPAKE2 with the current one-time code (only for [`AuthKind::OneTimeCode`]).
    /// The code rotates on success and after repeated failures. On a wrong code the viewer
    /// receives `Rejected(WrongCredentials)` and this returns [`NetError::AuthFailed`].
    pub async fn verify_code(
        self,
        code: &OneTimeCode,
        permissions: Permissions,
    ) -> Result<Session, NetError> {
        if self.kind != AuthKind::OneTimeCode {
            return Err(NetError::WrongAuthKind);
        }
        self.verify(Secret::Code(code), permissions).await
    }

    /// Runs SPAKE2 with the stored password verifier (only for [`AuthKind::Password`]).
    pub async fn verify_password(
        self,
        verifier: &PasswordVerifier,
        permissions: Permissions,
    ) -> Result<Session, NetError> {
        if self.kind != AuthKind::Password {
            return Err(NetError::WrongAuthKind);
        }
        self.verify(Secret::Password(verifier), permissions).await
    }

    /// Checks the reconnect ticket the viewer presented (only for
    /// [`AuthKind::ReconnectTicket`]). `check(token, viewer_fingerprint)` consults the host's
    /// ticket store and returns the permissions to grant, or the reason to send back
    /// (`WrongCredentials` = give up, `NotAllowed` = not yet / try again later). Every check
    /// counts against the per-IP rate limit like a password attempt. On refusal this returns
    /// [`NetError::AuthFailed`].
    pub async fn redeem_ticket(
        mut self,
        check: impl FnOnce(&[u8], &Fingerprint) -> Result<Permissions, RejectReason>,
    ) -> Result<Session, NetError> {
        if self.kind != AuthKind::ReconnectTicket {
            return Err(NetError::WrongAuthKind);
        }
        let ip = self.peer.addr.ip();
        let attempt = self
            .shared
            .limiter
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .attempt(&ip, Instant::now());
        if let Err(e) = attempt {
            tracing::warn!(peer = %self.peer.addr, device_id = %self.peer.device_id, retry_after = ?e.retry_after, "reconnect ticket refused: rate limited");
            reject_and_close(&self.conn, &mut self.send, RejectReason::RateLimited).await;
            return Err(NetError::RateLimited);
        }
        let token = std::mem::take(&mut self.payload);
        match check(&token, &self.peer.fingerprint) {
            Ok(permissions) => {
                self.shared
                    .limiter
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .record_success(&ip);
                tracing::info!(peer = %self.peer.addr, device_id = %self.peer.device_id, "reconnect ticket accepted");
                self.finish_accept(permissions).await
            }
            Err(reason) => {
                tracing::warn!(peer = %self.peer.addr, device_id = %self.peer.device_id, ?reason, "reconnect ticket refused");
                reject_and_close(&self.conn, &mut self.send, reason).await;
                Err(NetError::AuthFailed)
            }
        }
    }

    async fn verify(
        mut self,
        secret: Secret<'_>,
        permissions: Permissions,
    ) -> Result<Session, NetError> {
        let ip = self.peer.addr.ip();
        let attempt = self
            .shared
            .limiter
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .attempt(&ip, Instant::now());
        if let Err(e) = attempt {
            tracing::warn!(peer = %self.peer.addr, device_id = %self.peer.device_id, kind = ?self.kind, retry_after = ?e.retry_after, "auth attempt refused: rate limited");
            reject_and_close(&self.conn, &mut self.send, RejectReason::RateLimited).await;
            return Err(NetError::RateLimited);
        }
        let (method, secret_bytes) = match secret {
            Secret::Code(c) => (PakeMethod::OneTimeCode, c.current().into_bytes()),
            Secret::Password(v) => (PakeMethod::Password, v.as_secret().to_vec()),
        };
        let ctx = PakeContext {
            method,
            viewer_fp: self.peer.fingerprint,
            host_fp: self.shared.identity.fingerprint(),
            channel_binding: tls::channel_binding(&self.conn)?,
        };
        let (state, msg_b) = PakeSession::start(PakeRole::Host, &secret_bytes, ctx);
        let outcome: Result<Option<_>, NetError> = async {
            let Ok(keys) = state.finish(&self.payload) else {
                return Ok(None);
            };
            write_msg(
                &mut self.send,
                &Message::AuthResult(AuthResult::Spake2Reply { spake2_msg: msg_b }),
            )
            .await?;
            match timeout(self.shared.handshake_timeout, self.rx.next()).await?? {
                Message::AuthRequest(AuthRequest::KeyConfirm { mac }) => {
                    Ok(keys.verify_peer(&mac).is_ok().then_some(keys))
                }
                other => Err(unexpected(&other)),
            }
        }
        .await;
        match outcome {
            Ok(Some(keys)) => {
                if let Secret::Code(c) = secret {
                    c.record_success();
                }
                self.shared
                    .limiter
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .record_success(&ip);
                tracing::info!(peer = %self.peer.addr, device_id = %self.peer.device_id, kind = ?self.kind, "auth attempt succeeded");
                write_msg(
                    &mut self.send,
                    &Message::AuthResult(AuthResult::KeyConfirm {
                        mac: keys.my_confirmation(),
                    }),
                )
                .await?;
                self.finish_accept(permissions).await
            }
            Ok(None) => {
                if let Secret::Code(c) = secret {
                    c.record_failure();
                }
                tracing::warn!(peer = %self.peer.addr, device_id = %self.peer.device_id, kind = ?self.kind, "auth attempt failed: wrong credentials");
                reject_and_close(&self.conn, &mut self.send, RejectReason::WrongCredentials).await;
                Err(NetError::AuthFailed)
            }
            Err(e) => {
                if let Secret::Code(c) = secret {
                    c.record_failure();
                }
                tracing::warn!(peer = %self.peer.addr, device_id = %self.peer.device_id, kind = ?self.kind, %e, "auth attempt failed: protocol error");
                self.conn.close(CLOSE_PROTOCOL.into(), b"auth");
                Err(e)
            }
        }
    }

    async fn finish_accept(mut self, permissions: Permissions) -> Result<Session, NetError> {
        write_msg(
            &mut self.send,
            &Message::AuthResult(AuthResult::Accepted { permissions }),
        )
        .await?;
        tracing::info!(peer = %self.peer.addr, device_id = %self.peer.device_id, ?permissions, "session established (host)");
        Ok(Session::new(
            self.conn,
            Role::Host,
            self.peer,
            permissions,
            self.send,
            self.rx,
        ))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unreachable)]
    use super::*;

    /// A peer speaking another protocol version gets `Rejected(VersionMismatch)`.
    #[tokio::test(flavor = "multi_thread")]
    async fn hello_version_mismatch_is_rejected() {
        let host = Endpoint::bind(
            Arc::new(Identity::generate().unwrap()),
            EndpointConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                ..EndpointConfig::default()
            },
        )
        .unwrap();
        let addr = host.local_addr().unwrap();
        let old = Identity::generate().unwrap();
        let (cfg, _) = tls::client_config(&old, Pin::DeviceId(host.device_id())).unwrap();
        let ep = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let conn = ep
            .connect_with(cfg, addr, "tlp-desk")
            .unwrap()
            .await
            .unwrap();
        let (mut send, recv) = conn.open_bi().await.unwrap();
        let mut rx = FramedRecv::new(recv);
        write_msg(
            &mut send,
            &Message::Hello(Hello {
                protocol_version: 1,
                device_id: old.device_id().as_u32(),
                device_name: "old".into(),
                role: Role::Viewer,
            }),
        )
        .await
        .unwrap();
        let reply = timeout(Duration::from_secs(10), rx.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            reply,
            Message::AuthResult(AuthResult::Rejected {
                reason: RejectReason::VersionMismatch
            })
        );
        conn.close(0u32.into(), b"");
        // the host app never sees it
        assert!(timeout(Duration::from_millis(200), host.accept())
            .await
            .is_err());
    }

    /// A viewer whose `Hello` claims a device ID that its certificate does not prove.
    #[tokio::test(flavor = "multi_thread")]
    async fn hello_with_spoofed_device_id_is_rejected() {
        let host = Endpoint::bind(
            Arc::new(Identity::generate().unwrap()),
            EndpointConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                ..EndpointConfig::default()
            },
        )
        .unwrap();
        let addr = host.local_addr().unwrap();
        let me = Identity::generate().unwrap();
        let (cfg, _) = tls::client_config(&me, Pin::Tofu).unwrap();
        let ep = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let conn = ep
            .connect_with(cfg, addr, "tlp-desk")
            .unwrap()
            .await
            .unwrap();
        let (mut send, recv) = conn.open_bi().await.unwrap();
        let mut rx = FramedRecv::new(recv);
        let spoofed = if me.device_id().as_u32() == 123_456_789 {
            987_654_321
        } else {
            123_456_789
        };
        write_msg(
            &mut send,
            &Message::Hello(Hello {
                protocol_version: PROTOCOL_VERSION,
                device_id: spoofed,
                device_name: "liar".into(),
                role: Role::Viewer,
            }),
        )
        .await
        .unwrap();
        let reply = timeout(Duration::from_secs(10), rx.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            reply,
            Message::AuthResult(AuthResult::Rejected {
                reason: RejectReason::NotAllowed
            })
        );
    }

    /// A reconnect ticket reaches the host app as a pending session of kind `ReconnectTicket`
    /// with the viewer's proven fingerprint; the app's verdict decides. The token never shows in
    /// `Debug` output.
    #[tokio::test(flavor = "multi_thread")]
    async fn reconnect_ticket_is_checked_by_the_host_app() {
        let host = Endpoint::bind(
            Arc::new(Identity::generate().unwrap()),
            EndpointConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                ..EndpointConfig::default()
            },
        )
        .unwrap();
        let addr = host.local_addr().unwrap();
        let viewer = Endpoint::bind(
            Arc::new(Identity::generate().unwrap()),
            EndpointConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                listen: false,
                ..EndpointConfig::default()
            },
        )
        .unwrap();
        let viewer_fp = viewer.fingerprint();
        let good = vec![7u8; 32];
        assert_eq!(
            format!("{:?}", AuthMethod::ReconnectTicket(good.clone())),
            "ReconnectTicket(..)"
        );
        let host_side = async {
            for want_ok in [false, true] {
                let p = timeout(Duration::from_secs(10), host.accept())
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(p.auth_kind(), AuthKind::ReconnectTicket);
                assert!(!format!("{p:?}").contains("7, 7, 7"));
                let good = good.clone();
                let res = p
                    .redeem_ticket(move |token, fp| {
                        assert_eq!(*fp, viewer_fp);
                        if token == good.as_slice() {
                            Ok(Permissions::VIEW)
                        } else {
                            Err(RejectReason::WrongCredentials)
                        }
                    })
                    .await;
                assert_eq!(res.is_ok(), want_ok);
                if let Ok(s) = res {
                    return s;
                }
            }
            unreachable!()
        };
        let viewer_side = async {
            let err = viewer
                .connect(
                    addr,
                    Some(host.device_id()),
                    AuthMethod::ReconnectTicket(vec![8; 32]),
                )
                .await
                .unwrap_err();
            assert!(
                matches!(err, NetError::Rejected(RejectReason::WrongCredentials)),
                "{err}"
            );
            viewer
                .connect(
                    addr,
                    Some(host.device_id()),
                    AuthMethod::ReconnectTicket(good.clone()),
                )
                .await
                .unwrap()
        };
        let (host_session, viewer_session) = tokio::join!(host_side, viewer_side);
        assert_eq!(viewer_session.initial_permissions(), Permissions::VIEW);
        assert_eq!(host_session.peer().fingerprint, viewer_fp);
    }

    #[test]
    fn peer_names_keep_arabic_and_lose_control_characters() {
        assert_eq!(sanitize_peer_name("مكتب-1 office"), "مكتب-1 office");
        // RLM and ZWJ are formatting characters (Cf), not controls: kept.
        assert_eq!(
            sanitize_peer_name("\u{200F}جهاز\u{200D}٣"),
            "\u{200F}جهاز\u{200D}٣"
        );
        assert_eq!(sanitize_peer_name(" a\nb\r\0c\u{1b}[31m "), "abc[31m");
        let long = "ب".repeat(1000);
        assert_eq!(
            sanitize_peer_name(&long).chars().count(),
            MAX_PEER_NAME_CHARS
        );
        assert_eq!(sanitize_peer_name("\n\t"), "");
    }

    #[test]
    fn unexpected_message_text_is_capped() {
        let big = Message::ChatMessage {
            id: 1,
            text: "ح".repeat(1 << 20),
            sent_unix_ms: 0,
        };
        let NetError::Unexpected(s) = unexpected(&big) else {
            panic!("wrong error kind");
        };
        assert_eq!(s.chars().count(), 80);
        assert!(s.starts_with("ChatMessage"), "{s}");
        let NetError::Unexpected(s) = unexpected(&Message::RequestKeyframe) else {
            panic!("wrong error kind");
        };
        assert_eq!(s, "RequestKeyframe");
    }
}
