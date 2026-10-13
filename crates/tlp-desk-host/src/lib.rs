//! Host session logic: accept incoming viewers, authorize them, then stream the screen and
//! inject their input.
//!
//! # Wiring (GUI or CLI)
//!
//! ```text
//! let endpoint = Arc::new(Endpoint::bind(identity, EndpointConfig::default())?); // one per process
//! let (host, mut events) = Host::start(endpoint.clone(), HostConfig::default()).await?;
//! // viewer side shares the same endpoint: Viewer::new(endpoint, ...)
//! while let Some(ev) = events.recv().await { /* show prompt / banner */ }
//! ```
//!
//! * Accept prompts surface as [`HostEvent::IncomingRequest`]; answer with [`Host::accept`] /
//!   [`Host::reject`] (unanswered requests are rejected after [`HostConfig::accept_timeout`]).
//!   Prompts are rate limited per peer IP.
//! * One-time-code and password requests are verified automatically.
//! * [`Host::sessions`] is the always-visible "who is connected" list; [`Host::disconnect`]
//!   ends a session; [`Host::set_permissions`] changes permissions live.
//! * Per session, capture + encode run on one std thread and input injection on another;
//!   nothing CPU-heavy runs on the tokio runtime. Factories in [`HostConfig`] let tests swap
//!   in fake capturers, encoders and injectors.
//!
//! * **Local priority**: while a viewer controls the host, the host user's own physical mouse
//!   and keyboard always win. Remote input is dropped for [`HostConfig::local_input_grace`]
//!   after any local input (held remote keys/buttons are released on takeover) and the viewer
//!   is told with `Message::HostInputPriority`. While the viewer moves the pointer, a green
//!   marker follows it on the host screen ([`HostConfig::remote_pointer_marker`]).
//! * **Monitors**: the viewer gets `Message::MonitorList` (after accept, on hot-plug and on
//!   switches) and may switch the streamed/controlled monitor with `Message::SelectMonitor`.
//!
//! All methods except [`Host::start`] are non-blocking and may be called from any thread.
#![forbid(unsafe_code)]

mod cursor;
mod input;
mod pipeline;
mod priority;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use tlp_desk_capture::{CaptureError, MonitorInfo, ScreenCapturer};
use tlp_desk_codec::{CodecError, EncoderConfig, VideoEncoder};
use tlp_desk_crypto::{
    DeviceId, Fingerprint, OneTimeCode, PasswordVerifier, RateLimitConfig, RateLimiter,
};
use tlp_desk_input::{
    InputError, InputEvent, InputInjector, LocalActivity, MonitorRect, PointerMarker,
};
use tlp_desk_net::{
    default_device_name, Advertisement, Discovery, DiscoveryConfig, Endpoint, NetError,
    PendingSession, Session,
};
use tlp_desk_proto::{ByeReason, Message, MonitorDesc, Permissions, RejectReason};
use tokio::sync::{mpsc, oneshot, watch, Notify};
use tokio::task::JoinHandle;

pub use cursor::{composite_cursor, scale_shape, CursorOverlay, CursorPatch};
pub use tlp_desk_net::{AuthKind, PeerInfo};

/// Creates the screen capturer for a monitor index (default: [`open_default_capturer`]).
pub type CapturerFactory =
    Arc<dyn Fn(u32) -> Result<Box<dyn ScreenCapturer>, CaptureError> + Send + Sync>;
/// Creates the video encoder (default: [`tlp_desk_codec::new_encoder`]).
pub type EncoderFactory =
    Arc<dyn Fn(EncoderConfig) -> Result<Box<dyn VideoEncoder>, CodecError> + Send + Sync>;
/// Creates the input injector for the captured monitor (default:
/// [`tlp_desk_input::injector_for_monitor`]).
pub type InjectorFactory =
    Arc<dyn Fn(MonitorRect) -> Result<Box<dyn InputInjector>, InputError> + Send + Sync>;
/// Starts the watcher of the host user's physical input (default:
/// [`tlp_desk_input::start_local_activity_monitor`]).
pub type LocalActivityFactory =
    Arc<dyn Fn() -> Result<Arc<dyn LocalActivity>, InputError> + Send + Sync>;
/// Creates the green remote-pointer marker, labelled with the viewer's name (default:
/// [`tlp_desk_input::start_pointer_marker`]).
pub type PointerMarkerFactory =
    Arc<dyn Fn(&str) -> Result<Box<dyn PointerMarker>, InputError> + Send + Sync>;
/// Lists the host's monitors (default: [`tlp_desk_capture::list_monitors`]).
pub type MonitorLister = Arc<dyn Fn() -> Result<Vec<MonitorInfo>, CaptureError> + Send + Sync>;

/// Largest video frame the default capturer delivers. Bigger monitors are downscaled on the GPU
/// before readback (3840x2400 -> 1920x1200), which keeps capture + encode well above 60 fps.
pub const MAX_CAPTURE_SIZE: (u32, u32) = (1920, 1200);

/// The default [`CapturerFactory`]: Windows.Graphics.Capture (DXGI fallback) with frames capped
/// at [`MAX_CAPTURE_SIZE`]. [`ScreenCapturer::monitor`] still reports the real monitor size.
pub fn open_default_capturer(monitor: u32) -> Result<Box<dyn ScreenCapturer>, CaptureError> {
    tlp_desk_capture::open_capturer_opts(
        monitor,
        tlp_desk_capture::CaptureOptions::max_size(MAX_CAPTURE_SIZE.0, MAX_CAPTURE_SIZE.1),
    )
}

/// How often the monitor list is re-read for hot-plug.
const MONITOR_POLL: Duration = Duration::from_secs(2);

/// Permissions granted by default: see the screen, use mouse and keyboard.
pub const DEFAULT_PERMISSIONS: Permissions =
    Permissions(Permissions::VIEW.0 | Permissions::CONTROL.0 | Permissions::KEYBOARD.0);

/// Host settings.
#[derive(Clone)]
pub struct HostConfig {
    /// Monitor to capture (0 = primary).
    pub monitor: u32,
    /// Target frame rate (default 60).
    pub fps: u32,
    /// Permissions for sessions authorized by code/password, and for [`Host::accept`] with
    /// `None`.
    pub default_permissions: Permissions,
    /// Unanswered accept prompts are rejected after this long.
    pub accept_timeout: Duration,
    /// Accept-prompt requests per peer IP (a viewer could otherwise flood the host user with
    /// prompts). Default: 5 per minute, then a 10-minute lockout. Accepting clears it.
    pub prompt_rate_limit: RateLimitConfig,
    /// At most this many accept prompts may be open at once; more are rejected.
    pub max_pending_prompts: usize,
    /// Whether viewers may authenticate with the one-time code.
    pub allow_one_time_code: bool,
    /// The one-time code (clones share state, so the UI can display the same code).
    pub one_time_code: OneTimeCode,
    /// Unattended-access password verifier; `None` = password access disabled.
    pub password: Option<PasswordVerifier>,
    /// Name announced in discovery.
    pub device_name: String,
    /// Announce this host on the LAN (mDNS + broadcast). The advertisement is filled in from
    /// the endpoint; other fields of `discovery` are used as given.
    pub advertise: bool,
    pub discovery: DiscoveryConfig,
    /// Local input priority: remote mouse/keyboard input is dropped while the host user's own
    /// physical input happened within this window. Default 1000 ms; zero disables priority.
    pub local_input_grace: Duration,
    /// Show a green marker at the pointer on the host screen while the viewer moves it.
    pub remote_pointer_marker: bool,
    pub capturer_factory: CapturerFactory,
    pub encoder_factory: EncoderFactory,
    pub injector_factory: InjectorFactory,
    pub local_activity_factory: LocalActivityFactory,
    pub pointer_marker_factory: PointerMarkerFactory,
    pub monitor_lister: MonitorLister,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            monitor: 0,
            fps: 60,
            default_permissions: DEFAULT_PERMISSIONS,
            accept_timeout: Duration::from_secs(60),
            prompt_rate_limit: RateLimitConfig::default(),
            max_pending_prompts: 8,
            allow_one_time_code: true,
            one_time_code: OneTimeCode::new(),
            password: None,
            device_name: default_device_name(),
            advertise: true,
            discovery: DiscoveryConfig::default(),
            local_input_grace: Duration::from_millis(1000),
            remote_pointer_marker: true,
            capturer_factory: Arc::new(open_default_capturer),
            encoder_factory: Arc::new(tlp_desk_codec::new_encoder),
            injector_factory: Arc::new(tlp_desk_input::injector_for_monitor),
            local_activity_factory: Arc::new(tlp_desk_input::start_local_activity_monitor),
            pointer_marker_factory: Arc::new(tlp_desk_input::start_pointer_marker),
            monitor_lister: Arc::new(tlp_desk_capture::list_monitors),
        }
    }
}

impl std::fmt::Debug for HostConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostConfig")
            .field("monitor", &self.monitor)
            .field("fps", &self.fps)
            .field("default_permissions", &self.default_permissions)
            .field("accept_timeout", &self.accept_timeout)
            .field("allow_one_time_code", &self.allow_one_time_code)
            .field("password", &self.password.is_some())
            .field("device_name", &self.device_name)
            .field("advertise", &self.advertise)
            .field("local_input_grace", &self.local_input_grace)
            .field("remote_pointer_marker", &self.remote_pointer_marker)
            .finish_non_exhaustive()
    }
}

/// Identifies an accept prompt awaiting the local user's decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(pub u64);

/// Identifies a connected viewer session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub u64);

/// A viewer asking the host user to click Accept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingRequest {
    pub id: RequestId,
    /// Device ID and fingerprint are authenticated (TLS); the name is self-reported.
    pub peer: PeerInfo,
    pub auth: AuthKind,
    /// The request is rejected automatically at this time.
    pub deadline: Instant,
}

/// One entry of the "who is connected" list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedViewer {
    pub id: SessionId,
    pub peer: PeerInfo,
    /// How the viewer was authorized.
    pub auth: AuthKind,
    pub permissions: Permissions,
    pub since: SystemTime,
}

/// Why an accept prompt went away without an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    /// The viewer gave up or disconnected.
    ViewerLeft,
    /// Nobody answered within [`HostConfig::accept_timeout`] (the viewer was rejected).
    TimedOut,
    /// The host is shutting down.
    HostShutdown,
}

/// Why a host session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndReason {
    /// [`Host::disconnect`] (or shutdown).
    Local,
    /// The viewer said goodbye.
    Remote(ByeReason),
    /// `VIEW` was revoked with [`Host::set_permissions`], which ends the session.
    PermissionRevoked,
    /// The connection dropped without a goodbye.
    ConnectionLost(String),
    /// Capture/encode failed for good.
    Error(String),
}

/// Events for the UI.
#[derive(Debug, Clone)]
pub enum HostEvent {
    /// Show an Accept / Reject prompt.
    IncomingRequest(IncomingRequest),
    /// Dismiss the prompt.
    RequestCancelled { id: RequestId, reason: CancelReason },
    /// A code/password attempt failed or a peer was refused (for the log / a toast).
    AuthFailed {
        peer: PeerInfo,
        auth: AuthKind,
        reason: RejectReason,
    },
    /// The one-time code rotated; re-read [`Host::one_time_code`].
    CodeChanged,
    /// A viewer is now connected (show the banner).
    SessionStarted(ConnectedViewer),
    PermissionsChanged {
        id: SessionId,
        permissions: Permissions,
    },
    SessionEnded {
        id: SessionId,
        peer: PeerInfo,
        reason: EndReason,
    },
}

/// Receiving end of the host event stream.
pub type HostEvents = mpsc::UnboundedReceiver<HostEvent>;

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error(transparent)]
    Net(#[from] NetError),
}

enum Decision {
    Accept(Permissions),
    Reject,
}

struct PendingEntry {
    info: IncomingRequest,
    decide: oneshot::Sender<Decision>,
}

struct SessionCtl {
    info: Mutex<ConnectedViewer>,
    perms: AtomicU16,
    stop: Notify,
    perms_changed: Notify,
}

struct Inner {
    endpoint: Arc<Endpoint>,
    cfg: HostConfig,
    password: Mutex<Option<PasswordVerifier>>,
    events: mpsc::UnboundedSender<HostEvent>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<RequestId, PendingEntry>>,
    sessions: Mutex<HashMap<SessionId, Arc<SessionCtl>>>,
    prompt_limiter: Mutex<RateLimiter>,
    shutting_down: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Inner {
    fn emit(&self, ev: HostEvent) {
        let _ = self.events.send(ev);
    }

    fn next(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// A running host. Dropping it shuts it down (see [`Host::shutdown`]).
pub struct Host {
    inner: Arc<Inner>,
    accept_task: Mutex<Option<JoinHandle<()>>>,
    discovery: Mutex<Option<Arc<Discovery>>>,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("device_id", &self.device_id())
            .finish_non_exhaustive()
    }
}

impl Host {
    /// Starts accepting sessions on `endpoint` (which must have been bound with
    /// `listen: true`) and, if configured, advertising on the LAN. Must run inside a tokio
    /// runtime. A discovery failure is logged, not fatal (manual IP connect still works).
    ///
    /// Only one `Host` per endpoint: it consumes [`Endpoint::accept`].
    pub async fn start(
        endpoint: Arc<Endpoint>,
        cfg: HostConfig,
    ) -> Result<(Host, HostEvents), HostError> {
        let port = endpoint.local_addr()?.port();
        let (tx, rx) = mpsc::unbounded_channel();
        let discovery = if cfg.advertise {
            let mut dcfg = cfg.discovery.clone();
            dcfg.advertise = Some(Advertisement {
                device_id: endpoint.device_id(),
                name: cfg.device_name.clone(),
                port,
            });
            match Discovery::start(dcfg).await {
                Ok(d) => Some(Arc::new(d)),
                Err(e) => {
                    tracing::warn!(%e, "LAN discovery unavailable; manual IP connect still works");
                    None
                }
            }
        } else {
            None
        };
        let inner = Arc::new(Inner {
            endpoint,
            password: Mutex::new(cfg.password.clone()),
            prompt_limiter: Mutex::new(RateLimiter::new(cfg.prompt_rate_limit)),
            cfg,
            events: tx,
            next_id: AtomicU64::new(0),
            pending: Mutex::default(),
            sessions: Mutex::default(),
            shutting_down: AtomicBool::new(false),
        });
        let accept_task = tokio::spawn(accept_loop(inner.clone()));
        tracing::info!(
            device_id = %inner.endpoint.device_id(),
            port,
            advertise = discovery.is_some(),
            "host started"
        );
        Ok((
            Host {
                inner,
                accept_task: Mutex::new(Some(accept_task)),
                discovery: Mutex::new(discovery),
            },
            rx,
        ))
    }

    pub fn device_id(&self) -> DeviceId {
        self.inner.endpoint.device_id()
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.inner.endpoint.fingerprint()
    }

    /// The shared endpoint (also used by the viewer side).
    pub fn endpoint(&self) -> &Arc<Endpoint> {
        &self.inner.endpoint
    }

    /// The one-time code to display (`display()` gives `"042 917"`). Clones share state.
    pub fn one_time_code(&self) -> OneTimeCode {
        self.inner.cfg.one_time_code.clone()
    }

    /// Rotates the one-time code now and emits [`HostEvent::CodeChanged`].
    pub fn rotate_code(&self) {
        self.inner.cfg.one_time_code.rotate();
        self.inner.emit(HostEvent::CodeChanged);
    }

    /// Enables (Some) or disables (None) unattended password access.
    pub fn set_password(&self, verifier: Option<PasswordVerifier>) {
        *lock(&self.inner.password) = verifier;
    }

    /// The running discovery (advertising this host and browsing the LAN), so the viewer side
    /// can list devices without starting a second instance.
    pub fn discovery(&self) -> Option<Arc<Discovery>> {
        lock(&self.discovery).clone()
    }

    /// Accept prompts currently awaiting an answer.
    pub fn pending_requests(&self) -> Vec<IncomingRequest> {
        let mut v: Vec<_> = lock(&self.inner.pending)
            .values()
            .map(|p| p.info.clone())
            .collect();
        v.sort_by_key(|r| r.id);
        v
    }

    /// The user clicked **Accept**. `None` grants [`HostConfig::default_permissions`].
    /// Returns `false` if the request no longer exists.
    pub fn accept(&self, id: RequestId, permissions: Option<Permissions>) -> bool {
        let perms = permissions.unwrap_or(self.inner.cfg.default_permissions);
        match lock(&self.inner.pending).remove(&id) {
            Some(p) => p.decide.send(Decision::Accept(perms)).is_ok(),
            None => false,
        }
    }

    /// The user clicked **Reject**. Returns `false` if the request no longer exists.
    pub fn reject(&self, id: RequestId) -> bool {
        match lock(&self.inner.pending).remove(&id) {
            Some(p) => p.decide.send(Decision::Reject).is_ok(),
            None => false,
        }
    }

    /// Who is connected right now (the security banner must always show this).
    pub fn sessions(&self) -> Vec<ConnectedViewer> {
        let mut v: Vec<_> = lock(&self.inner.sessions)
            .values()
            .map(|s| lock(&s.info).clone())
            .collect();
        v.sort_by_key(|s| s.id);
        v
    }

    /// Changes a session's permissions live. Removing `VIEW` ends the session.
    pub fn set_permissions(&self, id: SessionId, permissions: Permissions) -> bool {
        let Some(ctl) = lock(&self.inner.sessions).get(&id).cloned() else {
            return false;
        };
        ctl.perms.store(permissions.0, Ordering::Release);
        lock(&ctl.info).permissions = permissions;
        ctl.perms_changed.notify_one();
        true
    }

    /// Ends a session (the viewer receives `Bye`). [`HostEvent::SessionEnded`] follows.
    pub fn disconnect(&self, id: SessionId) -> bool {
        match lock(&self.inner.sessions).get(&id) {
            Some(ctl) => {
                ctl.stop.notify_one();
                true
            }
            None => false,
        }
    }

    pub fn disconnect_all(&self) {
        for ctl in lock(&self.inner.sessions).values() {
            ctl.stop.notify_one();
        }
    }

    /// Stops accepting and advertising, rejects open prompts and ends all sessions.
    pub fn shutdown(&self) {
        self.inner.shutting_down.store(true, Ordering::Release);
        if let Some(t) = lock(&self.accept_task).take() {
            t.abort();
        }
        lock(&self.discovery).take();
        let pending: Vec<_> = lock(&self.inner.pending).drain().collect();
        for (id, _) in pending {
            // Dropping the sender rejects the viewer.
            self.inner.emit(HostEvent::RequestCancelled {
                id,
                reason: CancelReason::HostShutdown,
            });
        }
        self.disconnect_all();
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn accept_loop(inner: Arc<Inner>) {
    while let Some(pending) = inner.endpoint.accept().await {
        if inner.shutting_down.load(Ordering::Acquire) {
            pending.reject(RejectReason::NotAllowed).await;
            continue;
        }
        tokio::spawn(handle_pending(inner.clone(), pending));
    }
    tracing::debug!("host accept loop ended");
}

async fn handle_pending(inner: Arc<Inner>, p: PendingSession) {
    let peer = p.peer().clone();
    let kind = p.auth_kind();
    match kind {
        AuthKind::AcceptPrompt => handle_prompt(inner, p).await,
        AuthKind::OneTimeCode => {
            if !inner.cfg.allow_one_time_code {
                tracing::info!(peer = %peer.addr, device_id = %peer.device_id, "one-time code access disabled; rejecting");
                refuse(&inner, p, RejectReason::NotAllowed).await;
                return;
            }
            let code = inner.cfg.one_time_code.clone();
            let generation = code.generation();
            let res = p.verify_code(&code, inner.cfg.default_permissions).await;
            if code.generation() != generation {
                inner.emit(HostEvent::CodeChanged);
            }
            finish_auth(inner, peer, kind, res).await;
        }
        AuthKind::Password => {
            let verifier = lock(&inner.password).clone();
            let Some(verifier) = verifier else {
                tracing::info!(peer = %peer.addr, device_id = %peer.device_id, "password access disabled; rejecting");
                refuse(&inner, p, RejectReason::NotAllowed).await;
                return;
            };
            let res = p
                .verify_password(&verifier, inner.cfg.default_permissions)
                .await;
            finish_auth(inner, peer, kind, res).await;
        }
    }
}

async fn refuse(inner: &Inner, p: PendingSession, reason: RejectReason) {
    let peer = p.peer().clone();
    let auth = p.auth_kind();
    p.reject(reason).await;
    inner.emit(HostEvent::AuthFailed { peer, auth, reason });
}

async fn finish_auth(
    inner: Arc<Inner>,
    peer: PeerInfo,
    auth: AuthKind,
    res: Result<Session, NetError>,
) {
    match res {
        Ok(session) => run_session(inner, session, auth).await,
        Err(e) => {
            let reason = match e {
                NetError::AuthFailed => RejectReason::WrongCredentials,
                NetError::RateLimited => RejectReason::RateLimited,
                ref other => {
                    tracing::warn!(peer = %peer.addr, %other, "authentication aborted");
                    return;
                }
            };
            inner.emit(HostEvent::AuthFailed { peer, auth, reason });
        }
    }
}

enum PromptOutcome {
    Decided(Decision),
    ViewerLeft,
    TimedOut,
    Dropped,
}

async fn handle_prompt(inner: Arc<Inner>, p: PendingSession) {
    let peer = p.peer().clone();
    let ip = peer.addr.ip();
    let limited = lock(&inner.prompt_limiter).attempt(&ip, Instant::now());
    if let Err(e) = limited {
        tracing::warn!(peer = %peer.addr, device_id = %peer.device_id, retry_after = ?e.retry_after, "accept prompt refused: rate limited");
        refuse(&inner, p, RejectReason::RateLimited).await;
        return;
    }
    let id = RequestId(inner.next());
    let deadline = Instant::now() + inner.cfg.accept_timeout;
    let info = IncomingRequest {
        id,
        peer: peer.clone(),
        auth: AuthKind::AcceptPrompt,
        deadline,
    };
    let (tx, rx) = oneshot::channel();
    let admitted = {
        let mut pending = lock(&inner.pending);
        if pending.len() >= inner.cfg.max_pending_prompts {
            false
        } else {
            pending.insert(
                id,
                PendingEntry {
                    info: info.clone(),
                    decide: tx,
                },
            );
            true
        }
    };
    if !admitted {
        tracing::warn!(peer = %peer.addr, "accept prompt refused: too many open prompts");
        refuse(&inner, p, RejectReason::RateLimited).await;
        return;
    }
    inner.emit(HostEvent::IncomingRequest(info));

    let outcome = {
        let timeout = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline));
        tokio::select! {
            d = rx => match d {
                Ok(d) => PromptOutcome::Decided(d),
                Err(_) => PromptOutcome::Dropped,
            },
            _ = p.cancelled() => PromptOutcome::ViewerLeft,
            _ = timeout => PromptOutcome::TimedOut,
        }
    };
    let was_pending = lock(&inner.pending).remove(&id).is_some();
    match outcome {
        PromptOutcome::Decided(Decision::Accept(perms)) => {
            lock(&inner.prompt_limiter).record_success(&ip);
            match p.accept(perms).await {
                Ok(session) => run_session(inner, session, AuthKind::AcceptPrompt).await,
                Err(e) => tracing::warn!(peer = %peer.addr, %e, "accepting the session failed"),
            }
        }
        PromptOutcome::Decided(Decision::Reject) => {
            p.reject(RejectReason::UserDeclined).await;
        }
        PromptOutcome::Dropped => {
            // Host shut down (the cancellation event was already sent).
            p.reject(RejectReason::UserDeclined).await;
        }
        PromptOutcome::ViewerLeft => {
            tracing::info!(peer = %peer.addr, "viewer left before the prompt was answered");
            if was_pending {
                inner.emit(HostEvent::RequestCancelled {
                    id,
                    reason: CancelReason::ViewerLeft,
                });
            }
        }
        PromptOutcome::TimedOut => {
            tracing::info!(peer = %peer.addr, "accept prompt timed out; rejecting");
            if was_pending {
                inner.emit(HostEvent::RequestCancelled {
                    id,
                    reason: CancelReason::TimedOut,
                });
            }
            p.reject(RejectReason::UserDeclined).await;
        }
    }
}

/// Mouse needs CONTROL, keys need KEYBOARD; `ReleaseAll` needs either.
fn input_allowed(perms: Permissions, ev: &InputEvent) -> bool {
    match ev {
        InputEvent::MouseMove { .. }
        | InputEvent::MouseButton { .. }
        | InputEvent::MouseWheel { .. } => perms.contains(Permissions::CONTROL),
        InputEvent::Key { .. } => perms.contains(Permissions::KEYBOARD),
        InputEvent::ReleaseAll => {
            perms.contains(Permissions::CONTROL) || perms.contains(Permissions::KEYBOARD)
        }
    }
}

async fn run_session(inner: Arc<Inner>, session: Session, auth: AuthKind) {
    let session = Arc::new(session);
    let peer = session.peer().clone();
    let perms = session.initial_permissions();
    let id = SessionId(inner.next());
    let info = ConnectedViewer {
        id,
        peer: peer.clone(),
        auth,
        permissions: perms,
        since: SystemTime::now(),
    };
    let ctl = Arc::new(SessionCtl {
        info: Mutex::new(info.clone()),
        perms: AtomicU16::new(perms.0),
        stop: Notify::new(),
        perms_changed: Notify::new(),
    });
    if inner.shutting_down.load(Ordering::Acquire) {
        session.close(ByeReason::UserClosed).await;
        return;
    }
    lock(&inner.sessions).insert(id, ctl.clone());
    tracing::info!(session = id.0, peer = %peer.addr, device_id = %peer.device_id, name = %peer.name, ?auth, ?perms, "viewer connected");
    inner.emit(HostEvent::SessionStarted(info));

    let (rect_tx, rect_rx) = watch::channel(None);
    let (input_tx, input_rx) = std::sync::mpsc::channel::<InputEvent>();
    let (prio_tx, mut prio_rx) = mpsc::unbounded_channel::<bool>();
    let input_ctx = input::InputCtx {
        factory: inner.cfg.injector_factory.clone(),
        rect: rect_rx,
        events: input_rx,
        grace: inner.cfg.local_input_grace,
        activity_factory: Some(inner.cfg.local_activity_factory.clone()),
        marker_factory: inner
            .cfg
            .remote_pointer_marker
            .then(|| inner.cfg.pointer_marker_factory.clone()),
        label: if peer.name.trim().is_empty() {
            peer.device_id.to_string()
        } else {
            peer.name.clone()
        },
        priority: prio_tx,
    };
    let input_thread = std::thread::Builder::new()
        .name(format!("tlp-host-input-{}", id.0))
        .spawn(move || input::run(input_ctx));
    if let Err(e) = &input_thread {
        tracing::error!(%e, "cannot spawn input thread");
    }

    let stop = Arc::new(AtomicBool::new(false));
    let keyframe = Arc::new(AtomicBool::new(false));
    let (fatal_tx, mut fatal_rx) = mpsc::unbounded_channel();
    let (select_tx, select_rx) = watch::channel(inner.cfg.monitor);
    let (active_tx, mut active_rx) = watch::channel(inner.cfg.monitor);
    let ctx = pipeline::PipelineCtx {
        capturer_factory: inner.cfg.capturer_factory.clone(),
        encoder_factory: inner.cfg.encoder_factory.clone(),
        monitor: inner.cfg.monitor,
        select: select_rx,
        active: active_tx,
        fps: inner.cfg.fps,
        session: session.clone(),
        keyframe: keyframe.clone(),
        stop: stop.clone(),
        rect: rect_tx,
        fatal: fatal_tx.clone(),
    };
    if let Err(e) = std::thread::Builder::new()
        .name(format!("tlp-host-video-{}", id.0))
        .spawn(move || pipeline::run(ctx))
    {
        let _ = fatal_tx.send(format!("cannot spawn video thread: {e}"));
    }

    let mut current = perms;
    let lister = inner.cfg.monitor_lister.clone();
    let mut monitors = list_monitors(&lister).await;
    let mut sent_list: Option<(Vec<MonitorDesc>, u32)> = None;
    let mut monitor_poll = tokio::time::interval(MONITOR_POLL);
    monitor_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    monitor_poll.reset();
    let mut active_open = true;
    let mut priority_open = true;
    let reason = loop {
        // Publish the monitor list whenever it (or the active monitor) differs from what the
        // viewer last saw.
        if let Some(list) = &monitors {
            let now = (list.clone(), *active_rx.borrow_and_update());
            if sent_list.as_ref() != Some(&now) {
                let msg = Message::MonitorList {
                    monitors: now.0.clone(),
                    active: now.1,
                };
                if let Err(e) = session.send(&msg).await {
                    break EndReason::ConnectionLost(e.to_string());
                }
                sent_list = Some(now);
            }
        }
        tokio::select! {
            _ = monitor_poll.tick() => {
                if let Some(list) = list_monitors(&lister).await {
                    monitors = Some(list);
                }
            }
            changed = active_rx.changed(), if active_open => {
                if changed.is_err() {
                    // The pipeline is gone; it reports fatal errors separately.
                    active_open = false;
                }
                // The list is re-sent at the top of the loop if the active index differs.
            }
            p = prio_rx.recv(), if priority_open => match p {
                Some(local_active) => {
                    tracing::info!(session = id.0, local_active, "host input priority changed");
                    if let Err(e) = session.send(&Message::HostInputPriority { local_active }).await {
                        break EndReason::ConnectionLost(e.to_string());
                    }
                }
                None => priority_open = false,
            },
            _ = ctl.stop.notified() => {
                session.close(ByeReason::UserClosed).await;
                break EndReason::Local;
            }
            Some(err) = fatal_rx.recv() => {
                session.close(ByeReason::Error).await;
                break EndReason::Error(err);
            }
            _ = ctl.perms_changed.notified() => {
                let new = Permissions(ctl.perms.load(Ordering::Acquire));
                if new == current {
                    continue;
                }
                tracing::info!(session = id.0, old = ?current, new = ?new, "permissions changed");
                let lost_input = (current.contains(Permissions::CONTROL) && !new.contains(Permissions::CONTROL))
                    || (current.contains(Permissions::KEYBOARD) && !new.contains(Permissions::KEYBOARD));
                if lost_input {
                    let _ = input_tx.send(InputEvent::ReleaseAll);
                }
                current = new;
                inner.emit(HostEvent::PermissionsChanged { id, permissions: new });
                if !new.contains(Permissions::VIEW) {
                    session.close(ByeReason::PermissionRevoked).await;
                    break EndReason::PermissionRevoked;
                }
                if let Err(e) = session.send(&Message::Permissions(new)).await {
                    break EndReason::ConnectionLost(e.to_string());
                }
            }
            msg = session.recv() => match msg {
                Ok(Message::InputEvent(ev)) => {
                    if input_allowed(current, &ev) {
                        let _ = input_tx.send(ev);
                    } else {
                        tracing::trace!(?ev, "input blocked by permissions");
                    }
                }
                Ok(Message::RequestKeyframe) => keyframe.store(true, Ordering::Release),
                Ok(Message::SelectMonitor { index }) => {
                    let known = monitors
                        .as_ref()
                        .is_some_and(|l| l.iter().any(|m| m.index == index));
                    if known {
                        tracing::info!(session = id.0, index, "viewer selected monitor");
                        select_tx.send_replace(index);
                    } else {
                        tracing::warn!(session = id.0, index, "viewer selected an unknown monitor; ignoring");
                    }
                }
                Ok(Message::Bye(b)) => break EndReason::Remote(b.reason),
                Ok(other) => tracing::debug!(msg = ?std::mem::discriminant(&other), "ignoring control message"),
                Err(NetError::Closed) => break EndReason::ConnectionLost("control stream closed".into()),
                Err(e) => break EndReason::ConnectionLost(e.to_string()),
            }
        }
    };

    stop.store(true, Ordering::Release);
    // Dropping the sender makes the input thread release every key/button and exit.
    drop(input_tx);
    lock(&inner.sessions).remove(&id);
    tracing::info!(session = id.0, peer = %peer.addr, device_id = %peer.device_id, ?reason, "viewer disconnected");
    inner.emit(HostEvent::SessionEnded { id, peer, reason });
}

/// Reads the monitor list off the runtime; `None` (logged) if it fails.
async fn list_monitors(lister: &MonitorLister) -> Option<Vec<MonitorDesc>> {
    let lister = lister.clone();
    match tokio::task::spawn_blocking(move || lister()).await {
        Ok(Ok(list)) => Some(list.into_iter().map(monitor_desc).collect()),
        Ok(Err(e)) => {
            tracing::debug!(%e, "cannot list monitors");
            None
        }
        Err(e) => {
            tracing::debug!(%e, "monitor listing task failed");
            None
        }
    }
}

fn monitor_desc(m: MonitorInfo) -> MonitorDesc {
    MonitorDesc {
        index: m.index,
        name: m.name,
        width: m.width,
        height: m.height,
        primary: m.primary,
        x: m.x,
        y: m.y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_gating_follows_permissions() {
        let mv = InputEvent::MouseMove { x: 1, y: 2 };
        let key = InputEvent::Key {
            scancode: 0x1e,
            extended: false,
            down: true,
        };
        let view = Permissions::VIEW;
        assert!(!input_allowed(view, &mv));
        assert!(!input_allowed(view, &key));
        assert!(!input_allowed(view, &InputEvent::ReleaseAll));
        let mouse = view | Permissions::CONTROL;
        assert!(input_allowed(mouse, &mv));
        assert!(!input_allowed(mouse, &key));
        assert!(input_allowed(mouse, &InputEvent::ReleaseAll));
        assert!(input_allowed(DEFAULT_PERMISSIONS, &key));
    }
}
