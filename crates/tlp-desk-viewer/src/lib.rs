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
//! * **Performance.** Each session sends `ViewerFeedback` ~4x/s for the host's adaptive
//!   bitrate and `LatencyProbe` every 500 ms to synchronize clocks for the latency overlay
//!   ([`ViewerStats::latency_ms`], method in [`LatencyClock`]); the UI picks presets with
//!   [`ViewerSession::set_quality`] and the "all monitors" view with
//!   [`ViewerSession::select_all_monitors`].
//! * **Threads.** Decoding runs on a dedicated std thread; networking on tokio tasks. Cancel a
//!   pending [`Viewer::connect`] by dropping its future (or aborting its task).
#![forbid(unsafe_code)]

mod frame;
mod perf;
mod session;

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tlp_desk_crypto::{CryptoError, DeviceId, Fingerprint, HostKey, KnownHosts, KNOWN_HOSTS_FILE};
use tlp_desk_net::{AllowList, DiscoveredDevice, Discovery, Endpoint, NetError, Pin, DEFAULT_PORT};
use tlp_desk_proto::RejectReason;

pub use frame::{DecodedFrame, FrameSlot, PixelFormat};
pub use perf::LatencyClock;
pub use session::{EndReason, ViewerEvent, ViewerEvents, ViewerSession, ViewerStats};
pub use tlp_desk_net::{
    AuthMethod, AwaitingAcceptFn, Feature, FeatureCtx, FeatureEvent, FeatureSet, PeerInfo,
};
pub use tlp_desk_proto::{MonitorDesc, QualityPreset, QualitySettings};

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
            .field("features", &self.features)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ViewerError {
    /// The ID/name was not found on the LAN.
    #[error("{0} was not found on the local network")]
    NotFound(String),
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
    /// The host failed SPAKE2 key confirmation (it does not know the code/password).
    #[error("host failed to prove it knows the code/password")]
    HostAuthFailed,
    #[error("protocol version mismatch: ours {ours}, host {theirs}")]
    VersionMismatch { ours: u32, theirs: u32 },
    #[error("known-hosts store: {0}")]
    KnownHosts(#[from] CryptoError),
    #[error(transparent)]
    Net(NetError),
}

impl ViewerError {
    /// True for errors where another address of the same device might work.
    fn is_transport(&self) -> bool {
        matches!(
            self,
            ViewerError::Unreachable(_) | ViewerError::NotAllowed(_)
        )
    }
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
            cfg,
        })
    }

    pub fn set_discovery(&self, discovery: Option<Arc<Discovery>>) {
        *lock(&self.discovery) = discovery;
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
        let (addrs, id) = self.resolve(&target).await?;
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

        let mut last_err = ViewerError::NotFound(target.to_string());
        let mut session = None;
        for addr in addrs {
            if !self.cfg.allow_list.is_allowed(addr.ip()) {
                tracing::warn!(%addr, "refusing to dial a non-LAN address");
                last_err = ViewerError::NotAllowed(addr);
                continue;
            }
            match self
                .endpoint
                .connect_pinned_with_progress(addr, pin, auth.clone(), on_awaiting_accept.clone())
                .await
            {
                Ok(s) => {
                    session = Some(s);
                    break;
                }
                Err(e) => {
                    let err = map_net_error(e, pin, &key);
                    tracing::info!(%addr, %err, "connect attempt failed");
                    if !err.is_transport() {
                        return Err(err);
                    }
                    last_err = err;
                }
            }
        }
        let Some(session) = session else {
            return Err(last_err);
        };

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

fn map_net_error(e: NetError, pin: Pin, key: &HostKey) -> ViewerError {
    match e {
        NetError::PinMismatch {
            actual_id,
            actual_fp,
            ..
        } => match pin {
            Pin::Fingerprint(previous) => ViewerError::HostKeyChanged {
                key: key.clone(),
                previous,
                presented: actual_fp,
                presented_id: actual_id,
            },
            Pin::DeviceId(expected) => ViewerError::IdentityMismatch {
                expected,
                presented: actual_id,
            },
            Pin::Tofu => ViewerError::Net(NetError::PinMismatch {
                expected: "any key".into(),
                actual_id,
                actual_fp,
            }),
        },
        NetError::Rejected(r) => ViewerError::Rejected(r),
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
