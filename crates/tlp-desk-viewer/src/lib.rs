//! Viewer session logic: connect to a host (with key pinning / trust-on-first-use), decode its
//! video into a latest-frame slot, send input.
//!
//! ```text
//! let viewer = Viewer::new(endpoint.clone(), ViewerConfig::default())?;   // shared endpoint
//! let (session, mut events) = viewer.connect("123 456 789".parse()?, AuthMethod::AcceptPrompt).await?;
//! // UI thread, every paint: if let Some(f) = session.take_frame() { upload f.pixels }
//! session.send_input(InputEvent::MouseMove { x, y });
//! ```
//!
//! * **Pinning.** Dialing by device ID pins the host key to that ID; once a host was seen its
//!   full fingerprint is remembered in the known-hosts file and pinned on every later connect.
//!   A remembered key that changed fails with [`ViewerError::HostKeyChanged`], which the UI
//!   must show loudly; only an explicit [`Viewer::forget`] clears it.
//! * **LAN only.** Addresses outside the allow-list (default: private, link-local, loopback)
//!   are refused before any packet is sent.
//! * **Features.** Register feature modules in [`ViewerConfig::features`]; each session starts
//!   them and routes the host's feature messages to them ([`ViewerEvent::Feature`] for the UI).
//!   Privacy mode: [`PrivacyViewerFeature`] + [`privacy_request`].
//! * **Restart and reconnect.** [`ReconnectTickets`] keeps the ticket a host sends before it
//!   restarts; [`Viewer::reconnect_with_ticket`] gets back in once the host is up again.
//! * **Performance.** Each session sends `ViewerFeedback` ~4x/s for the host's adaptive
//!   bitrate and `LatencyProbe` every 500 ms to synchronize clocks for the latency overlay
//!   ([`ViewerStats::latency_ms`], method in [`LatencyClock`]); the UI picks presets with
//!   [`ViewerSession::set_quality`] and the "all monitors" view with
//!   [`ViewerSession::select_all_monitors`].
//! * **Recording.** [`Recording::start`] writes what a session receives to an MP4 file;
//!   [`recording::Player`] plays such files back (see the [`recording`] module).
//! * **Threads.** Decoding runs on a dedicated std thread; networking on tokio tasks. Cancel a
//!   pending [`Viewer::connect`] by dropping its future (or aborting its task).
#![forbid(unsafe_code)]

mod device_info;
mod frame;
pub mod netsim;
mod perf;
mod privacy;
mod reconnect;
pub mod recording;
mod session;
mod tunnel;
mod whiteboard;

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tlp_desk_crypto::{CryptoError, DeviceId, Fingerprint, HostKey, KnownHosts, KNOWN_HOSTS_FILE};
use tlp_desk_net::relay::RelayConfig;
use tlp_desk_net::{
    AllowList, DialFailure, DiscoveredDevice, Discovery, Endpoint, NetError, Pin, DEFAULT_PORT,
};
use tlp_desk_proto::RejectReason;

pub use device_info::{
    request_device_info, sanitize as sanitize_device_info, DeviceInfoClient, DEVICE_INFO_FEATURE,
};
pub use frame::{DecodedFrame, FrameSlot, PixelFormat};
pub use netsim::NetSim;
pub use perf::LatencyClock;
pub use privacy::{privacy_request, PrivacyState, PrivacyViewerFeature, PRIVACY_FEATURE};
pub use reconnect::{
    AttemptFn, HeldTicket, ReconnectPolicy, ReconnectTickets, TicketReceived, RECONNECT_FEATURE,
};
pub use recording::{Recording, RecordingConfig, RecordingSummary};
pub use session::{EndReason, ViewerEvent, ViewerEvents, ViewerSession, ViewerStats};
pub use tlp_desk_net::{
    AuthMethod, AwaitingAcceptFn, Feature, FeatureCtx, FeatureEvent, FeatureSet, PeerInfo,
};
pub use tlp_desk_proto::{DeviceInfo, DiskInfo};
pub use tlp_desk_proto::{MonitorDesc, QualityPreset, QualitySettings};
pub use tunnel::{
    ForwardSpec, TunnelCloseReason, TunnelEvent, TunnelId, TunnelStatus, TunnelViewerConfig,
    ViewerTunnels, TUNNEL_FEATURE,
};
pub use whiteboard::{WhiteboardEvent, WhiteboardViewer};

/// Callback invoked (from any thread) when a new frame or event is available, e.g. to wake
/// the GUI for a repaint.
pub type RepaintFn = Arc<dyn Fn() + Send + Sync>;

/// What to connect to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectTarget {
    /// Device ID, resolved through LAN discovery; the host key is pinned to the ID.
    Id(DeviceId),
    /// Device ID at a known address (no discovery needed); pinned to the ID.
    IdAt(DeviceId, SocketAddr),
    /// Plain address: trust-on-first-use, then pinned to the remembered key.
    Addr(SocketAddr),
    /// Device name as announced on the LAN (resolved through discovery).
    Name(String),
    /// A device from the discovery list.
    Device(DiscoveredDevice),
}

impl FromStr for ConnectTarget {
    type Err = std::convert::Infallible;

    /// `123 456 789` -> `Id`; `192.168.1.20`, `192.168.1.20:5000`, `[fe80::1]:47400` ->
    /// `Addr`; `123456789@192.168.1.20[:port]` -> `IdAt`; anything else -> `Name`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if let Some((id, addr)) = s.split_once('@') {
            if let (Ok(id), Some(addr)) = (id.parse::<DeviceId>(), parse_addr(addr)) {
                return Ok(Self::IdAt(id, addr));
            }
        }
        if let Ok(id) = s.parse::<DeviceId>() {
            return Ok(Self::Id(id));
        }
        if let Some(addr) = parse_addr(s) {
            return Ok(Self::Addr(addr));
        }
        Ok(Self::Name(s.to_owned()))
    }
}

fn parse_addr(s: &str) -> Option<SocketAddr> {
    let s = s.trim();
    s.parse::<SocketAddr>().ok().or_else(|| {
        s.parse::<IpAddr>()
            .ok()
            .map(|ip| SocketAddr::new(ip, DEFAULT_PORT))
    })
}

impl std::fmt::Display for ConnectTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Id(id) => write!(f, "{id}"),
            Self::IdAt(id, a) => write!(f, "{id} @ {a}"),
            Self::Addr(a) => write!(f, "{a}"),
            Self::Name(n) => f.write_str(n),
            Self::Device(d) => write!(f, "{} ({})", d.name, d.device_id),
        }
    }
}

/// Where remembered host keys live.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum KnownHostsStore {
    /// `known_hosts` in [`tlp_desk_crypto::default_dir`].
    #[default]
    Default,
    File(PathBuf),
    /// Not persisted (tests).
    InMemory,
}

/// How long the whole relay exchange (ask, punch, wait for the host) may take.
const RELAY_REQUEST_LIMIT: Duration = Duration::from_secs(12);

/// Viewer settings.
#[derive(Clone)]
pub struct ViewerConfig {
    pub known_hosts: KnownHostsStore,
    pub pixel_format: PixelFormat,
    /// Called after each published frame and each event.
    pub on_frame: Option<RepaintFn>,
    /// Needed to resolve [`ConnectTarget::Id`] / [`ConnectTarget::Name`]; may be set later
    /// with [`Viewer::set_discovery`] (e.g. the host's discovery instance).
    pub discovery: Option<Arc<Discovery>>,
    /// How long to wait for an ID/name to show up in discovery.
    pub resolve_timeout: Duration,
    /// Addresses we may dial. Default: LAN ranges only.
    pub allow_list: AllowList,
    /// A relay to try when a device ID is not on this network (TLP-relay). The relay's own
    /// address is dialed even though it is outside `allow_list`.
    pub relay: Option<RelayConfig>,
    /// Session features (files, clipboard, chat, ...), started for every session; they report
    /// through [`ViewerEvent::Feature`].
    // FEATURE HOOK: lanes do not edit this file; the app/CLI builds the set.
    pub features: FeatureSet,
}

impl Default for ViewerConfig {
    fn default() -> Self {
        Self {
            known_hosts: KnownHostsStore::Default,
            pixel_format: PixelFormat::Bgra,
            on_frame: None,
            discovery: None,
            resolve_timeout: Duration::from_secs(4),
            allow_list: AllowList::default(),
            relay: None,
            features: FeatureSet::new(),
        }
    }
}

impl std::fmt::Debug for ViewerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewerConfig")
            .field("known_hosts", &self.known_hosts)
            .field("pixel_format", &self.pixel_format)
            .field("discovery", &self.discovery.is_some())
            .field("resolve_timeout", &self.resolve_timeout)
            .field("relay", &self.relay.as_ref().map(|r| &r.addr))
            .field("features", &self.features)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ViewerError {
    /// The ID/name was not found on the LAN.
    #[error("{0} was not found on the local network")]
    NotFound(String),
    /// The ID was not on the LAN and the relay does not know it either.
    #[error("{0} was not found on the local network or through the relay")]
    NotFoundAnywhere(String),
    /// The relay could not be used (unreachable, wrong token, timed out).
    #[error("relay: {0}")]
    Relay(String),
    /// The address is outside the LAN allow-list.
    #[error("{0} is not a local-network address")]
    NotAllowed(SocketAddr),
    #[error("host unreachable: {0}")]
    Unreachable(String),
    #[error("rejected by host: {0:?}")]
    Rejected(RejectReason),
    /// Dialed by ID, but the host's key does not hash to that ID (wrong device or MITM).
    #[error("the device at this address is {presented}, not {expected}")]
    IdentityMismatch {
        expected: DeviceId,
        presented: DeviceId,
    },
    /// The host's key differs from the one remembered for it. Possible man-in-the-middle, or
    /// the host was reinstalled. Show loudly; only [`Viewer::forget`] clears it.
    #[error("WARNING: host key changed for {key:?} (was {previous}, now {presented})")]
    HostKeyChanged {
        key: HostKey,
        previous: Fingerprint,
        presented: Fingerprint,
        presented_id: DeviceId,
    },
    /// The target is this device itself: its own ID, or an address that answered with this
    /// install's key. Nothing was sent to anyone.
    #[error("that is this device; connect to another device's ID or address")]
    SelfConnection,
    /// The host failed SPAKE2 key confirmation (it does not know the code/password).
    #[error("host failed to prove it knows the code/password")]
    HostAuthFailed,
    #[error("protocol version mismatch: ours {ours}, host {theirs}")]
    VersionMismatch { ours: u32, theirs: u32 },
    #[error("known-hosts store: {0}")]
    KnownHosts(#[from] CryptoError),
    #[error(transparent)]
    Net(NetError),

    // --- tunnels: port forwarding (TLP-tunnel) ---
    /// The host has not granted `TUNNEL` in this session.
    #[error("the host has not allowed port forwarding in this session")]
    TunnelNotAllowed,
    /// The host refused the tunnel (bad or non-LAN target, limits...).
    #[error("the host refused port forwarding: {0}")]
    TunnelRefused(TunnelCloseReason),
    /// The local listening socket could not be opened (port in use, no rights...).
    #[error("cannot listen on {addr}: {reason}")]
    TunnelBind { addr: SocketAddr, reason: String },
    /// Listening on a non-loopback address exposes the remote service to the whole network;
    /// the caller must confirm it with `ForwardSpec::allow_lan_bind`.
    #[error("{0} is reachable from other devices; LAN binding must be confirmed")]
    TunnelLanBind(SocketAddr),
    /// A forwarding rule could not be parsed.
    #[error("invalid forwarding rule {0:?}: use LOCAL_PORT:REMOTE_HOST:REMOTE_PORT")]
    TunnelSpec(String),
    /// The maximum number of forwarding rules for this session is reached.
    #[error("too many port forwarding rules")]
    TunnelLimit,
    /// The session is over (or unknown to the tunnel feature).
    #[error("the session has ended")]
    SessionEnded,
    /// Reading or writing a recording file failed.
    #[error("recording file: {0}")]
    RecordingIo(#[source] std::io::Error),
    /// Not enough free disk space to (keep) recording.
    #[error("only {available} bytes free on the recording disk, {needed} needed")]
    DiskFull { available: u64, needed: u64 },
    /// A recording file is damaged or not one of ours.
    #[error("unusable recording: {0}")]
    BadRecording(&'static str),
    /// The received H.264 stream could not be parsed for recording.
    #[error("unrecordable video: {0}")]
    BadVideo(&'static str),
    /// The video decoder failed (playback).
    #[error("video decoder: {0}")]
    Decoder(String),
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The key a plain address is remembered under: the IP, plus the port if not the default.
pub fn addr_host_key(addr: SocketAddr) -> HostKey {
    let ip = tlp_desk_net::normalize(addr.ip());
    if addr.port() == DEFAULT_PORT {
        HostKey::Addr(ip.to_string())
    } else {
        HostKey::Addr(SocketAddr::new(ip, addr.port()).to_string())
    }
}

/// Dials hosts. Cheap to share (`Arc<Viewer>`); several sessions may run at once.
pub struct Viewer {
    endpoint: Arc<Endpoint>,
    known: Mutex<KnownHosts>,
    cfg: ViewerConfig,
    discovery: Mutex<Option<Arc<Discovery>>>,
    relay: Mutex<Option<RelayConfig>>,
}

impl std::fmt::Debug for Viewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Viewer")
            .field("cfg", &self.cfg)
            .finish_non_exhaustive()
    }
}

impl Viewer {
    /// Loads the known-hosts store. `endpoint` is the process-wide endpoint (shared with the
    /// host side; a dial-only endpoint with `listen: false` also works).
    pub fn new(endpoint: Arc<Endpoint>, cfg: ViewerConfig) -> Result<Self, ViewerError> {
        let known = match &cfg.known_hosts {
            KnownHostsStore::Default => {
                KnownHosts::load(&tlp_desk_crypto::default_dir().join(KNOWN_HOSTS_FILE))?
            }
            KnownHostsStore::File(p) => KnownHosts::load(p)?,
            KnownHostsStore::InMemory => KnownHosts::in_memory(),
        };
        Ok(Self {
            endpoint,
            known: Mutex::new(known),
            discovery: Mutex::new(cfg.discovery.clone()),
            relay: Mutex::new(cfg.relay.clone()),
            cfg,
        })
    }

    pub fn set_discovery(&self, discovery: Option<Arc<Discovery>>) {
        *lock(&self.discovery) = discovery;
    }

    /// Changes the relay used for IDs that are not on this network (`None` = LAN only).
    pub fn set_relay(&self, relay: Option<RelayConfig>) {
        *lock(&self.relay) = relay;
    }

    /// The relay currently configured, if any.
    pub fn relay(&self) -> Option<RelayConfig> {
        lock(&self.relay).clone()
    }

    /// Forgets a remembered host key (after the user confirmed a [`ViewerError::HostKeyChanged`]
    /// warning). The next connect trusts the new key on first use.
    pub fn forget(&self, key: &HostKey) -> Result<(), ViewerError> {
        let mut k = lock(&self.known);
        k.forget(key);
        k.save()?;
        Ok(())
    }

    /// Remembered fingerprint for a host, if any.
    pub fn known_fingerprint(&self, key: &HostKey) -> Option<Fingerprint> {
        lock(&self.known).fingerprint_for(key)
    }

    /// Connects and authorizes. For [`AuthMethod::AcceptPrompt`] this waits until the host
    /// user answers (the host rejects after its timeout, 60 s by default). Must run inside a
    /// tokio runtime.
    pub async fn connect(
        &self,
        target: ConnectTarget,
        auth: AuthMethod,
    ) -> Result<(ViewerSession, ViewerEvents), ViewerError> {
        self.connect_with_progress(target, auth, None).await
    }

    /// [`Viewer::connect`] with a progress hook that runs once the host received an
    /// accept-prompt request (the host user is being asked). Not called for code/password.
    pub async fn connect_with_progress(
        &self,
        target: ConnectTarget,
        auth: AuthMethod,
        on_awaiting_accept: Option<AwaitingAcceptFn>,
    ) -> Result<(ViewerSession, ViewerEvents), ViewerError> {
        if let ConnectTarget::Id(id) | ConnectTarget::IdAt(id, _) = &target {
            if *id == self.endpoint.device_id() {
                return Err(ViewerError::SelfConnection);
            }
        }
        let (addrs, id, via_relay) = match (self.resolve(&target).await, &target) {
            (Ok((addrs, id)), _) => (addrs, id, false),
            // Not on this network: a configured relay may know the device.
            (Err(ViewerError::NotFound(_)), ConnectTarget::Id(id)) if self.relay().is_some() => {
                let addr = self.via_relay(*id).await?;
                (vec![addr], Some(*id), true)
            }
            (Err(e), _) => return Err(e),
        };
        let key = match (id, addrs.first()) {
            (Some(id), _) => HostKey::Id(id),
            (None, Some(a)) => addr_host_key(*a),
            (None, None) => return Err(ViewerError::NotFound(target.to_string())),
        };
        let remembered = lock(&self.known).fingerprint_for(&key);
        let pin = match (remembered, id) {
            (Some(fp), _) => Pin::Fingerprint(fp),
            (None, Some(id)) => Pin::DeviceId(id),
            (None, None) => Pin::Tofu,
        };
        tracing::info!(%target, ?addrs, ?pin, ?auth, "connecting");

        let (allowed, refused): (Vec<SocketAddr>, Vec<SocketAddr>) = addrs
            .iter()
            .partition(|a| via_relay || self.cfg.allow_list.is_allowed(a.ip()));
        for addr in &refused {
            tracing::warn!(%addr, "refusing to dial a non-LAN address");
        }
        if allowed.is_empty() {
            return Err(refused.first().map_or_else(
                || ViewerError::NotFound(target.to_string()),
                |a| ViewerError::NotAllowed(*a),
            ));
        }
        let dialed = match self.endpoint.dial_any(&allowed, pin).await {
            Ok(d) => d,
            Err(failures) => return Err(pick_dial_error(failures, pin, &key, &target)),
        };
        tracing::info!(addr = %dialed.addr(), "host answered; authorizing");
        let session = self
            .endpoint
            .authorize(dialed, auth, on_awaiting_accept)
            .await
            .map_err(|e| map_net_error(e, pin, &key))?;

        let peer = session.peer().clone();
        {
            let mut k = lock(&self.known);
            k.remember(key.clone(), peer.fingerprint);
            k.remember(HostKey::Id(peer.device_id), peer.fingerprint);
            if let Err(e) = k.save() {
                tracing::warn!(%e, "could not save known hosts");
            }
        }
        tracing::info!(host = %peer.device_id, name = %peer.name, addr = %peer.addr, "connected");
        Ok(ViewerSession::start(
            session,
            self.cfg.pixel_format,
            self.cfg.on_frame.clone(),
            &self.cfg.features,
        ))
    }

    /// Asks the configured relay for `id` and returns the relay session address to dial.
    async fn via_relay(&self, id: DeviceId) -> Result<SocketAddr, ViewerError> {
        let Some(cfg) = self.relay() else {
            return Err(ViewerError::NotFound(id.to_string()));
        };
        tracing::info!(%id, relay = %cfg.addr, "not on this network; asking the relay");
        match tlp_desk_net::relay::request(&self.endpoint, &cfg, id, RELAY_REQUEST_LIMIT).await {
            Ok(addr) => Ok(addr),
            Err(NetError::RelayOffline) => Err(ViewerError::NotFoundAnywhere(id.to_string())),
            Err(NetError::Relay(m)) => Err(ViewerError::Relay(m)),
            Err(e) => Err(ViewerError::Net(e)),
        }
    }

    async fn resolve(
        &self,
        target: &ConnectTarget,
    ) -> Result<(Vec<SocketAddr>, Option<DeviceId>), ViewerError> {
        match target {
            ConnectTarget::IdAt(id, addr) => Ok((vec![*addr], Some(*id))),
            ConnectTarget::Addr(addr) => Ok((vec![*addr], None)),
            ConnectTarget::Device(d) => Ok((d.addrs.clone(), Some(d.device_id))),
            ConnectTarget::Id(id) => {
                let d = self
                    .find(|d| d.device_id == *id)
                    .await
                    .ok_or_else(|| ViewerError::NotFound(target.to_string()))?;
                Ok((d.addrs, Some(*id)))
            }
            ConnectTarget::Name(name) => {
                let want = name.trim().to_lowercase();
                let d = self
                    .find(|d| d.name.trim().to_lowercase() == want)
                    .await
                    .ok_or_else(|| ViewerError::NotFound(target.to_string()))?;
                Ok((d.addrs, Some(d.device_id)))
            }
        }
    }

    async fn find(&self, pred: impl Fn(&DiscoveredDevice) -> bool) -> Option<DiscoveredDevice> {
        let disc = lock(&self.discovery).clone()?;
        disc.refresh().await;
        let deadline = Instant::now() + self.cfg.resolve_timeout;
        loop {
            if let Some(d) = disc.devices().into_iter().find(|d| pred(d)) {
                return Some(d);
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

/// The error to show when no address of a device worked. A key mismatch is reported over
/// plain unreachability (it may be an attack, and a stale address now used by another
/// device must not read as "offline"); "it was only ourselves" only when that is all we
/// reached.
fn pick_dial_error(
    failures: Vec<DialFailure>,
    pin: Pin,
    key: &HostKey,
    target: &ConnectTarget,
) -> ViewerError {
    let mut mismatch = None;
    let mut only_self = !failures.is_empty();
    let mut last = None;
    for f in failures {
        tracing::info!(addr = %f.addr, err = %f.error, "connect attempt failed");
        match f.error {
            NetError::SelfConnection => {}
            e @ NetError::PinMismatch { .. } => {
                only_self = false;
                mismatch.get_or_insert(e);
            }
            e => {
                only_self = false;
                last = Some(e);
            }
        }
    }
    if only_self {
        return ViewerError::SelfConnection;
    }
    match (mismatch, last) {
        (Some(m), _) => map_net_error(m, pin, key),
        (None, Some(e)) => map_net_error(e, pin, key),
        (None, None) => ViewerError::NotFound(target.to_string()),
    }
}

fn map_net_error(e: NetError, pin: Pin, key: &HostKey) -> ViewerError {
    match e {
        NetError::PinMismatch {
            actual_id,
            actual_fp,
            ..
        } => match (pin, key) {
            // Dialed by ID and the key belongs to another ID: another device answers at
            // that address (a reused DHCP lease), not a changed key for this one.
            (Pin::Fingerprint(_), HostKey::Id(expected)) if *expected != actual_id => {
                ViewerError::IdentityMismatch {
                    expected: *expected,
                    presented: actual_id,
                }
            }
            (Pin::Fingerprint(previous), _) => ViewerError::HostKeyChanged {
                key: key.clone(),
                previous,
                presented: actual_fp,
                presented_id: actual_id,
            },
            (Pin::DeviceId(expected), _) => ViewerError::IdentityMismatch {
                expected,
                presented: actual_id,
            },
            (Pin::Tofu, _) => ViewerError::Net(NetError::PinMismatch {
                expected: "any key".into(),
                actual_id,
                actual_fp,
            }),
        },
        NetError::Rejected(r) => ViewerError::Rejected(r),
        NetError::SelfConnection => ViewerError::SelfConnection,
        NetError::HostAuthFailed => ViewerError::HostAuthFailed,
        NetError::VersionMismatch { ours, theirs } => ViewerError::VersionMismatch { ours, theirs },
        NetError::Connect(e) => ViewerError::Unreachable(e.to_string()),
        NetError::Connection(e) => ViewerError::Unreachable(e.to_string()),
        NetError::Io(e) => ViewerError::Unreachable(e.to_string()),
        NetError::Timeout => ViewerError::Unreachable("timed out".into()),
        other => ViewerError::Net(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_parsing() {
        let id: DeviceId = "123456789".parse().unwrap();
        assert_eq!(
            "123 456 789".parse::<ConnectTarget>().unwrap(),
            ConnectTarget::Id(id)
        );
        assert_eq!(
            "192.168.1.20".parse::<ConnectTarget>().unwrap(),
            ConnectTarget::Addr("192.168.1.20:47400".parse().unwrap())
        );
        assert_eq!(
            "10.0.0.5:5000".parse::<ConnectTarget>().unwrap(),
            ConnectTarget::Addr("10.0.0.5:5000".parse().unwrap())
        );
        assert_eq!(
            "123-456-789@10.0.0.5".parse::<ConnectTarget>().unwrap(),
            ConnectTarget::IdAt(id, "10.0.0.5:47400".parse().unwrap())
        );
        assert_eq!(
            "مكتب".parse::<ConnectTarget>().unwrap(),
            ConnectTarget::Name("مكتب".into())
        );
    }

    #[test]
    fn addr_keys_omit_default_port() {
        assert_eq!(
            addr_host_key("192.168.1.2:47400".parse().unwrap()),
            HostKey::Addr("192.168.1.2".into())
        );
        assert_eq!(
            addr_host_key("192.168.1.2:5000".parse().unwrap()),
            HostKey::Addr("192.168.1.2:5000".into())
        );
        assert_eq!(
            addr_host_key("[::ffff:10.0.0.1]:47400".parse().unwrap()),
            HostKey::Addr("10.0.0.1".into())
        );
    }
}
