//! The real backend: one tokio runtime, **one** QUIC [`Endpoint`] (UDP 47400) shared by the
//! [`Host`] (accepting viewers, advertising on the LAN) and the [`Viewer`] (dialing hosts).
//!
//! * Identity comes from [`tlp_desk_crypto::default_dir`] (generated on first run).
//! * The device list comes from the host's [`Discovery`] instance (or a browse-only one when
//!   this device cannot host), refreshed on every discovery event.
//! * Viewer frames: the viewer decodes straight into RGBA ([`PixelFormat::Rgba`]); its
//!   `on_frame` callback runs on the decode thread and *moves* the buffer into the app's
//!   [`FrameSlot`] (no copy). Buffers the UI consumed or that were overwritten go back to the
//!   decoder through the slot's recycler.
//! * If UDP 47400 is taken (another instance), the backend falls back to a dial-only endpoint
//!   on an ephemeral port and reports [`HostProblem::PortInUse`] instead of failing.
//!
//! Nothing here contacts anything outside the local network.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tlp_desk_crypto::{Fingerprint, HostKey, Identity};
use tlp_desk_files::clipfiles::ClipFilesConfig;
use tlp_desk_files::fs::FsConfig;
use tlp_desk_files::{FileTransfer, FilesError, TransferConfig, TransferSnapshot, TransferState};
use tlp_desk_host::{
    EndReason as HostEnd, Host, HostConfig, HostEvent, HostEvents, RequestId as HostRequestId,
    SessionId as HostSessionId, WhiteboardHost, DEFAULT_PERMISSIONS,
};
use tlp_desk_net::{
    default_device_name, AwaitingAcceptFn, Discovery, DiscoveryConfig, Endpoint, EndpointConfig,
    FeatureSet, NetError, DEFAULT_PORT,
};
use tlp_desk_proto::{InputEvent, Permissions, RejectReason};
use tlp_desk_viewer::{
    AuthMethod, ConnectTarget as ViewerTarget, EndReason as ViewerEnd, KnownHostsStore,
    PixelFormat, Viewer, ViewerConfig, ViewerError, ViewerEvent, ViewerEvents, ViewerSession,
};
use tokio::runtime::Handle;
use tokio::task::AbortHandle;

use super::chat::{self, ChatBridge, ChatLine, ChatSendError};
use super::files::{
    FsOp, FsRequestId, FsSide, TransferDir, TransferId, TransferRequest, TransferStartError,
    TransferStatus, TransferView,
};
use super::files_bridge::{EngineStarter, FilesBridge};
use super::{
    whiteboard, AttemptId, Backend, BackendEvent, ConnectError, ConnectTarget, DeviceId,
    DiscoveredDevice, EndReason, EventReceiver, EventSender, FrameSlot, HostProblem, LocalInfo,
    PeerInfo, RepaintFn, RequestId, SessionId, SessionStats, VideoImage, WhiteboardOp,
};

/// How often host sessions are reconciled with [`Host::sessions`] and LAN addresses re-read.
const RECONCILE_EVERY: Duration = Duration::from_secs(2);
/// How often the device list is re-sent even without discovery events (expiry, first paint).
const DEVICES_EVERY: Duration = Duration::from_secs(5);

/// Startup settings of the real backend.
#[derive(Clone)]
pub struct RealConfig {
    /// Where the identity key (and by default the known-hosts file) lives.
    pub data_dir: PathBuf,
    /// Address and UDP port of the shared endpoint (default `[::]:47400`).
    pub bind: SocketAddr,
    /// Name announced on the LAN and sent to peers (default: the computer name).
    pub device_name: Option<String>,
    pub known_hosts: KnownHostsStore,
    /// Host settings template (`device_name` is overwritten). Tests swap in fake capture.
    pub host: HostConfig,
    /// Start a browse-only discovery when the host (and its discovery) is unavailable.
    pub fallback_discovery: bool,
    /// Clipboard to sync in sessions (the app passes the system clipboard). `None` = a private
    /// in-memory clipboard, so tests never touch the user's real clipboard.
    pub clipboard: Option<Arc<dyn tlp_desk_files::ClipboardBackend>>,
    /// File transfer settings (conflict policy, where uploads from viewers land).
    pub transfer: TransferConfig,
}

impl Default for RealConfig {
    fn default() -> Self {
        Self {
            data_dir: tlp_desk_crypto::default_dir(),
            bind: SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), DEFAULT_PORT),
            device_name: None,
            known_hosts: KnownHostsStore::Default,
            host: HostConfig {
                // Moving files and chatting are core uses: accepted sessions may do both.
                // The host user can revoke either live.
                default_permissions: DEFAULT_PERMISSIONS
                    .union(Permissions::FILES)
                    .union(Permissions::CHAT),
                ..HostConfig::default()
            },
            fallback_discovery: true,
            clipboard: None,
            transfer: TransferConfig::default(),
        }
    }
}

/// Real implementation of [`Backend`].
pub struct RealBackend {
    inner: Arc<Inner>,
}

struct Bridge {
    session: SessionId,
    src: Arc<tlp_desk_viewer::FrameSlot>,
    dst: Arc<FrameSlot>,
}

type Bridges = Arc<Mutex<Vec<Bridge>>>;

struct Inner {
    rt: Handle,
    tx: EventSender,
    repaint: RepaintFn,
    next_id: AtomicU64,
    endpoint: Option<Arc<Endpoint>>,
    host: Option<Host>,
    viewer: Option<Viewer>,
    discovery: Option<Arc<Discovery>>,
    own_id: DeviceId,
    bridges: Bridges,
    chat: ChatBridge,
    /// File transfer engine shared by host and viewer sessions (lane files-transfer).
    files: Arc<FileTransfer>,
    state: Mutex<State>,
    /// Running session recordings (TLP-recording).
    recorders: super::recording::Recorders,
    clipboard: Arc<tlp_desk_files::ClipboardFeature>,
    /// Files lane: remote file system + clipboard files features (TLP-files).
    fsb: FilesBridge,
}

#[derive(Default)]
struct State {
    local: Option<LocalInfo>,
    attempts: HashMap<AttemptId, AbortHandle>,
    viewer_sessions: HashMap<SessionId, Arc<ViewerSession>>,
    host_sessions: HashMap<SessionId, HostSessionId>,
    requests: HashMap<RequestId, HostRequestId>,
    /// Remembered keys that failed with `HostKeyChanged`, per attempt.
    changed_keys: HashMap<AttemptId, HostKey>,
    last_devices: Option<Vec<DiscoveredDevice>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The UI's view of an engine snapshot.
fn transfer_view(s: &TransferSnapshot) -> TransferView {
    let status = match &s.state {
        TransferState::Preparing => TransferStatus::Preparing,
        TransferState::Running => TransferStatus::Running,
        TransferState::Paused => TransferStatus::Paused,
        TransferState::Completed => TransferStatus::Completed,
        TransferState::Cancelled => TransferStatus::Cancelled,
        TransferState::Failed(e) => TransferStatus::Failed(e.clone()),
        TransferState::Disconnected => TransferStatus::Disconnected,
    };
    TransferView {
        id: TransferId(s.info.id.0),
        dir: match s.info.direction {
            tlp_desk_proto::TransferDirection::Upload => TransferDir::Send,
            _ => TransferDir::Receive,
        },
        label: s.info.label.clone(),
        status,
        bytes_done: s.bytes_done,
        total_bytes: s.info.total_bytes,
        files_done: s.files_done,
        total_files: s.info.total_files,
        failed_files: s.failed_items,
        bytes_per_sec: s.bytes_per_sec,
        local_dest: s.local_dest.clone(),
    }
}

fn app_id(id: tlp_desk_crypto::DeviceId) -> DeviceId {
    DeviceId(id.as_u32())
}

fn app_peer(p: &tlp_desk_net::PeerInfo) -> PeerInfo {
    PeerInfo {
        device_id: app_id(p.device_id),
        name: p.name.clone(),
        address: p.addr.ip(),
    }
}

/// First 16 hex digits of a fingerprint in groups of four (`3f9a 1c07 e2b4 58d1`).
pub fn short_fingerprint(fp: &Fingerprint) -> String {
    let hex = fp.to_hex();
    hex.as_bytes()
        .chunks(4)
        .take(4)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether `ip` is an address other LAN devices can reach us on (private / link-local IPv4,
/// unique-local IPv6). Loopback, IPv6 link-local (needs a scope) and public addresses are
/// left out.
pub fn is_lan_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
}

/// This machine's LAN addresses, IPv4 first. Reads local interfaces only (no network I/O).
pub fn lan_addresses() -> Vec<IpAddr> {
    let mut v: Vec<IpAddr> = if_addrs::get_if_addrs()
        .map(|ifs| {
            ifs.into_iter()
                .map(|i| i.ip())
                .filter(|ip| is_lan_address(*ip))
                .collect()
        })
        .unwrap_or_else(|e| {
            tracing::warn!(%e, "cannot enumerate network interfaces");
            Vec::new()
        });
    v.sort_by_key(|ip| (ip.is_ipv6(), *ip));
    v.dedup();
    v
}

fn map_viewer_error(e: &ViewerError) -> ConnectError {
    match e {
        ViewerError::NotFound(_) => ConnectError::NotFound,
        ViewerError::NotAllowed(_) => ConnectError::NotLocalNetwork,
        ViewerError::Unreachable(_) => ConnectError::Unreachable,
        ViewerError::Rejected(r) => ConnectError::Rejected(*r),
        ViewerError::IdentityMismatch { .. } | ViewerError::HostAuthFailed => {
            ConnectError::IdentityMismatch
        }
        ViewerError::HostKeyChanged {
            previous,
            presented,
            ..
        } => ConnectError::HostKeyChanged {
            previous: short_fingerprint(previous),
            presented: short_fingerprint(presented),
        },
        ViewerError::VersionMismatch { .. } => {
            ConnectError::Rejected(RejectReason::VersionMismatch)
        }
        ViewerError::Net(NetError::Timeout) => ConnectError::Timeout,
        other => ConnectError::Other(other.to_string()),
    }
}

fn map_viewer_end(r: ViewerEnd) -> EndReason {
    match r {
        ViewerEnd::Local => EndReason::Local,
        ViewerEnd::Remote(_) => EndReason::Remote,
        ViewerEnd::ConnectionLost(e) | ViewerEnd::Error(e) => EndReason::Error(e),
    }
}

fn map_host_end(r: HostEnd) -> EndReason {
    match r {
        HostEnd::Local | HostEnd::PermissionRevoked => EndReason::Local,
        HostEnd::Remote(_) => EndReason::Remote,
        HostEnd::ConnectionLost(e) | HostEnd::Error(e) => EndReason::Error(e),
    }
}

fn is_addr_in_use(e: &NetError) -> bool {
    matches!(e, NetError::Io(io) if io.kind() == std::io::ErrorKind::AddrInUse)
}

impl RealBackend {
    /// Start with the default configuration (UDP 47400, identity in the data dir).
    ///
    /// Must be called from outside the runtime (it blocks briefly on `rt` while binding the
    /// endpoint and starting discovery). Never fails: problems are reported as
    /// [`BackendEvent::HostUnavailable`] and the UI keeps working as far as possible.
    pub fn start(rt: &Handle, repaint: RepaintFn) -> (Arc<dyn Backend>, EventReceiver) {
        Self::start_with(rt, repaint, RealConfig::default())
    }

    /// [`RealBackend::start`] with explicit settings (tests, `--port`).
    pub fn start_with(
        rt: &Handle,
        repaint: RepaintFn,
        cfg: RealConfig,
    ) -> (Arc<dyn Backend>, EventReceiver) {
        let (b, rx) = Self::start_concrete(rt, repaint, cfg);
        (b, rx)
    }

    /// The local UDP address of the shared endpoint (`None` if networking failed).
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.inner.endpoint.as_ref()?.local_addr().ok()
    }

    /// Records sessions into `folder` instead of `%USERPROFILE%\Videos\TLP-Desk` (tests).
    pub fn set_recordings_folder(&self, folder: PathBuf) {
        self.inner.recorders.set_folder(folder);
    }

    /// Like [`RealBackend::start_with`] but returns the concrete type (tests).
    pub fn start_concrete(
        rt: &Handle,
        repaint: RepaintFn,
        cfg: RealConfig,
    ) -> (Arc<RealBackend>, EventReceiver) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut problem = None;

        let identity = match Identity::load_or_generate(&cfg.data_dir) {
            Ok(i) => Some(Arc::new(i)),
            Err(e) => {
                tracing::error!(%e, dir = %cfg.data_dir.display(), "cannot load or create the identity key");
                problem = Some(HostProblem::NoNetwork(format!(
                    "{}: {e}",
                    cfg.data_dir.display()
                )));
                None
            }
        };
        let device_name = cfg
            .device_name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(default_device_name);

        // The endpoint spawns its accept loop, so bind inside the runtime.
        let bind = |c: EndpointConfig, id: &Arc<Identity>| {
            let id = id.clone();
            rt.block_on(async move { Endpoint::bind(id, c) })
        };
        let mut endpoint = None;
        if let Some(identity) = &identity {
            let listen_cfg = EndpointConfig {
                bind: cfg.bind,
                device_name: device_name.clone(),
                listen: true,
                ..EndpointConfig::default()
            };
            match bind(listen_cfg, identity) {
                Ok(ep) => endpoint = Some((Arc::new(ep), true)),
                Err(e) => {
                    tracing::warn!(%e, bind = %cfg.bind, "cannot listen; falling back to dial-only");
                    problem = Some(if is_addr_in_use(&e) {
                        HostProblem::PortInUse(cfg.bind.port())
                    } else {
                        HostProblem::Other(e.to_string())
                    });
                    let dial_cfg = EndpointConfig {
                        bind: SocketAddr::new(cfg.bind.ip(), 0),
                        device_name: device_name.clone(),
                        listen: false,
                        ..EndpointConfig::default()
                    };
                    match bind(dial_cfg, identity) {
                        Ok(ep) => endpoint = Some((Arc::new(ep), false)),
                        Err(e) => {
                            tracing::error!(%e, "cannot open any UDP socket");
                            problem = Some(HostProblem::NoNetwork(e.to_string()));
                        }
                    }
                }
            }
        }

        let chat = ChatBridge::default();
        let clipboard = super::clipboard::feature(cfg.clipboard.clone());
        let files = FileTransfer::new(cfg.transfer.clone());
        let fsb = FilesBridge::new(FsConfig::default(), ClipFilesConfig::default()).with_starter(
            Arc::new(EngineStarter::new(files.clone(), cfg.transfer.conflict)),
        );
        fsb.link_engine(&files);
        let mut host = None;
        let mut host_events = None;
        if let Some((ep, true)) = &endpoint {
            let mut host_cfg = HostConfig {
                device_name: device_name.clone(),
                features: super::clipboard::register(
                    fsb.register(cfg.host.features.clone().with(files.clone())),
                    &clipboard,
                ),
                ..cfg.host.clone()
            };
            chat.register_host(&mut host_cfg);
            let whiteboard = Arc::new(WhiteboardHost::new());
            host_cfg.features.push(whiteboard.clone());
            match rt.block_on(Host::start(ep.clone(), host_cfg)) {
                Ok((h, ev)) => {
                    whiteboard.set_monitor_source(h.streamed_monitor_fn());
                    host = Some(h);
                    host_events = Some(ev);
                }
                Err(e) => {
                    tracing::error!(%e, "host failed to start");
                    problem = Some(HostProblem::Other(e.to_string()));
                }
            }
        }

        let mut discovery = host.as_ref().and_then(Host::discovery);
        if discovery.is_none() && cfg.fallback_discovery && endpoint.is_some() {
            match rt.block_on(Discovery::start(DiscoveryConfig::default())) {
                Ok(d) => discovery = Some(Arc::new(d)),
                Err(e) => tracing::warn!(%e, "LAN discovery unavailable"),
            }
        }

        let bridges: Bridges = Arc::default();
        let viewer = endpoint.as_ref().and_then(|(ep, _)| {
            let bridges = bridges.clone();
            let repaint = repaint.clone();
            let on_frame: tlp_desk_viewer::RepaintFn = Arc::new(move || {
                // Runs on the decode thread right after a frame was published (and after
                // events): move every new frame into the UI's slot without copying.
                for b in lock(&bridges).iter() {
                    if let Some(f) = b.src.take() {
                        b.dst.publish(VideoImage {
                            width: f.width,
                            height: f.height,
                            rgba: f.pixels,
                            seq: f.seq,
                        });
                    }
                }
                repaint();
            });
            let mut vcfg = ViewerConfig {
                known_hosts: cfg.known_hosts.clone(),
                pixel_format: PixelFormat::Rgba,
                on_frame: Some(on_frame),
                discovery: discovery.clone(),
                features: super::clipboard::register(
                    fsb.register(
                        FeatureSet::new()
                            .with(files.clone())
                            .with(whiteboard::viewer_feature()),
                    ),
                    &clipboard,
                ),
                ..ViewerConfig::default()
            };
            chat.register_viewer(&mut vcfg);
            match Viewer::new(ep.clone(), vcfg) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::error!(%e, "viewer failed to start");
                    None
                }
            }
        });

        let own_id = identity
            .as_ref()
            .map_or(DeviceId(0), |i| app_id(i.device_id()));
        let inner = Arc::new(Inner {
            rt: rt.clone(),
            tx,
            repaint,
            next_id: AtomicU64::new(1),
            endpoint: endpoint.map(|(ep, _)| ep),
            host,
            viewer,
            discovery,
            own_id,
            bridges,
            chat,
            files,
            state: Mutex::default(),
            recorders: Default::default(),
            clipboard,
            fsb,
        });
        let local = LocalInfo {
            device_id: own_id,
            device_name,
            addresses: lan_addresses(),
            one_time_code: inner.current_code(),
        };
        lock(&inner.state).local = Some(local);

        if let Some(p) = problem {
            inner.emit(BackendEvent::HostUnavailable(p));
        }
        if let Some(ev) = host_events {
            rt.spawn(inner.clone().pump_host(ev));
        }
        rt.spawn(inner.clone().pump_files());
        rt.spawn(inner.clone().pump_devices());
        rt.spawn(inner.clone().reconcile_loop());
        (Arc::new(RealBackend { inner }), rx)
    }
}

impl Inner {
    fn emit(&self, event: BackendEvent) {
        if self.tx.send(event).is_ok() {
            (self.repaint)();
        }
    }

    /// [`Inner::emit`] as a callback for other threads (a no-op once the backend is gone).
    fn emitter(self: &Arc<Self>) -> super::recording::Emit {
        let me = Arc::downgrade(self);
        Arc::new(move |event| {
            if let Some(me) = me.upgrade() {
                me.emit(event);
            }
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn current_code(&self) -> String {
        self.host
            .as_ref()
            .map(|h| h.one_time_code().current())
            .unwrap_or_default()
    }

    fn local(&self) -> LocalInfo {
        self.state().local.clone().unwrap_or(LocalInfo {
            device_id: self.own_id,
            device_name: String::new(),
            addresses: Vec::new(),
            one_time_code: String::new(),
        })
    }

    /// Apply `f` to the local info and emit it if it changed.
    fn update_local(&self, f: impl FnOnce(&mut LocalInfo)) {
        let changed = {
            let mut st = self.state();
            let Some(local) = st.local.as_mut() else {
                return;
            };
            let before = local.clone();
            f(local);
            (*local != before).then(|| local.clone())
        };
        if let Some(info) = changed {
            self.emit(BackendEvent::LocalInfo(info));
        }
    }

    // ----- file transfers (lane files-transfer) -----------------------------------------

    /// Forwards the engine's events of viewer sessions to the UI as [`BackendEvent::Transfer`].
    /// Transfers of host sessions (the other side started them) are not shown here.
    async fn pump_files(self: Arc<Self>) {
        let mut rx = self.files.subscribe();
        loop {
            let (key, event) = match rx.recv().await {
                Ok(x) => x,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            };
            let Some(session) = self.viewer_session_for_key(key) else {
                continue;
            };
            if let Some(snap) = self.files.snapshot(event.id()) {
                if snap.info.initiated_locally {
                    self.emit(BackendEvent::Transfer {
                        session,
                        view: transfer_view(&snap),
                    });
                }
            }
        }
    }

    fn viewer_session_for_key(&self, key: u64) -> Option<SessionId> {
        self.state()
            .viewer_sessions
            .iter()
            .find(|(_, s)| s.feature_key() == key)
            .map(|(id, _)| *id)
    }

    fn feature_key(&self, session: SessionId) -> Option<u64> {
        self.state()
            .viewer_sessions
            .get(&session)
            .map(|s| s.feature_key())
    }

    /// Runs `start` (an engine call) and reports a failure to the UI.
    fn spawn_transfer<F>(self: &Arc<Self>, session: SessionId, start: F)
    where
        F: std::future::Future<Output = Result<(), FilesError>> + Send + 'static,
    {
        let me = Arc::clone(self);
        self.rt.spawn(async move {
            if let Err(e) = start.await {
                tracing::warn!(%e, "transfer did not start");
                let error = match e {
                    FilesError::PermissionDenied => TransferStartError::NoPermission,
                    other => TransferStartError::Other(other.to_string()),
                };
                me.emit(BackendEvent::TransferNotStarted { session, error });
            }
        });
    }

    // ----- host side -------------------------------------------------------------------

    async fn pump_host(self: Arc<Self>, mut events: HostEvents) {
        while let Some(ev) = events.recv().await {
            match ev {
                HostEvent::IncomingRequest(r) => {
                    let request = RequestId(self.next_id());
                    self.state().requests.insert(request, r.id);
                    self.emit(BackendEvent::IncomingRequest {
                        request,
                        peer: app_peer(&r.peer),
                    });
                }
                HostEvent::RequestCancelled { id, reason } => {
                    tracing::info!(?reason, "accept prompt cancelled");
                    let found = {
                        let mut st = self.state();
                        let key = st.requests.iter().find(|(_, h)| **h == id).map(|(k, _)| *k);
                        if let Some(k) = key {
                            st.requests.remove(&k);
                        }
                        key
                    };
                    if let Some(request) = found {
                        self.emit(BackendEvent::IncomingRequestCancelled { request });
                    }
                }
                HostEvent::AuthFailed { peer, auth, reason } => {
                    tracing::warn!(peer = %peer.addr, device_id = %peer.device_id, ?auth, ?reason, "authentication failed");
                }
                HostEvent::CodeChanged => {
                    let code = self.current_code();
                    self.update_local(|l| l.one_time_code = code);
                }
                HostEvent::SessionStarted(v) => self.host_session_started(v.id, &v.peer),
                HostEvent::PermissionsChanged { .. } => {}
                HostEvent::InputBlocked { id } => {
                    tracing::warn!(session = id.0, "remote input blocked by the OS");
                    let session = {
                        let st = self.state();
                        st.host_sessions
                            .iter()
                            .find(|(_, h)| **h == id)
                            .map(|(k, _)| *k)
                    };
                    if let Some(session) = session {
                        self.emit(BackendEvent::HostInputBlocked { session });
                    }
                }
                HostEvent::SessionEnded { id, reason, .. } => {
                    self.host_session_ended(id, map_host_end(reason));
                }
                // FEATURE HOOK: lanes map their FeatureEvent payloads to BackendEvents in
                // their own `backend/<feature>.rs`; match on `event.feature` here.
                HostEvent::Feature { id, event } => {
                    // chat (TLP-chat)
                    if let Some(ev) = self
                        .app_host_session(id)
                        .and_then(|s| chat::backend_event(s, &event))
                    {
                        self.emit(ev);
                        continue;
                    }
                    tracing::debug!(session = id.0, feature = event.feature, "feature event");
                    let session = self.host_session_of(id);
                    if let Some(ev) = session.and_then(|s| super::clipboard::map(s, &event)) {
                        self.emit(ev);
                    }
                }
            }
        }
        tracing::debug!("host event stream ended");
    }

    /// The app's id for a host session.
    fn host_session_of(&self, id: HostSessionId) -> Option<SessionId> {
        self.state()
            .host_sessions
            .iter()
            .find(|(_, h)| **h == id)
            .map(|(s, _)| *s)
    }

    fn host_session_started(&self, id: HostSessionId, peer: &tlp_desk_net::PeerInfo) {
        let session = {
            let mut st = self.state();
            if st.host_sessions.values().any(|h| *h == id) {
                return;
            }
            let session = SessionId(self.next_id());
            st.host_sessions.insert(session, id);
            session
        };
        self.emit(BackendEvent::HostSessionStarted {
            session,
            peer: app_peer(peer),
        });
    }

    /// The app's id for a host session.
    fn app_host_session(&self, id: HostSessionId) -> Option<SessionId> {
        self.state()
            .host_sessions
            .iter()
            .find(|(_, h)| **h == id)
            .map(|(k, _)| *k)
    }

    fn host_session_ended(&self, id: HostSessionId, reason: EndReason) {
        let found = {
            let mut st = self.state();
            let key = st
                .host_sessions
                .iter()
                .find(|(_, h)| **h == id)
                .map(|(k, _)| *k);
            if let Some(k) = key {
                st.host_sessions.remove(&k);
            }
            key
        };
        if let Some(session) = found {
            self.emit(BackendEvent::SessionEnded { session, reason });
        }
    }

    /// Keeps the "who is connected" banner identical to [`Host::sessions`] even if an event
    /// was missed, and picks up LAN address changes.
    async fn reconcile_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(RECONCILE_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Some(host) = &self.host {
                let live = host.sessions();
                let known: Vec<HostSessionId> =
                    self.state().host_sessions.values().copied().collect();
                for v in &live {
                    if !known.contains(&v.id) {
                        self.host_session_started(v.id, &v.peer);
                    }
                }
                for id in known {
                    if !live.iter().any(|v| v.id == id) {
                        self.host_session_ended(id, EndReason::Remote);
                    }
                }
            }
            let addrs = lan_addresses();
            self.update_local(|l| l.addresses = addrs);
        }
    }

    // ----- discovery -------------------------------------------------------------------

    fn device_list(&self) -> Vec<DiscoveredDevice> {
        let Some(d) = &self.discovery else {
            return Vec::new();
        };
        d.devices()
            .into_iter()
            .filter(|d| app_id(d.device_id) != self.own_id)
            .filter_map(|d| {
                Some(DiscoveredDevice {
                    device_id: app_id(d.device_id),
                    address: d.addrs.first()?.ip(),
                    name: d.name,
                    online: true,
                })
            })
            .collect()
    }

    fn send_devices(&self, force: bool) {
        let list = self.device_list();
        {
            let mut st = self.state();
            if !force && st.last_devices.as_ref() == Some(&list) {
                return;
            }
            st.last_devices = Some(list.clone());
        }
        self.emit(BackendEvent::Devices(list));
    }

    async fn pump_devices(self: Arc<Self>) {
        let Some(disc) = self.discovery.clone() else {
            self.send_devices(true);
            return;
        };
        let mut events = disc.subscribe();
        disc.refresh().await;
        // Give mDNS / broadcast a moment so the first list is not needlessly empty.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        self.send_devices(true);
        let mut tick = tokio::time::interval(DEVICES_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                ev = events.recv() => match ev {
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        self.send_devices(false)
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
                _ = tick.tick() => self.send_devices(false),
            }
        }
    }

    // ----- viewer side -----------------------------------------------------------------

    async fn run_attempt(
        self: Arc<Self>,
        attempt: AttemptId,
        target: ConnectTarget,
        code: Option<String>,
    ) {
        let result = self.dial(attempt, target, code).await;
        match result {
            Ok((session, events)) => self.session_started(attempt, session, events),
            Err(e) => {
                tracing::info!(attempt = attempt.0, %e, "connect failed");
                let error = map_viewer_error(&e);
                let mut st = self.state();
                if st.attempts.remove(&attempt).is_none() {
                    return; // cancelled
                }
                if let ViewerError::HostKeyChanged { key, .. } = e {
                    st.changed_keys.insert(attempt, key);
                }
                drop(st);
                self.emit(BackendEvent::ConnectFailed { attempt, error });
            }
        }
    }

    async fn dial(
        self: &Arc<Self>,
        attempt: AttemptId,
        target: ConnectTarget,
        code: Option<String>,
    ) -> Result<(ViewerSession, ViewerEvents), ViewerError> {
        let Some(viewer) = &self.viewer else {
            return Err(ViewerError::Unreachable(
                "networking is unavailable on this device".into(),
            ));
        };
        let vt = match target {
            ConnectTarget::Id(id) => match tlp_desk_crypto::DeviceId::new(id.0) {
                Some(id) => ViewerTarget::Id(id),
                None => return Err(ViewerError::NotFound(id.to_string())),
            },
            ConnectTarget::Address(sa) => ViewerTarget::Addr(sa),
            ConnectTarget::Name(n) => ViewerTarget::Name(n),
        };
        let auth = match code {
            Some(c) if !c.trim().is_empty() => AuthMethod::OneTimeCode(c),
            _ => AuthMethod::AcceptPrompt,
        };
        let me = Arc::downgrade(self);
        let hook: AwaitingAcceptFn = Arc::new(move || {
            if let Some(me) = me.upgrade() {
                if me.state().attempts.contains_key(&attempt) {
                    me.emit(BackendEvent::AwaitingAccept { attempt });
                }
            }
        });
        viewer.connect_with_progress(vt, auth, Some(hook)).await
    }

    fn session_started(
        self: &Arc<Self>,
        attempt: AttemptId,
        session: ViewerSession,
        events: ViewerEvents,
    ) {
        let session = Arc::new(session);
        let id = SessionId(self.next_id());
        let src = session.frames();
        let recycle_to = src.clone();
        let frames = FrameSlot::with_recycler(Box::new(move |buf| recycle_to.recycle(buf)));
        {
            let mut st = self.state();
            if st.attempts.remove(&attempt).is_none() {
                // Cancelled while the handshake finished: dropping the session disconnects.
                return;
            }
            st.viewer_sessions.insert(id, session.clone());
        }
        lock(&self.bridges).push(Bridge {
            session: id,
            src,
            dst: frames.clone(),
        });
        let peer = app_peer(session.peer());
        tracing::info!(session = id.0, host = %peer.device_id, name = %peer.name, "viewer session started");
        self.emit(BackendEvent::SessionStarted {
            attempt,
            session: id,
            peer,
            permissions: session.permissions(),
            frames,
        });
        self.rt.spawn(self.clone().pump_viewer(id, events));
    }

    async fn pump_viewer(self: Arc<Self>, session: SessionId, mut events: ViewerEvents) {
        let mut reason = EndReason::Error("session event stream closed".into());
        while let Some(ev) = events.recv().await {
            match ev {
                ViewerEvent::PermissionsChanged(permissions) => {
                    self.emit(BackendEvent::PermissionsChanged {
                        session,
                        permissions,
                    });
                }
                ViewerEvent::Stats(s) => self.emit(BackendEvent::Stats {
                    session,
                    stats: SessionStats {
                        fps: s.fps,
                        latency_ms: s.latency_ms,
                        bitrate_kbps: s.bitrate_kbps.max(0.0).round() as u32,
                    },
                }),
                ViewerEvent::HostInputPriority(local_active) => {
                    self.emit(BackendEvent::HostInputPriority {
                        session,
                        local_active,
                    });
                }
                ViewerEvent::MonitorsChanged { monitors, active } => {
                    self.emit(BackendEvent::MonitorsChanged {
                        session,
                        monitors,
                        active,
                    });
                }
                // FEATURE HOOK: lanes map their FeatureEvent payloads to BackendEvents in
                // their own `backend/<feature>.rs`; match on `event.feature` here.
                ViewerEvent::Feature(event) => {
                    if let Some(op) = whiteboard::remote_op(&event) {
                        self.emit(BackendEvent::WhiteboardRemote { session, op });
                        continue;
                    }
                    // chat (TLP-chat)
                    if let Some(ev) = chat::backend_event(session, &event) {
                        self.emit(ev);
                        continue;
                    }
                    if let Some(ev) = FilesBridge::map_event(session, &event) {
                        self.emit(ev);
                        continue;
                    }
                    tracing::debug!(
                        session = session.0,
                        feature = event.feature,
                        "feature event"
                    );
                    if let Some(ev) = super::clipboard::map(session, &event) {
                        self.emit(ev);
                    }
                }
                ViewerEvent::Ended(r) => {
                    reason = map_viewer_end(r);
                    break;
                }
            }
        }
        self.state().viewer_sessions.remove(&session);
        lock(&self.bridges).retain(|b| b.session != session);
        self.recorders.stop(session, &self.rt, self.emitter());
        self.emit(BackendEvent::SessionEnded { session, reason });
    }
}

impl Backend for RealBackend {
    fn local_info(&self) -> LocalInfo {
        self.inner.local()
    }

    fn set_device_name(&self, name: String) {
        // The running endpoint and LAN announcement keep the old name until restart; the app
        // persists the new one and passes it to the next start.
        self.inner.update_local(|l| l.device_name = name);
    }

    fn refresh_one_time_code(&self) {
        if let Some(h) = &self.inner.host {
            h.rotate_code(); // emits CodeChanged -> LocalInfo
        }
    }

    fn connect(&self, target: ConnectTarget, code: Option<String>) -> AttemptId {
        let attempt = AttemptId(self.inner.next_id());
        let me = Arc::clone(&self.inner);
        // Hold the lock while spawning so the task cannot finish before it is registered.
        let mut st = self.inner.state();
        let handle = self
            .inner
            .rt
            .spawn(me.run_attempt(attempt, target, code))
            .abort_handle();
        st.attempts.insert(attempt, handle);
        attempt
    }

    fn cancel_connect(&self, attempt: AttemptId) {
        if let Some(h) = self.inner.state().attempts.remove(&attempt) {
            // Dropping the connect future closes the QUIC connection; the host dismisses its
            // prompt.
            h.abort();
        }
    }

    fn forget_host_key(&self, attempt: AttemptId) -> bool {
        let Some(key) = self.inner.state().changed_keys.remove(&attempt) else {
            return false;
        };
        let Some(viewer) = &self.inner.viewer else {
            return false;
        };
        match viewer.forget(&key) {
            Ok(()) => {
                tracing::warn!(?key, "user chose to forget a changed host key");
                true
            }
            Err(e) => {
                tracing::error!(%e, ?key, "could not forget host key");
                false
            }
        }
    }

    fn respond_to_request(&self, request: RequestId, accept: bool) {
        let Some(id) = self.inner.state().requests.remove(&request) else {
            return;
        };
        if let Some(h) = &self.inner.host {
            if accept {
                h.accept(id, None);
            } else {
                h.reject(id);
            }
        }
    }

    fn send_input(&self, session: SessionId, event: InputEvent) {
        let s = self.inner.state().viewer_sessions.get(&session).cloned();
        if let Some(s) = s {
            s.send_input(event);
        }
    }

    fn set_view_only(&self, _session: SessionId, _view_only: bool) {
        // Input gating happens in the UI (and permissions are enforced by the host).
    }

    fn select_monitor(&self, session: SessionId, index: u32) {
        let s = self.inner.state().viewer_sessions.get(&session).cloned();
        if let Some(s) = s {
            s.select_monitor(index);
        }
    }

    fn send_whiteboard(&self, session: SessionId, ops: Vec<WhiteboardOp>) {
        let s = self.inner.state().viewer_sessions.get(&session).cloned();
        if let Some(s) = s {
            for msg in whiteboard::messages(&ops) {
                s.send_message(msg);
            }
        }
    }

    fn send_chat(&self, session: SessionId, text: &str) -> Result<ChatLine, ChatSendError> {
        let (viewer, host) = {
            let st = self.inner.state();
            (
                st.viewer_sessions.get(&session).map(|s| s.feature_key()),
                st.host_sessions.get(&session).copied(),
            )
        };
        match (viewer, host) {
            (Some(key), _) => self.inner.chat.send_viewer(key, text),
            (None, Some(id)) => self.inner.chat.send_host(id.0, text),
            (None, None) => Err(ChatSendError::Unavailable),
        }
    }

    fn set_chat_allowed(&self, session: SessionId, allowed: bool) {
        let Some(id) = self.inner.state().host_sessions.get(&session).copied() else {
            return;
        };
        let Some(host) = &self.inner.host else {
            return;
        };
        let Some(current) = host
            .sessions()
            .into_iter()
            .find(|v| v.id == id)
            .map(|v| v.permissions)
        else {
            return;
        };
        let chat = tlp_desk_proto::Permissions::CHAT;
        let new = if allowed {
            current | chat
        } else {
            current.difference(chat)
        };
        if new != current {
            tracing::info!(
                session = session.0,
                allowed,
                "host user changed chat permission"
            );
            host.set_permissions(id, new);
        }
    }

    fn fs_request(&self, side: FsSide, op: FsOp) -> Option<FsRequestId> {
        Some(self.files_request(side, op))
    }

    fn fs_transfer(&self, session: SessionId, request: TransferRequest) {
        self.files_transfer(session, request);
    }

    fn send_files(&self, session: SessionId, paths: Vec<PathBuf>) {
        let Some(key) = self.inner.feature_key(session) else {
            return;
        };
        let ft = self.inner.files.clone();
        self.inner.spawn_transfer(session, async move {
            ft.start_upload(key, paths, String::new()).await.map(|_| ())
        });
    }

    fn download_files(&self, session: SessionId, remote_paths: Vec<String>, dest: PathBuf) {
        let Some(key) = self.inner.feature_key(session) else {
            return;
        };
        let ft = self.inner.files.clone();
        self.inner.spawn_transfer(session, async move {
            ft.start_download(key, remote_paths, dest).await.map(|_| ())
        });
    }

    fn cancel_transfer(&self, _session: SessionId, id: TransferId) {
        self.inner.files.cancel(tlp_desk_files::TransferId(id.0));
    }

    fn retry_transfer(&self, session: SessionId, id: TransferId) {
        let Some(key) = self.inner.feature_key(session) else {
            return;
        };
        let ft = self.inner.files.clone();
        self.inner.spawn_transfer(session, async move {
            ft.resume(tlp_desk_files::TransferId(id.0), key).await
        });
    }

    fn disconnect(&self, session: SessionId) {
        let (viewer, host) = {
            let st = self.inner.state();
            (
                st.viewer_sessions.get(&session).cloned(),
                st.host_sessions.get(&session).copied(),
            )
        };
        if let Some(v) = viewer {
            v.disconnect(); // Ended(Local) -> SessionEnded via pump_viewer
        } else if let Some(id) = host {
            if let Some(h) = &self.inner.host {
                h.disconnect(id); // SessionEnded via pump_host
            }
        }
    }

    // --- power (TLP-power) ---
    // recording (TLP-recording)
    fn start_recording(&self, session: SessionId) {
        let s = self.inner.state().viewer_sessions.get(&session).cloned();
        if let Some(s) = s {
            self.inner
                .recorders
                .start(session, &s, self.inner.emitter());
        }
    }

    fn stop_recording(&self, session: SessionId) {
        self.inner
            .recorders
            .stop(session, &self.inner.rt, self.inner.emitter());
    }

    // --- clipboard (TLP-clipboard) ---
    fn set_clipboard_sync(&self, session: SessionId, enabled: bool) {
        let (viewer, host) = {
            let st = self.inner.state();
            (
                st.viewer_sessions.get(&session).map(|v| v.feature_key()),
                st.host_sessions.get(&session).map(|h| h.0),
            )
        };
        let clip = &self.inner.clipboard;
        if let Some(key) = viewer {
            clip.set_sync_enabled(tlp_desk_proto::Role::Viewer, key, enabled);
        } else if let Some(key) = host {
            clip.set_sync_enabled(tlp_desk_proto::Role::Host, key, enabled);
        }
    }

    fn set_clipboard_default(&self, enabled: bool) {
        self.inner.clipboard.set_default_enabled(enabled);
    }
}

// --- files (TLP-files) ---
impl RealBackend {
    /// Feature key of a viewer session (what the files features are keyed by).
    fn feature_key(&self, session: SessionId) -> Option<u64> {
        self.inner
            .state()
            .viewer_sessions
            .get(&session)
            .map(|s| s.feature_key())
    }

    fn emitter(&self) -> impl Fn(BackendEvent) + Send + Sync + 'static {
        let inner = Arc::downgrade(&self.inner);
        move |ev| {
            if let Some(inner) = inner.upgrade() {
                inner.emit(ev);
            }
        }
    }

    fn files_request(&self, side: FsSide, op: FsOp) -> FsRequestId {
        let request = FsRequestId(self.inner.next_id());
        let key = match side {
            FsSide::Local => None,
            FsSide::Remote(s) => self.feature_key(s),
        };
        self.inner
            .fsb
            .request(&self.inner.rt, side, key, op, request, self.emitter());
        request
    }

    fn files_transfer(&self, session: SessionId, request: TransferRequest) {
        let key = self.feature_key(session);
        self.inner
            .fsb
            .transfer(&self.inner.rt, session, key, request, self.emitter());
    }
}

impl Drop for RealBackend {
    /// App exit: say goodbye to every peer, then close the endpoint so they notice at once.
    fn drop(&mut self) {
        // Close recording files properly before the sessions go away.
        self.inner.recorders.stop_all_blocking();
        if let Some(h) = &self.inner.host {
            h.shutdown();
        }
        let sessions: Vec<_> = self
            .inner
            .state()
            .viewer_sessions
            .values()
            .cloned()
            .collect();
        for s in &sessions {
            s.disconnect();
        }
        if let Some(ep) = &self.inner.endpoint {
            // Let the Bye messages leave (they are sent on the runtime's worker threads).
            std::thread::sleep(Duration::from_millis(150));
            ep.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lan_address_filter() {
        for ok in [
            "192.168.1.2",
            "10.1.2.3",
            "172.16.0.9",
            "169.254.10.1",
            "fd00::1",
        ] {
            assert!(is_lan_address(ok.parse().unwrap()), "{ok}");
        }
        for no in ["127.0.0.1", "8.8.8.8", "fe80::1", "::1", "2001:db8::1"] {
            assert!(!is_lan_address(no.parse().unwrap()), "{no}");
        }
    }

    #[test]
    fn lan_addresses_reads_interfaces_without_network() {
        // Must not panic or block; every result passes the filter.
        let v = lan_addresses();
        assert!(v.iter().all(|ip| is_lan_address(*ip)));
    }

    #[test]
    fn fingerprint_is_shortened_in_groups() {
        let fp = Fingerprint([0xab; 32]);
        assert_eq!(short_fingerprint(&fp), "abab abab abab abab");
    }

    #[test]
    fn viewer_errors_map_to_ui_errors() {
        let fp = Fingerprint([1; 32]);
        let e = ViewerError::HostKeyChanged {
            key: HostKey::Addr("10.0.0.2".into()),
            previous: fp,
            presented: Fingerprint([2; 32]),
            presented_id: Fingerprint([2; 32]).device_id(),
        };
        assert!(matches!(
            map_viewer_error(&e),
            ConnectError::HostKeyChanged { previous, presented }
                if previous == "0101 0101 0101 0101" && presented == "0202 0202 0202 0202"
        ));
        assert_eq!(
            map_viewer_error(&ViewerError::NotAllowed("8.8.8.8:47400".parse().unwrap())),
            ConnectError::NotLocalNetwork
        );
        assert_eq!(
            map_viewer_error(&ViewerError::Rejected(RejectReason::UserDeclined)),
            ConnectError::Rejected(RejectReason::UserDeclined)
        );
        assert_eq!(
            map_viewer_error(&ViewerError::HostAuthFailed),
            ConnectError::IdentityMismatch
        );
    }
}
