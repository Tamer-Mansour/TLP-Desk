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

mod unattended;

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tlp_desk_crypto::{
    Fingerprint, HostKey, Identity, PasswordStore, TicketStore, DEFAULT_TICKET_TTL, TICKETS_FILE,
};
use tlp_desk_host::{
    EndReason as HostEnd, Host, HostConfig, HostEvent, HostEvents, RequestId as HostRequestId,
    SessionId as HostSessionId, SystemPower, UnattendedFeature, WindowsPower,
};
use tlp_desk_net::{
    default_device_name, AwaitingAcceptFn, Discovery, DiscoveryConfig, Endpoint, EndpointConfig,
    FeatureSet, NetError, DEFAULT_PORT,
};
use tlp_desk_proto::{InputEvent, Permissions, RejectReason};
use tlp_desk_viewer::{
    AuthMethod, ConnectTarget as ViewerTarget, EndReason as ViewerEnd, KnownHostsStore,
    PixelFormat, ReconnectTickets, Viewer, ViewerConfig, ViewerError, ViewerEvent, ViewerEvents,
    ViewerSession,
};
use tokio::runtime::Handle;
use tokio::task::AbortHandle;

use super::{
    AttemptId, Backend, BackendEvent, ConnectError, ConnectTarget, DeviceId, DiscoveredDevice,
    EndReason, EventReceiver, EventSender, FrameSlot, HostProblem, LocalInfo, PeerInfo, RepaintFn,
    RequestId, SessionId, SessionStats, VideoImage,
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
    /// Restarts / locks this machine when a viewer with `RESTART` / `SYSTEM` asks (TLP-unattended).
    /// Tests that exercise those requests must swap in a fake.
    pub power: Arc<dyn SystemPower>,
    /// Grant `RESTART` and `SYSTEM` (restart, lock, Ctrl+Alt+Del) to new sessions in addition
    /// to `host.default_permissions`, so unattended access can restart and lock (TLP-unattended).
    pub system_permissions: bool,
}

impl Default for RealConfig {
    fn default() -> Self {
        Self {
            data_dir: tlp_desk_crypto::default_dir(),
            bind: SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), DEFAULT_PORT),
            device_name: None,
            known_hosts: KnownHostsStore::Default,
            host: HostConfig::default(),
            fallback_discovery: true,
            power: Arc::new(WindowsPower),
            system_permissions: true,
        }
    }
}

/// Real implementation of [`Backend`].
pub struct RealBackend {
    inner: Arc<Inner>,
    /// Background loops (they hold the shared state); stopped on drop so the endpoint, and
    /// with it the UDP port, is released when the backend goes away.
    tasks: Vec<AbortHandle>,
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
    state: Mutex<State>,
    /// Reconnect tickets received from hosts we asked to restart (TLP-unattended).
    keeper: ReconnectTickets,
    /// Where the permanent password's verifier lives (`None` without an identity).
    password_store: Option<PasswordStore>,
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
    /// Permanent password state shown in Settings (TLP-unattended).
    password: super::unattended::PasswordState,
    /// Viewer sessions waiting for their restarted host (TLP-unattended).
    reconnects: HashMap<SessionId, AbortHandle>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
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

        // Unattended access (TLP-unattended): permanent password, reconnect tickets, restart.
        let password_store = identity.as_ref().map(|_| PasswordStore::new(&cfg.data_dir));
        let stored_password = password_store.as_ref().and_then(|s| {
            s.load()
                .map_err(|e| tracing::warn!(%e, "cannot read the permanent password; disabled"))
                .ok()
                .flatten()
        });
        let tickets = TicketStore::load(&cfg.data_dir.join(TICKETS_FILE), DEFAULT_TICKET_TTL)
            .unwrap_or_else(|e| {
                tracing::warn!(%e, "cannot read reconnect tickets; starting empty");
                TicketStore::in_memory(DEFAULT_TICKET_TTL)
            });
        let mut host = None;
        let mut host_events = None;
        if let Some((ep, true)) = &endpoint {
            let mut host_cfg = HostConfig {
                device_name: device_name.clone(),
                ..cfg.host.clone()
            };
            if host_cfg.password.is_none() {
                host_cfg.password = stored_password.clone();
            }
            host_cfg.reconnect_tickets = Some(tickets.clone());
            host_cfg.features.push(Arc::new(UnattendedFeature::new(
                Some(tickets.clone()),
                cfg.power.clone(),
            )));
            if cfg.system_permissions {
                host_cfg.default_permissions = host_cfg
                    .default_permissions
                    .union(Permissions::RESTART)
                    .union(Permissions::SYSTEM);
            }
            match rt.block_on(Host::start(ep.clone(), host_cfg)) {
                Ok((h, ev)) => {
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
        let keeper = ReconnectTickets::new();
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
            let vcfg = ViewerConfig {
                known_hosts: cfg.known_hosts.clone(),
                pixel_format: PixelFormat::Rgba,
                on_frame: Some(on_frame),
                discovery: discovery.clone(),
                features: FeatureSet::new().with(keeper.feature()),
                ..ViewerConfig::default()
            };
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
            state: Mutex::default(),
            keeper,
            password_store: password_store.clone(),
        });
        let local = LocalInfo {
            device_id: own_id,
            device_name,
            addresses: lan_addresses(),
            one_time_code: inner.current_code(),
        };
        lock(&inner.state).local = Some(local);
        lock(&inner.state).password = super::unattended::PasswordState {
            set: inner.host.is_some() && stored_password.is_some(),
            machine_wide: super::unattended::is_machine_wide(&cfg.data_dir),
            admin: false,
        };

        if let Some(p) = problem {
            inner.emit(BackendEvent::HostUnavailable(p));
        }
        let mut tasks = Vec::new();
        if let Some(ev) = host_events {
            tasks.push(rt.spawn(inner.clone().pump_host(ev)).abort_handle());
        }
        tasks.push(rt.spawn(inner.clone().pump_devices()).abort_handle());
        tasks.push(rt.spawn(inner.clone().reconcile_loop()).abort_handle());
        tasks.push(rt.spawn(inner.clone().probe_elevation()).abort_handle());
        (Arc::new(RealBackend { inner, tasks }), rx)
    }
}

impl Inner {
    fn emit(&self, event: BackendEvent) {
        if self.tx.send(event).is_ok() {
            (self.repaint)();
        }
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
                HostEvent::SessionEnded { id, reason, .. } => {
                    self.host_session_ended(id, map_host_end(reason));
                }
                // FEATURE HOOK: lanes map their FeatureEvent payloads to BackendEvents in
                // their own `backend/<feature>.rs`; match on `event.feature` here.
                HostEvent::Feature { id, event } => {
                    tracing::debug!(session = id.0, feature = event.feature, "feature event");
                }
            }
        }
        tracing::debug!("host event stream ended");
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
        let auth = match code {
            Some(c) if !c.trim().is_empty() => AuthMethod::OneTimeCode(c),
            _ => AuthMethod::AcceptPrompt,
        };
        self.run_attempt_auth(attempt, target, auth).await;
    }

    async fn run_attempt_auth(
        self: Arc<Self>,
        attempt: AttemptId,
        target: ConnectTarget,
        auth: AuthMethod,
    ) {
        let result = self.dial(attempt, target, auth).await;
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
        auth: AuthMethod,
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
        let mut raw_end = None;
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
                    tracing::debug!(
                        session = session.0,
                        feature = event.feature,
                        "feature event"
                    );
                }
                ViewerEvent::Ended(r) => {
                    raw_end = Some(r.clone());
                    reason = map_viewer_end(r);
                    break;
                }
            }
        }
        let old = self.state().viewer_sessions.remove(&session);
        let dst = {
            let mut bridges = lock(&self.bridges);
            let dst = bridges
                .iter()
                .find(|b| b.session == session)
                .map(|b| b.dst.clone());
            bridges.retain(|b| b.session != session);
            dst
        };
        // A host we asked to restart left a ticket: wait for it instead of ending.
        if let (Some(old), Some(raw)) = (&old, &raw_end) {
            if self.try_start_reconnect(session, old, raw, dst) {
                return;
            }
        }
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

    fn connect_with_password(&self, target: ConnectTarget, password: String) -> AttemptId {
        let attempt = AttemptId(self.inner.next_id());
        let me = Arc::clone(&self.inner);
        let mut st = self.inner.state();
        let handle = self
            .inner
            .rt
            .spawn(me.run_attempt_auth(attempt, target, AuthMethod::Password(password)))
            .abort_handle();
        st.attempts.insert(attempt, handle);
        attempt
    }

    fn set_unattended_password(&self, current: Option<String>, new: Option<String>) {
        let me = Arc::clone(&self.inner);
        self.inner.rt.spawn(me.change_password(current, new));
    }

    fn unattended_password_state(&self) -> super::unattended::PasswordState {
        self.inner.password_state()
    }

    fn system_action(&self, session: SessionId, action: super::unattended::SystemAction) {
        self.inner.system_action(session, action);
    }

    fn cancel_reconnect(&self, session: SessionId) {
        if !self.inner.cancel_reconnect(session) {
            self.disconnect(session);
        }
    }

    fn disconnect(&self, session: SessionId) {
        if self.inner.cancel_reconnect(session) {
            return;
        }
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
}

impl Drop for RealBackend {
    /// App exit: say goodbye to every peer, then close the endpoint so they notice at once.
    fn drop(&mut self) {
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
        for h in self.inner.state().reconnects.values() {
            h.abort();
        }
        for t in &self.tasks {
            t.abort();
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
