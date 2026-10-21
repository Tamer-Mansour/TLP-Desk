//! LAN discovery: mDNS (`_tlpdesk._udp.local.`) plus a UDP broadcast fallback on 47401,
//! merged into one device list keyed by device ID. Nothing here leaves the LAN: mDNS is
//! link-local multicast and the fallback uses subnet broadcasts.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tlp_desk_crypto::DeviceId;
use tlp_desk_proto::PROTOCOL_VERSION;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::allow::{normalize, AllowList};
use crate::{NetError, BROADCAST_PORT};

/// mDNS service type.
pub const MDNS_SERVICE_TYPE: &str = "_tlpdesk._udp.local.";
/// Magic prefix of broadcast packets.
pub const BROADCAST_MAGIC: &[u8; 4] = b"TLPD";
/// Broadcast source addresses remembered per device.
const MAX_BCAST_ADDRS: usize = 8;
/// Device names longer than this are truncated in announcements.
const MAX_NAME_CHARS: usize = 64;
/// App versions longer than this are ignored (a semver like `1.0.0-rc.3` is ~10 bytes).
pub const MAX_APP_VERSION_LEN: usize = 32;
/// The version this build announces on the LAN (`ver` TXT key / broadcast trailer). Every
/// crate inherits the workspace version, so this is the app version.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Accepts a peer-announced app version only if it is short and plain ASCII
/// (`[0-9A-Za-z.+-]`); anything else is dropped rather than shown.
pub fn sanitize_app_version(v: &str) -> Option<String> {
    let ok = !v.is_empty()
        && v.len() <= MAX_APP_VERSION_LEN
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'));
    ok.then(|| v.to_owned())
}

/// What this device announces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advertisement {
    pub device_id: DeviceId,
    pub name: String,
    /// QUIC port (normally 47400).
    pub port: u16,
}

/// A device seen on the LAN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredDevice {
    pub device_id: DeviceId,
    pub name: String,
    /// Addresses to dial (QUIC port included), best first: addresses a broadcast packet
    /// just came from (newest first), then addresses on a subnet this machine is on, then
    /// other IPv4, then IPv6. Never one of this machine's own addresses, never an IPv6
    /// link-local address (it cannot be dialed without its interface scope). See
    /// [`rank_addrs`].
    pub addrs: Vec<SocketAddr>,
    pub protocol_version: u32,
    /// App version the peer announces (`1.0.0-rc.3`), `None` for builds older than the
    /// field or a malformed value. Display/compare only; never trusted for security.
    pub app_version: Option<String>,
    pub via_mdns: bool,
    pub via_broadcast: bool,
    pub last_seen: Instant,
}

/// Change notifications from [`Discovery::subscribe`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryEvent {
    /// New device or changed details.
    Updated(DiscoveredDevice),
    /// Device went away (mDNS goodbye or broadcast announcements stopped).
    Lost(DeviceId),
}

/// Discovery settings.
#[derive(Debug, Clone)]
pub struct DiscoveryConfig {
    /// Announce ourselves (host). `None` = browse only.
    pub advertise: Option<Advertisement>,
    pub enable_mdns: bool,
    pub enable_broadcast: bool,
    /// Local port for the broadcast fallback (default 47401).
    pub broadcast_port: u16,
    /// Where announcements/queries go. `None` = every IPv4 interface's subnet broadcast plus
    /// `255.255.255.255`, on `broadcast_port`. Tests use explicit unicast targets.
    pub broadcast_targets: Option<Vec<SocketAddr>>,
    pub announce_interval: Duration,
    /// Broadcast-only devices are dropped after this long without an announcement.
    pub expiry: Duration,
    /// Devices outside this list are ignored.
    pub allow_list: AllowList,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            advertise: None,
            enable_mdns: true,
            enable_broadcast: true,
            broadcast_port: BROADCAST_PORT,
            broadcast_targets: None,
            announce_interval: Duration::from_secs(5),
            expiry: Duration::from_secs(16),
            allow_list: AllowList::default(),
        }
    }
}

/// Broadcast fallback packet, at most 512 bytes:
/// `b"TLPD" || postcard(BroadcastPacket) [|| postcard(app_version: String)]`.
///
/// postcard is not self-describing, so the app version travels as an optional trailer after
/// the original struct instead of as a new struct field: older builds decode the struct and
/// ignore the trailing bytes, newer builds read the trailer when it is present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BroadcastPacket {
    pub kind: PacketKind,
    pub protocol_version: u32,
    /// Sender's device ID (0 for an anonymous browse-only query).
    pub device_id: u32,
    pub name: String,
    /// Sender's QUIC port (0 in queries from browse-only peers).
    pub port: u16,
    /// Sender's app version (the trailer; see the type docs). `None` from older builds.
    #[serde(skip)]
    pub app_version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PacketKind {
    /// "I am here" (periodic, and as a unicast reply to a query).
    Announce,
    /// "Who is there?" Hosts reply with a unicast `Announce`.
    Query,
}

impl BroadcastPacket {
    pub const MAX_LEN: usize = 512;

    pub fn encode(&self) -> Vec<u8> {
        let mut p = self.clone();
        p.name = p.name.chars().take(MAX_NAME_CHARS).collect();
        let mut out = BROADCAST_MAGIC.to_vec();
        out.extend(postcard::to_stdvec(&p).unwrap_or_default());
        if let Some(v) = p.app_version.as_deref().and_then(sanitize_app_version) {
            out.extend(postcard::to_stdvec(&v).unwrap_or_default());
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > Self::MAX_LEN {
            return None;
        }
        let body = bytes.strip_prefix(BROADCAST_MAGIC.as_slice())?;
        let (mut p, rest): (Self, &[u8]) = postcard::take_from_bytes(body).ok()?;
        p.app_version = postcard::from_bytes::<String>(rest)
            .ok()
            .and_then(|v| sanitize_app_version(&v));
        Some(p)
    }
}

#[derive(Debug, Clone)]
struct Entry {
    device: DiscoveredDevice,
    mdns_fullname: Option<String>,
    bcast_seen: Option<Instant>,
    /// The addresses of the latest mDNS resolution (replaced, never accumulated).
    mdns_addrs: Vec<SocketAddr>,
    /// Source addresses of broadcast announcements and when each was last heard.
    bcast_addrs: Vec<(SocketAddr, Instant)>,
}

impl Entry {
    fn rerank(&mut self, local: &[LocalIf]) {
        self.device.addrs = rank_addrs(&self.mdns_addrs, &self.bcast_addrs, local);
    }
}

type Table = Arc<Mutex<HashMap<DeviceId, Entry>>>;

/// One address of this machine (from the OS interface list).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LocalIf {
    pub ip: IpAddr,
    /// IPv4 netmask; `None` for IPv6 or when unknown.
    pub netmask: Option<Ipv4Addr>,
    pub loopback: bool,
}

/// This machine's interface addresses, sorted (comparable between calls).
pub fn local_interfaces() -> Vec<LocalIf> {
    let mut v: Vec<LocalIf> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|i| LocalIf {
            ip: normalize(i.ip()),
            netmask: match &i.addr {
                if_addrs::IfAddr::V4(a) => Some(a.netmask),
                if_addrs::IfAddr::V6(_) => None,
            },
            loopback: i.is_loopback(),
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

fn is_v6_link_local(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) == 0xfe80)
}

fn same_subnet(ip: Ipv4Addr, local: &LocalIf) -> bool {
    match (local.ip, local.netmask) {
        (IpAddr::V4(mine), Some(mask)) if !local.loopback && mask != Ipv4Addr::UNSPECIFIED => {
            let m = u32::from(mask);
            u32::from(mine) & m == u32::from(ip) & m
        }
        _ => false,
    }
}

/// True if `ip` is one of this machine's own non-loopback addresses.
fn is_own_address(ip: IpAddr, local: &[LocalIf]) -> bool {
    let ip = normalize(ip);
    local.iter().any(|l| !l.loopback && l.ip == ip)
}

/// Orders a device's addresses for dialing, best first, and drops the ones that cannot be
/// right:
///
/// * an address that is also one of **this** machine's addresses is dropped: dialing it
///   reaches ourselves. This happens with virtual adapters that use the same address on
///   every PC (VirtualBox host-only `192.168.56.1`, Docker `172.17.0.1`, Hyper-V/WSL
///   switches) and with a stale DHCP lease that now belongs to us;
/// * IPv6 link-local (`fe80::/10`) is dropped: mDNS does not tell us its interface scope,
///   so it can never be dialed;
/// * the source address of a broadcast announcement comes first (newest first): a packet
///   just arrived from it, so it is routable from here right now;
/// * then IPv4 addresses on a subnet one of our interfaces is on, then other IPv4, then
///   IPv6; ties keep a stable numeric order.
pub fn rank_addrs(
    mdns: &[SocketAddr],
    bcast: &[(SocketAddr, Instant)],
    local: &[LocalIf],
) -> Vec<SocketAddr> {
    let usable = |a: &SocketAddr| !is_own_address(a.ip(), local) && !is_v6_link_local(a.ip());
    let mut heard: Vec<(SocketAddr, Instant)> =
        bcast.iter().copied().filter(|(a, _)| usable(a)).collect();
    heard.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut out: Vec<SocketAddr> = heard.into_iter().map(|(a, _)| a).collect();
    let mut rest: Vec<SocketAddr> = mdns
        .iter()
        .copied()
        .filter(|a| usable(a) && !out.contains(a))
        .collect();
    rest.sort_by_key(|a| {
        let class = match normalize(a.ip()) {
            IpAddr::V4(v4) if local.iter().any(|l| same_subnet(v4, l)) => 0u8,
            IpAddr::V4(_) => 1,
            IpAddr::V6(_) => 2,
        };
        (class, *a)
    });
    rest.dedup();
    out.extend(rest);
    out
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// How often the network watcher compares the interface list.
const NETWATCH_INTERVAL: Duration = Duration::from_secs(2);

/// Local snapshot shared by the discovery tasks; refreshed by the network watcher.
type LocalNet = Arc<Mutex<Vec<LocalIf>>>;

/// Another machine announced *our* device ID (a cloned disk/VM or a copied key file).
type Conflict = Arc<Mutex<Option<(IpAddr, Instant)>>>;

/// Running discovery (mDNS + broadcast). Stops when dropped.
pub struct Discovery {
    table: Table,
    conflict: Conflict,
    events: broadcast::Sender<DiscoveryEvent>,
    mdns: Option<mdns_sd::ServiceDaemon>,
    bcast: Option<Arc<BroadcastCtx>>,
    tasks: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for Discovery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Discovery")
            .field("mdns", &self.mdns.is_some())
            .field("broadcast", &self.bcast.is_some())
            .finish_non_exhaustive()
    }
}

struct BroadcastCtx {
    socket: tokio::net::UdpSocket,
    targets: Option<Vec<SocketAddr>>,
    port: u16,
    me: Option<Advertisement>,
}

impl BroadcastCtx {
    fn packet(&self, kind: PacketKind) -> BroadcastPacket {
        BroadcastPacket {
            kind,
            protocol_version: PROTOCOL_VERSION,
            device_id: self.me.as_ref().map_or(0, |m| m.device_id.as_u32()),
            name: self.me.as_ref().map(|m| m.name.clone()).unwrap_or_default(),
            port: self.me.as_ref().map_or(0, |m| m.port),
            app_version: Some(APP_VERSION.to_owned()),
        }
    }

    async fn send_all(&self, kind: PacketKind) {
        let bytes = self.packet(kind).encode();
        let targets = match &self.targets {
            Some(t) => t.clone(),
            None => broadcast_targets(self.port),
        };
        for t in targets {
            if let Err(e) = self.socket.send_to(&bytes, t).await {
                tracing::trace!(target = %t, %e, "broadcast send failed");
            }
        }
    }
}

/// Subnet broadcast address of every up, non-loopback IPv4 interface, plus the limited
/// broadcast address.
pub fn broadcast_targets(port: u16) -> Vec<SocketAddr> {
    let mut v: Vec<SocketAddr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(a) => a.broadcast.map(|b| SocketAddr::new(IpAddr::V4(b), port)),
            _ => None,
        })
        .collect();
    v.push(SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), port));
    v.sort();
    v.dedup();
    v
}

fn bind_broadcast(port: u16) -> std::io::Result<tokio::net::UdpSocket> {
    let s = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    s.set_reuse_address(true)?;
    s.set_broadcast(true)?;
    s.set_nonblocking(true)?;
    s.bind(&SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port).into())?;
    tokio::net::UdpSocket::from_std(s.into())
}

fn upsert(
    table: &Table,
    local: &LocalNet,
    events: &broadcast::Sender<DiscoveryEvent>,
    id: DeviceId,
    update: impl FnOnce(&mut Entry),
) {
    let local = lock(local).clone();
    let snapshot = {
        let mut t = table.lock().unwrap_or_else(|p| p.into_inner());
        let e = t.entry(id).or_insert_with(|| Entry {
            device: DiscoveredDevice {
                device_id: id,
                name: String::new(),
                addrs: Vec::new(),
                protocol_version: 0,
                app_version: None,
                via_mdns: false,
                via_broadcast: false,
                last_seen: Instant::now(),
            },
            mdns_fullname: None,
            bcast_seen: None,
            mdns_addrs: Vec::new(),
            bcast_addrs: Vec::new(),
        });
        let before = e.device.clone();
        update(e);
        e.device.last_seen = Instant::now();
        e.rerank(&local);
        let changed = DiscoveredDevice {
            last_seen: before.last_seen,
            ..e.device.clone()
        } != before;
        (changed && !e.device.addrs.is_empty()).then(|| e.device.clone())
    };
    if let Some(d) = snapshot {
        tracing::debug!(device_id = %d.device_id, name = %d.name, addrs = ?d.addrs, "device discovered/updated");
        let _ = events.send(DiscoveryEvent::Updated(d));
    }
}

fn remove_if_gone(table: &Table, events: &broadcast::Sender<DiscoveryEvent>, id: DeviceId) {
    let gone = {
        let mut t = table.lock().unwrap_or_else(|p| p.into_inner());
        match t.get(&id) {
            Some(e) if !e.device.via_mdns && !e.device.via_broadcast => {
                t.remove(&id);
                true
            }
            _ => false,
        }
    };
    if gone {
        tracing::debug!(device_id = %id, "device lost");
        let _ = events.send(DiscoveryEvent::Lost(id));
    }
}

impl Discovery {
    /// Starts advertising (if configured) and browsing. Must run inside a tokio runtime.
    pub async fn start(cfg: DiscoveryConfig) -> Result<Self, NetError> {
        let table: Table = Arc::default();
        let local: LocalNet = Arc::new(Mutex::new(local_interfaces()));
        let conflict: Conflict = Arc::default();
        let (events, _) = broadcast::channel(64);
        let me = cfg.advertise.as_ref().map(|a| a.device_id);
        let allow = Arc::new(cfg.allow_list.clone());
        let mut tasks = Vec::new();

        let mut mdns = None;
        if cfg.enable_mdns {
            match start_mdns(&cfg, me, &table, &local, &conflict, &events, &allow) {
                Ok((daemon, task)) => {
                    mdns = Some(daemon);
                    tasks.push(task);
                }
                Err(e) => tracing::warn!(%e, "mDNS unavailable; relying on broadcast fallback"),
            }
        }

        let mut bcast = None;
        if cfg.enable_broadcast {
            match bind_broadcast(cfg.broadcast_port) {
                Ok(socket) => {
                    let ctx = Arc::new(BroadcastCtx {
                        socket,
                        targets: cfg.broadcast_targets.clone(),
                        port: cfg.broadcast_port,
                        me: cfg.advertise.clone(),
                    });
                    tasks.push(tokio::spawn(bcast_recv_loop(
                        ctx.clone(),
                        table.clone(),
                        local.clone(),
                        conflict.clone(),
                        events.clone(),
                        allow.clone(),
                    )));
                    tasks.push(tokio::spawn(bcast_announce_loop(
                        ctx.clone(),
                        cfg.announce_interval,
                    )));
                    bcast = Some(ctx);
                }
                Err(e) => {
                    tracing::warn!(%e, port = cfg.broadcast_port, "broadcast discovery unavailable")
                }
            }
        }
        if mdns.is_none() && bcast.is_none() {
            return Err(NetError::Discovery(
                "neither mDNS nor broadcast could start".into(),
            ));
        }
        tasks.push(tokio::spawn(expiry_loop(
            table.clone(),
            local.clone(),
            events.clone(),
            cfg.expiry,
        )));
        tasks.push(tokio::spawn(netwatch_loop(
            table.clone(),
            local,
            events.clone(),
            bcast.clone(),
        )));
        Ok(Self {
            table,
            conflict,
            events,
            mdns,
            bcast,
            tasks,
        })
    }

    /// Snapshot of currently known devices that have at least one dialable address, sorted
    /// by name then ID.
    pub fn devices(&self) -> Vec<DiscoveredDevice> {
        let mut v: Vec<_> = lock(&self.table)
            .values()
            .filter(|e| !e.device.addrs.is_empty())
            .map(|e| e.device.clone())
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name).then(a.device_id.cmp(&b.device_id)));
        v
    }

    /// Live change notifications.
    pub fn subscribe(&self) -> broadcast::Receiver<DiscoveryEvent> {
        self.events.subscribe()
    }

    /// The address of another machine that recently announced **our own** device ID, if
    /// any (within the last minute). Two installs share one identity only when a disk, VM
    /// or key file was copied; the user must be told, because peers cannot tell them apart.
    pub fn identity_conflict(&self) -> Option<IpAddr> {
        let c = *lock(&self.conflict);
        c.filter(|(_, at)| at.elapsed() < Duration::from_secs(60))
            .map(|(ip, _)| ip)
    }

    /// Sends a broadcast query now (e.g. when the user presses Refresh).
    pub async fn refresh(&self) {
        if let Some(b) = &self.bcast {
            b.send_all(PacketKind::Query).await;
        }
    }

    /// Stops advertising and browsing (also happens on drop).
    pub fn shutdown(&mut self) {
        for t in self.tasks.drain(..) {
            t.abort();
        }
        if let Some(d) = self.mdns.take() {
            let _ = d.shutdown();
        }
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn start_mdns(
    cfg: &DiscoveryConfig,
    me: Option<DeviceId>,
    table: &Table,
    local: &LocalNet,
    conflict: &Conflict,
    events: &broadcast::Sender<DiscoveryEvent>,
    allow: &Arc<AllowList>,
) -> Result<(mdns_sd::ServiceDaemon, JoinHandle<()>), mdns_sd::Error> {
    let daemon = mdns_sd::ServiceDaemon::new()?;
    if let Some(ad) = &cfg.advertise {
        let id = ad.device_id.as_u32().to_string();
        let name: String = ad.name.chars().take(MAX_NAME_CHARS).collect();
        let props = [
            ("id", id.clone()),
            ("name", name),
            ("port", ad.port.to_string()),
            ("pv", PROTOCOL_VERSION.to_string()),
            ("ver", APP_VERSION.to_owned()),
        ];
        let info = mdns_sd::ServiceInfo::new(
            MDNS_SERVICE_TYPE,
            &id,
            &format!("tlpdesk-{id}.local."),
            "",
            ad.port,
            &props[..],
        )?
        .enable_addr_auto();
        daemon.register(info)?;
    }
    let rx = daemon.browse(MDNS_SERVICE_TYPE)?;
    let table = table.clone();
    let local = local.clone();
    let conflict = conflict.clone();
    let events = events.clone();
    let allow = allow.clone();
    let task = tokio::spawn(async move {
        while let Ok(ev) = rx.recv_async().await {
            match ev {
                mdns_sd::ServiceEvent::ServiceResolved(info) => {
                    let Some(id) = info
                        .get_property_val_str("id")
                        .and_then(|s| s.parse::<u32>().ok())
                        .and_then(DeviceId::new)
                    else {
                        continue;
                    };
                    if Some(id) == me {
                        let ips: Vec<IpAddr> = info
                            .get_addresses()
                            .iter()
                            .map(|ip| normalize(ip.to_ip_addr()))
                            .collect();
                        note_own_id(&conflict, &ips);
                        continue;
                    }
                    let port = info
                        .get_property_val_str("port")
                        .and_then(|p| p.parse().ok())
                        .unwrap_or_else(|| info.get_port());
                    let pv = info
                        .get_property_val_str("pv")
                        .and_then(|p| p.parse().ok())
                        .unwrap_or(0);
                    let name = info
                        .get_property_val_str("name")
                        .unwrap_or_default()
                        .to_owned();
                    let app_version = info
                        .get_property_val_str("ver")
                        .and_then(sanitize_app_version);
                    let addrs: Vec<SocketAddr> = info
                        .get_addresses()
                        .iter()
                        .map(|ip| normalize(ip.to_ip_addr()))
                        .filter(|ip| allow.is_allowed(*ip))
                        .map(|ip| SocketAddr::new(ip, port))
                        .collect();
                    if addrs.is_empty() {
                        continue;
                    }
                    let fullname = info.get_fullname().to_owned();
                    upsert(&table, &local, &events, id, |e| {
                        e.device.name = name;
                        e.device.protocol_version = pv;
                        if app_version.is_some() {
                            e.device.app_version = app_version;
                        }
                        e.device.via_mdns = true;
                        e.mdns_fullname = Some(fullname);
                        // The resolution carries the device's current address set: replace,
                        // so an address it no longer has (old DHCP lease) is not dialed.
                        e.mdns_addrs = addrs;
                    });
                }
                mdns_sd::ServiceEvent::ServiceRemoved(_, fullname) => {
                    let id = {
                        let mut t = lock(&table);
                        let found = t
                            .iter_mut()
                            .find(|(_, e)| e.mdns_fullname.as_deref() == Some(fullname.as_str()));
                        found.map(|(id, e)| {
                            e.device.via_mdns = false;
                            e.mdns_fullname = None;
                            e.mdns_addrs.clear();
                            *id
                        })
                    };
                    if let Some(id) = id {
                        refresh_entry(&table, &local, &events, id);
                        remove_if_gone(&table, &events, id);
                    }
                }
                _ => {}
            }
        }
    });
    Ok((daemon, task))
}

async fn bcast_announce_loop(ctx: Arc<BroadcastCtx>, interval: Duration) {
    ctx.send_all(PacketKind::Query).await;
    let mut tick = tokio::time::interval(interval);
    loop {
        tick.tick().await;
        if ctx.me.is_some() {
            ctx.send_all(PacketKind::Announce).await;
        }
    }
}

async fn bcast_recv_loop(
    ctx: Arc<BroadcastCtx>,
    table: Table,
    local: LocalNet,
    conflict: Conflict,
    events: broadcast::Sender<DiscoveryEvent>,
    allow: Arc<AllowList>,
) {
    let me = ctx.me.as_ref().map(|m| m.device_id.as_u32());
    let mut buf = [0u8; BroadcastPacket::MAX_LEN + 1];
    loop {
        let (n, src) = match ctx.socket.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => {
                // Windows reports ICMP port-unreachable as a recv error on UDP sockets.
                tracing::trace!(%e, "broadcast recv error");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let ip = normalize(src.ip());
        if !allow.is_allowed(ip) {
            continue;
        }
        let Some(pkt) = buf.get(..n).and_then(BroadcastPacket::decode) else {
            continue;
        };
        if pkt.device_id != 0 && Some(pkt.device_id) == me {
            // Normally our own announcement echoed back; from another address it is a
            // second machine with our identity.
            if pkt.kind == PacketKind::Announce {
                note_own_id(&conflict, &[ip]);
            }
            continue;
        }
        match pkt.kind {
            PacketKind::Query => {
                if ctx.me.is_some() {
                    let reply = ctx.packet(PacketKind::Announce).encode();
                    let _ = ctx.socket.send_to(&reply, src).await;
                }
            }
            PacketKind::Announce => {
                let Some(id) = DeviceId::new(pkt.device_id) else {
                    continue;
                };
                if pkt.port == 0 {
                    continue;
                }
                let addr = SocketAddr::new(ip, pkt.port);
                upsert(&table, &local, &events, id, |e| {
                    e.device.name = pkt.name;
                    e.device.protocol_version = pkt.protocol_version;
                    if pkt.app_version.is_some() {
                        e.device.app_version = pkt.app_version;
                    }
                    e.device.via_broadcast = true;
                    let now = Instant::now();
                    e.bcast_seen = Some(now);
                    match e.bcast_addrs.iter_mut().find(|(a, _)| *a == addr) {
                        Some(slot) => slot.1 = now,
                        None => e.bcast_addrs.push((addr, now)),
                    }
                    // A device has a handful of addresses; a flood of spoofed sources must
                    // not grow this without bound.
                    if e.bcast_addrs.len() > MAX_BCAST_ADDRS {
                        e.bcast_addrs.sort_by_key(|b| std::cmp::Reverse(b.1));
                        e.bcast_addrs.truncate(MAX_BCAST_ADDRS);
                    }
                });
            }
        }
    }
}

async fn expiry_loop(
    table: Table,
    local: LocalNet,
    events: broadcast::Sender<DiscoveryEvent>,
    expiry: Duration,
) {
    let mut tick = tokio::time::interval((expiry / 4).max(Duration::from_millis(100)));
    loop {
        tick.tick().await;
        let local_now = lock(&local).clone();
        let (stale, changed): (Vec<DeviceId>, Vec<DiscoveredDevice>) = {
            let mut t = lock(&table);
            let now = Instant::now();
            let mut stale = Vec::new();
            let mut changed = Vec::new();
            for e in t.values_mut() {
                let before = e.device.addrs.clone();
                // Addresses a device stopped announcing from (it moved, or that adapter is
                // gone) expire one by one, not only when the whole device goes silent.
                e.bcast_addrs
                    .retain(|(_, seen)| now.duration_since(*seen) <= expiry);
                if e.bcast_seen.is_some_and(|s| now.duration_since(s) > expiry) {
                    e.device.via_broadcast = false;
                    e.bcast_seen = None;
                    stale.push(e.device.device_id);
                }
                e.rerank(&local_now);
                if e.device.addrs != before && !e.device.addrs.is_empty() {
                    changed.push(e.device.clone());
                }
            }
            (stale, changed)
        };
        for d in changed {
            let _ = events.send(DiscoveryEvent::Updated(d));
        }
        for id in stale {
            remove_if_gone(&table, &events, id);
        }
    }
}

/// Re-ranks one entry after its sources changed and reports the new address list.
fn refresh_entry(
    table: &Table,
    local: &LocalNet,
    events: &broadcast::Sender<DiscoveryEvent>,
    id: DeviceId,
) {
    let local = lock(local).clone();
    let updated = {
        let mut t = lock(table);
        t.get_mut(&id).and_then(|e| {
            let before = e.device.addrs.clone();
            e.rerank(&local);
            (e.device.addrs != before && !e.device.addrs.is_empty()).then(|| e.device.clone())
        })
    };
    if let Some(d) = updated {
        let _ = events.send(DiscoveryEvent::Updated(d));
    }
}

/// Records that `ips` announced our own device ID, unless every one of them is ours.
fn note_own_id(conflict: &Conflict, ips: &[IpAddr]) {
    // Fresh interface list: our own address may have changed since the last watcher tick.
    let local = local_interfaces();
    let foreign = ips
        .iter()
        .copied()
        .find(|ip| !ip.is_loopback() && !is_v6_link_local(*ip) && !is_own_address(*ip, &local));
    if let Some(ip) = foreign {
        let mut c = lock(conflict);
        if c.is_none() {
            tracing::warn!(peer = %ip, "another device announces this device's ID (copied identity?)");
        }
        *c = Some((ip, Instant::now()));
    }
}

/// Watches the interface list. When it changes (Wi-Fi reconnect, new DHCP lease, VPN up or
/// down, sleep/wake), addresses learned over the old network are dropped, every device is
/// re-ranked against the new subnets, and we ask the LAN to announce itself again (and
/// announce ourselves at our new address) instead of waiting for the next periodic
/// announcement.
async fn netwatch_loop(
    table: Table,
    local: LocalNet,
    events: broadcast::Sender<DiscoveryEvent>,
    bcast: Option<Arc<BroadcastCtx>>,
) {
    let mut tick = tokio::time::interval(NETWATCH_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let now_ifs = tokio::task::spawn_blocking(local_interfaces)
            .await
            .unwrap_or_default();
        let previous = {
            let mut l = lock(&local);
            if *l == now_ifs {
                continue;
            }
            std::mem::replace(&mut *l, now_ifs.clone())
        };
        tracing::info!(
            before = previous.len(),
            after = now_ifs.len(),
            "network interfaces changed; refreshing discovery"
        );
        let changed: Vec<DiscoveredDevice> = {
            let mut t = lock(&table);
            t.values_mut()
                .filter_map(|e| {
                    let before = e.device.addrs.clone();
                    // Routes learned on the old network may be gone; announcements on the
                    // new one re-add them within a round trip of the query below.
                    e.bcast_addrs.clear();
                    e.rerank(&now_ifs);
                    (e.device.addrs != before && !e.device.addrs.is_empty())
                        .then(|| e.device.clone())
                })
                .collect()
        };
        for d in changed {
            let _ = events.send(DiscoveryEvent::Updated(d));
        }
        if let Some(b) = &bcast {
            b.send_all(PacketKind::Query).await;
            if b.me.is_some() {
                b.send_all(PacketKind::Announce).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcast_packet_roundtrip_and_size() {
        let p = BroadcastPacket {
            kind: PacketKind::Announce,
            protocol_version: PROTOCOL_VERSION,
            device_id: 123_456_789,
            name: "مكتب".repeat(100),
            port: 47400,
            app_version: Some(APP_VERSION.into()),
        };
        let bytes = p.encode();
        assert!(bytes.len() <= BroadcastPacket::MAX_LEN, "{}", bytes.len());
        let back = BroadcastPacket::decode(&bytes).unwrap();
        assert_eq!(back.device_id, p.device_id);
        assert_eq!(back.name.chars().count(), MAX_NAME_CHARS);
        assert!(BroadcastPacket::decode(b"XXXXjunk").is_none());
        assert!(BroadcastPacket::decode(b"TLPD\xff\xff").is_none());
    }

    /// Old peers (no trailer) still decode, and the new trailer survives a round trip next
    /// to an Arabic name.
    #[test]
    fn broadcast_app_version_trailer_is_backward_compatible() {
        #[derive(Serialize)]
        struct OldPacket {
            kind: PacketKind,
            protocol_version: u32,
            device_id: u32,
            name: String,
            port: u16,
        }
        let old = OldPacket {
            kind: PacketKind::Announce,
            protocol_version: 4,
            device_id: 123_456_789,
            name: "حاسوب المحاسبة".into(),
            port: 47400,
        };
        let mut bytes = BROADCAST_MAGIC.to_vec();
        bytes.extend(postcard::to_stdvec(&old).unwrap());
        let back = BroadcastPacket::decode(&bytes).unwrap();
        assert_eq!(back.name, "حاسوب المحاسبة");
        assert_eq!(back.app_version, None);

        let new = BroadcastPacket {
            kind: PacketKind::Announce,
            protocol_version: 4,
            device_id: 123_456_789,
            name: "حاسوب المحاسبة".into(),
            port: 47400,
            app_version: Some("1.0.0-rc.4".into()),
        };
        let bytes = new.encode();
        let back = BroadcastPacket::decode(&bytes).unwrap();
        assert_eq!(back, new);
        // An old build decodes the same bytes: postcard ignores the trailer.
        #[derive(Deserialize)]
        struct OldDecode {
            _kind: PacketKind,
            _pv: u32,
            device_id: u32,
            name: String,
            port: u16,
        }
        let old_view: OldDecode = postcard::from_bytes(&bytes[4..]).unwrap();
        assert_eq!(old_view.device_id, 123_456_789);
        assert_eq!(old_view.name, "حاسوب المحاسبة");
        assert_eq!(old_view.port, 47400);
    }

    #[test]
    fn hostile_app_versions_are_dropped() {
        assert_eq!(
            sanitize_app_version("1.0.0-rc.3").as_deref(),
            Some("1.0.0-rc.3")
        );
        assert_eq!(sanitize_app_version(""), None);
        assert_eq!(sanitize_app_version(&"9".repeat(33)), None);
        assert_eq!(sanitize_app_version("1.0\u{202e}"), None);
        assert_eq!(sanitize_app_version("١.٠"), None);
        let p = BroadcastPacket {
            kind: PacketKind::Announce,
            protocol_version: 4,
            device_id: 1,
            name: "x".into(),
            port: 1,
            app_version: None,
        };
        let mut bytes = p.encode();
        bytes.extend(postcard::to_stdvec("bad version!").unwrap());
        let back = BroadcastPacket::decode(&bytes).unwrap();
        assert_eq!(back.app_version, None);
    }

    fn sa(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn v4if(ip: &str, mask: &str) -> LocalIf {
        LocalIf {
            ip: ip.parse().unwrap(),
            netmask: Some(mask.parse().unwrap()),
            loopback: false,
        }
    }

    /// A PC with Wi-Fi on 192.168.1.0/24 plus the virtual adapters that exist on *every*
    /// machine with VirtualBox / Docker installed.
    fn my_pc() -> Vec<LocalIf> {
        let mut v = vec![
            v4if("192.168.1.20", "255.255.255.0"),
            v4if("192.168.56.1", "255.255.255.0"),
            v4if("172.17.0.1", "255.255.0.0"),
            LocalIf {
                ip: "127.0.0.1".parse().unwrap(),
                netmask: Some("255.0.0.0".parse().unwrap()),
                loopback: true,
            },
        ];
        v.sort();
        v
    }

    /// Regression (BUG 1, "it says I am connecting to my own device"): the other PC
    /// announces its virtual-adapter addresses too, which are byte-for-byte the same as
    /// ours. They sorted first and dialing them reached this PC. They must never be dialed.
    #[test]
    fn addresses_that_are_ours_are_never_dialed() {
        let peer = [
            sa("172.17.0.1:47400"),
            sa("192.168.1.30:47400"),
            sa("192.168.56.1:47400"),
        ];
        let ranked = rank_addrs(&peer, &[], &my_pc());
        assert_eq!(ranked, vec![sa("192.168.1.30:47400")]);
    }

    /// The peer's address on our own subnet comes before addresses on subnets we only
    /// reach through a router or not at all, whatever their numeric order.
    #[test]
    fn same_subnet_addresses_come_first() {
        let peer = [
            sa("10.8.0.6:47400"),
            sa("172.20.5.9:47400"),
            sa("192.168.1.30:47400"),
            sa("[fd00::30]:47400"),
        ];
        let ranked = rank_addrs(&peer, &[], &my_pc());
        assert_eq!(ranked.first(), Some(&sa("192.168.1.30:47400")));
        assert_eq!(ranked.last(), Some(&sa("[fd00::30]:47400")));
        assert_eq!(ranked.len(), 4);
    }

    #[test]
    fn scopeless_ipv6_link_local_is_dropped() {
        let peer = [sa("[fe80::1234]:47400"), sa("192.168.1.30:47400")];
        assert_eq!(
            rank_addrs(&peer, &[], &my_pc()),
            vec![sa("192.168.1.30:47400")]
        );
    }

    /// An address a broadcast packet just came from beats everything mDNS remembers, and
    /// the newest such address wins (the device moved to a new DHCP lease).
    #[test]
    fn freshly_heard_addresses_come_first_newest_first() {
        let old = Instant::now();
        let new = old + Duration::from_secs(5);
        let mdns = [sa("192.168.1.30:47400"), sa("192.168.1.31:47400")];
        let heard = [
            (sa("192.168.1.30:47400"), old),
            (sa("192.168.1.31:47400"), new),
        ];
        assert_eq!(
            rank_addrs(&mdns, &heard, &my_pc()),
            vec![sa("192.168.1.31:47400"), sa("192.168.1.30:47400")]
        );
    }

    /// Loopback stays dialable: it is how two installs on one machine (tests, a dev box)
    /// reach each other, and the endpoint itself refuses its own key.
    #[test]
    fn loopback_is_kept() {
        assert_eq!(
            rank_addrs(&[sa("127.0.0.1:47400")], &[], &my_pc()),
            vec![sa("127.0.0.1:47400")]
        );
    }

    #[test]
    fn own_id_from_our_own_addresses_is_not_a_conflict() {
        let c: Conflict = Arc::default();
        let mine: Vec<IpAddr> = local_interfaces().iter().map(|l| l.ip).collect();
        note_own_id(&c, &mine);
        note_own_id(
            &c,
            &["127.0.0.1".parse().unwrap(), "fe80::1".parse().unwrap()],
        );
        assert!(lock(&c).is_none());
        // TEST-NET-3: never assigned to a real interface.
        note_own_id(&c, &["203.0.113.7".parse().unwrap()]);
        assert_eq!(
            lock(&c).map(|(ip, _)| ip),
            Some("203.0.113.7".parse().unwrap())
        );
    }

    #[test]
    fn broadcast_targets_include_limited_broadcast() {
        let t = broadcast_targets(47401);
        assert!(t.contains(&SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), 47401)));
    }
}
