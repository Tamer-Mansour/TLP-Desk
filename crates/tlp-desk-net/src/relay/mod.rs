//! Relay (TLP-relay): reaching a device that is not on this network through a server the
//! user runs themselves.
//!
//! The relay is a plain UDP forwarder (TURN-like). It never sees anything in clear: the
//! session it carries is the ordinary QUIC/TLS 1.3 session between viewer and host, so key
//! pinning, SPAKE2 and the permission model work unchanged. What the relay adds is a small
//! control protocol, authenticated with a shared secret (the **relay token**) so only the
//! relay owner's devices can use it:
//!
//! 1. The host registers: `Register { device_id }` from a control socket every
//!    [`REGISTER_EVERY`]; the relay remembers the host's public address (the registration
//!    keeps the host's NAT mapping open).
//! 2. The viewer asks for a device: `Connect { device_id }`. The relay allocates a UDP
//!    **session port** `P` and answers `Allocated { session, port }`; it tells the host with
//!    `Incoming { session, port }` through the registration mapping.
//! 3. Both sides send `Punch { session, role }` to `relay:P` **from their QUIC socket**
//!    ([`Endpoint::send_raw`]). The relay learns each side's public address from the punch
//!    (and each side's NAT now lets `relay:P` in). Once it has both it tells the viewer
//!    `Ready`.
//! 4. The viewer dials `relay:P` like any address ([`Endpoint::dial_any`] with the device
//!    ID pin). From then on the relay copies packets host <-> viewer until the session port
//!    is idle for [`server::IDLE`].
//!
//! Every control message is `MAGIC | VERSION | postcard(Frame) | HMAC-SHA256`, keyed by the
//! token and carrying a timestamp (window [`TS_WINDOW`]). A wrong token is dropped silently;
//! a captured `Register` replayed inside the window can at most make the relay point a
//! device's incoming notices at the replayer, who cannot complete the pinned handshake.
//!
//! Nothing here runs unless the user configured a relay (CLAUDE.md hard rule 2: the relay
//! is the user's own server, named explicitly in the settings).

pub mod server;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tlp_desk_crypto::DeviceId;
use tokio::net::UdpSocket;
use tokio::time::{sleep, timeout, Instant};

use crate::endpoint::Endpoint;
use crate::NetError;

/// Default UDP port of the relay's control socket.
pub const DEFAULT_RELAY_PORT: u16 = 47402;
/// Default session port range of a relay (one port per live session).
pub const DEFAULT_SESSION_PORTS: std::ops::RangeInclusive<u16> = 47410..=47509;
/// First bytes of every control datagram.
pub const MAGIC: [u8; 4] = *b"TLPR";
/// Control protocol version.
pub const VERSION: u8 = 1;
/// Largest control datagram accepted.
pub const MAX_FRAME: usize = 256;
/// A message whose timestamp is further than this from the receiver's clock is dropped.
pub const TS_WINDOW: Duration = Duration::from_secs(120);
/// How often a host re-registers (keeps its NAT mapping and the relay entry alive).
pub const REGISTER_EVERY: Duration = Duration::from_secs(15);
/// Punches sent per attempt (the first may be lost while the NAT mapping is created).
const PUNCHES: u32 = 3;
const PUNCH_GAP: Duration = Duration::from_millis(150);
const TAG_LEN: usize = 32;
const HEADER_LEN: usize = MAGIC.len() + 1;
const TOKEN_LABEL: &[u8] = b"tlp-desk/relay/token/v1";

type HmacSha256 = Hmac<Sha256>;

/// The shared secret, already stretched into the HMAC key. Never logged.
#[derive(Clone)]
pub struct Token([u8; 32]);

impl Token {
    /// Derives the key from the secret the user typed (any non-empty string).
    pub fn from_secret(secret: &str) -> Result<Self, NetError> {
        let secret = secret.trim();
        if secret.is_empty() {
            return Err(NetError::Relay("the relay token is empty".into()));
        }
        let mut h = Sha256::new();
        h.update(TOKEN_LABEL);
        h.update(secret.as_bytes());
        Ok(Self(h.finalize().into()))
    }

    fn mac(&self, body: &[u8]) -> [u8; TAG_LEN] {
        // HMAC accepts a key of any length; a 32-byte key cannot fail.
        let mut m = match <HmacSha256 as KeyInit>::new_from_slice(&self.0) {
            Ok(m) => m,
            Err(_) => return [0; TAG_LEN],
        };
        m.update(body);
        m.finalize().into_bytes().into()
    }

    fn verify(&self, body: &[u8], tag: &[u8]) -> bool {
        let Ok(mut m) = <HmacSha256 as KeyInit>::new_from_slice(&self.0) else {
            return false;
        };
        m.update(body);
        m.verify_slice(tag).is_ok()
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(..)")
    }
}

/// Which side of a session a punch comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Host,
    Viewer,
}

/// Control messages (see the module docs for the exchange).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Payload {
    /// Host -> relay: "device `device_id` is reachable at the address this came from".
    Register { device_id: u32 },
    /// Relay -> host: registration seen.
    Registered,
    /// Viewer -> relay: "I want device `device_id`".
    Connect { device_id: u32 },
    /// Relay -> viewer: a session port was allocated; punch it, then wait for `Ready`.
    Allocated { session: u64, port: u16 },
    /// Relay -> viewer: that device is not registered (or its registration expired).
    Offline,
    /// Relay -> host: a viewer wants you; punch session `port`.
    Incoming { session: u64, port: u16 },
    /// Either side -> relay session port, from the QUIC socket.
    Punch { session: u64, role: Role },
    /// Relay -> viewer: both sides punched; dial the session port now.
    Ready { session: u64 },
}

/// A timestamped control message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    /// Sender's clock, seconds since the Unix epoch.
    pub ts: u64,
    pub payload: Payload,
}

/// Seconds since the Unix epoch (0 if the clock is before 1970).
pub fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Encodes and signs a control message.
pub fn encode(token: &Token, payload: &Payload) -> Vec<u8> {
    let frame = Frame {
        ts: now_ts(),
        payload: payload.clone(),
    };
    let mut out = Vec::with_capacity(96);
    out.extend_from_slice(&MAGIC);
    out.push(VERSION);
    // A fixed enum of small integers cannot fail to serialize.
    if let Ok(body) = postcard::to_stdvec(&frame) {
        out.extend_from_slice(&body);
    }
    let tag = token.mac(&out);
    out.extend_from_slice(&tag);
    out
}

/// Checks magic, version, tag and timestamp and returns the message; `None` for anything
/// else (including every QUIC packet, which never starts with the magic).
pub fn decode(token: &Token, bytes: &[u8], now: u64) -> Option<Payload> {
    if bytes.len() < HEADER_LEN + TAG_LEN || bytes.len() > MAX_FRAME {
        return None;
    }
    if bytes.get(..MAGIC.len())? != MAGIC || *bytes.get(MAGIC.len())? != VERSION {
        return None;
    }
    let split = bytes.len().checked_sub(TAG_LEN)?;
    let (body, tag) = (bytes.get(..split)?, bytes.get(split..)?);
    if !token.verify(body, tag) {
        return None;
    }
    let frame: Frame = postcard::from_bytes(body.get(HEADER_LEN..)?).ok()?;
    let window = TS_WINDOW.as_secs();
    if frame.ts.abs_diff(now) > window {
        return None;
    }
    Some(frame.payload)
}

/// Where the relay is and the secret that unlocks it.
#[derive(Clone, Debug)]
pub struct RelayConfig {
    /// `host` or `host:port` (default port [`DEFAULT_RELAY_PORT`]). DNS names are fine.
    pub addr: String,
    pub token: Token,
}

impl RelayConfig {
    /// Builds a config from the two settings strings.
    pub fn new(addr: &str, secret: &str) -> Result<Self, NetError> {
        let addr = addr.trim();
        if addr.is_empty() {
            return Err(NetError::Relay("the relay address is empty".into()));
        }
        Ok(Self {
            addr: addr.to_owned(),
            token: Token::from_secret(secret)?,
        })
    }

    /// Resolves the address (DNS if needed); IPv4 first, like most home connections.
    pub async fn resolve(&self) -> Result<SocketAddr, NetError> {
        let target = if self.addr.contains(':') && !self.addr.starts_with('[') {
            // "host:port" (an IPv6 literal must be written "[::1]:port").
            self.addr.clone()
        } else if self.addr.starts_with('[') && !self.addr.contains("]:") {
            format!("{}:{DEFAULT_RELAY_PORT}", self.addr)
        } else if self.addr.contains("]:") {
            self.addr.clone()
        } else {
            format!("{}:{DEFAULT_RELAY_PORT}", self.addr)
        };
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host(&target)
            .await
            .map_err(|e| NetError::Relay(format!("cannot resolve {}: {e}", self.addr)))?
            .collect();
        addrs
            .iter()
            .find(|a| a.is_ipv4())
            .or_else(|| addrs.first())
            .copied()
            .ok_or_else(|| NetError::Relay(format!("{} has no address", self.addr)))
    }
}

/// A control socket of the same family as `relay` (bound to any port).
async fn control_socket(relay: SocketAddr) -> Result<UdpSocket, NetError> {
    let bind: SocketAddr = if relay.is_ipv4() {
        "0.0.0.0:0".parse().map_err(|_| NetError::Timeout)?
    } else {
        "[::]:0".parse().map_err(|_| NetError::Timeout)?
    };
    Ok(UdpSocket::bind(bind).await?)
}

/// Sends `Punch` to the session port a few times from the endpoint's QUIC socket.
async fn punch(ep: &Endpoint, to: SocketAddr, token: &Token, session: u64, role: Role) {
    let bytes = encode(token, &Payload::Punch { session, role });
    for i in 0..PUNCHES {
        if let Err(e) = ep.send_raw(to, &bytes) {
            tracing::debug!(%e, %to, "relay punch not sent");
        }
        if i + 1 < PUNCHES {
            sleep(PUNCH_GAP).await;
        }
    }
}

/// Host side: keeps this endpoint registered with the relay and answers `Incoming` notices
/// with a punch, forever (abort the task to stop). Also lets the relay's address through
/// the endpoint's allow-list ([`Endpoint::set_relay_peer`]). Logs, never fails: a relay
/// that is down is retried every [`REGISTER_EVERY`].
pub async fn run_host(ep: Arc<Endpoint>, cfg: RelayConfig) {
    let device_id = ep.device_id().as_u32();
    let mut relay: Option<(SocketAddr, UdpSocket)> = None;
    let mut acked = false;
    let mut last_ack = Instant::now();
    let mut buf = vec![0u8; MAX_FRAME + 1];
    loop {
        if relay.is_none() {
            match cfg.resolve().await {
                Ok(addr) => match control_socket(addr).await {
                    Ok(sock) => {
                        ep.set_relay_peer(Some(addr.ip()));
                        tracing::info!(%addr, "relay: registering this device");
                        relay = Some((addr, sock));
                    }
                    Err(e) => tracing::warn!(%e, "relay: cannot open a control socket"),
                },
                Err(e) => tracing::warn!(%e, "relay: address not resolved; retrying"),
            }
            if relay.is_none() {
                sleep(REGISTER_EVERY * 2).await;
                continue;
            }
        }
        let Some((addr, sock)) = relay.as_ref() else {
            continue;
        };
        let reg = encode(&cfg.token, &Payload::Register { device_id });
        if let Err(e) = sock.send_to(&reg, addr).await {
            tracing::debug!(%e, "relay: register not sent");
        }
        let deadline = Instant::now() + REGISTER_EVERY;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let Ok(Ok((n, from))) = timeout(left, sock.recv_from(&mut buf)).await else {
                break;
            };
            if from.ip() != addr.ip() {
                continue;
            }
            let Some(msg) = buf.get(..n).and_then(|b| decode(&cfg.token, b, now_ts())) else {
                continue;
            };
            match msg {
                Payload::Registered => {
                    last_ack = Instant::now();
                    if !acked {
                        acked = true;
                        tracing::info!(relay = %addr, "relay: this device is reachable through the relay");
                    }
                }
                Payload::Incoming { session, port } => {
                    let to = SocketAddr::new(addr.ip(), port);
                    tracing::info!(session, %to, "relay: incoming session; punching");
                    let (ep, token) = (ep.clone(), cfg.token.clone());
                    tokio::spawn(async move { punch(&ep, to, &token, session, Role::Host).await });
                }
                _ => {}
            }
        }
        if acked && last_ack.elapsed() > REGISTER_EVERY * 4 {
            tracing::warn!(relay = %addr, "relay: no answer for a minute; re-resolving");
            acked = false;
            ep.set_relay_peer(None);
            relay = None;
        }
    }
}

/// Viewer side: asks the relay for `device_id`, punches the session port and waits until the
/// host punched too. Returns the address to dial (`relay:port`), to be used with
/// [`Endpoint::dial_any`] and the device-ID pin. The whole exchange is bounded by `limit`.
pub async fn request(
    ep: &Endpoint,
    cfg: &RelayConfig,
    device_id: DeviceId,
    limit: Duration,
) -> Result<SocketAddr, NetError> {
    timeout(limit, request_inner(ep, cfg, device_id))
        .await
        .map_err(|_| NetError::Relay("the relay did not answer in time".into()))?
}

async fn request_inner(
    ep: &Endpoint,
    cfg: &RelayConfig,
    device_id: DeviceId,
) -> Result<SocketAddr, NetError> {
    let relay = cfg.resolve().await?;
    let sock = control_socket(relay).await?;
    let mut buf = vec![0u8; MAX_FRAME + 1];
    let ask = encode(
        &cfg.token,
        &Payload::Connect {
            device_id: device_id.as_u32(),
        },
    );
    // Ask until the relay answers (a lost datagram costs 1.5 s, not the whole limit).
    let (session, port) = loop {
        sock.send_to(&ask, relay).await?;
        let t = Instant::now() + Duration::from_millis(1500);
        let answer = loop {
            let left = t.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break None;
            }
            let Ok(Ok((n, from))) = timeout(left, sock.recv_from(&mut buf)).await else {
                break None;
            };
            if from.ip() != relay.ip() {
                continue;
            }
            match buf.get(..n).and_then(|b| decode(&cfg.token, b, now_ts())) {
                Some(Payload::Allocated { session, port }) => break Some(Ok((session, port))),
                Some(Payload::Offline) => break Some(Err(NetError::RelayOffline)),
                _ => continue,
            }
        };
        match answer {
            Some(Ok(sp)) => break sp,
            Some(Err(e)) => return Err(e),
            None => continue,
        }
    };
    let target = SocketAddr::new(relay.ip(), port);
    tracing::info!(session, %target, "relay: session allocated; punching");
    // Punch until the relay says the host punched too (re-punch every 500 ms: the host's
    // notice may have been lost and the relay re-sends it on our punches).
    loop {
        punch(ep, target, &cfg.token, session, Role::Viewer).await;
        let t = Instant::now() + Duration::from_millis(500);
        loop {
            let left = t.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let Ok(Ok((n, from))) = timeout(left, sock.recv_from(&mut buf)).await else {
                break;
            };
            if from.ip() != relay.ip() {
                continue;
            }
            match buf.get(..n).and_then(|b| decode(&cfg.token, b, now_ts())) {
                Some(Payload::Ready { session: s }) if s == session => {
                    tracing::info!(session, %target, "relay: both sides punched; dialing");
                    return Ok(target);
                }
                Some(Payload::Offline) => return Err(NetError::RelayOffline),
                _ => continue,
            }
        }
    }
}

/// The relay's IP as seen by the endpoint's allow-list (used by the viewer, which otherwise
/// refuses to dial anything outside the LAN).
pub fn is_relay_addr(relay: Option<IpAddr>, addr: SocketAddr) -> bool {
    relay.is_some_and(|ip| crate::allow::normalize(ip) == crate::allow::normalize(addr.ip()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> Token {
        Token::from_secret("  correct horse  ").unwrap()
    }

    #[test]
    fn token_rejects_empty_and_trims() {
        assert!(Token::from_secret("   ").is_err());
        assert_eq!(
            Token::from_secret("abc").unwrap().0,
            Token::from_secret(" abc ").unwrap().0
        );
        assert_ne!(
            Token::from_secret("abc").unwrap().0,
            Token::from_secret("abd").unwrap().0
        );
        assert_eq!(format!("{:?}", token()), "Token(..)");
    }

    #[test]
    fn frames_round_trip_and_reject_tampering() {
        let t = token();
        let p = Payload::Allocated {
            session: 0x1234_5678_9abc_def0,
            port: 47410,
        };
        let bytes = encode(&t, &p);
        assert!(bytes.starts_with(&MAGIC));
        assert!(bytes.len() <= MAX_FRAME);
        assert_eq!(decode(&t, &bytes, now_ts()), Some(p.clone()));
        // Wrong token.
        assert_eq!(
            decode(&Token::from_secret("other").unwrap(), &bytes, now_ts()),
            None
        );
        // Any flipped byte.
        for i in 0..bytes.len() {
            let mut bad = bytes.clone();
            bad[i] ^= 0x01;
            assert_eq!(decode(&t, &bad, now_ts()), None, "byte {i}");
        }
        // Too short, too long, not ours.
        assert_eq!(decode(&t, &bytes[..bytes.len() - 1], now_ts()), None);
        assert_eq!(decode(&t, &[0u8; MAX_FRAME + 1], now_ts()), None);
        assert_eq!(decode(&t, b"", now_ts()), None);
        // A QUIC long header (first byte has the high bit set) is never a frame.
        let mut quic = vec![0xC0u8; 60];
        quic[1..5].copy_from_slice(&[0, 0, 0, 1]);
        assert_eq!(decode(&t, &quic, now_ts()), None);
    }

    #[test]
    fn stale_or_future_frames_are_dropped() {
        let t = token();
        let bytes = encode(&t, &Payload::Registered);
        let now = now_ts();
        assert!(decode(&t, &bytes, now + TS_WINDOW.as_secs()).is_some());
        assert!(decode(&t, &bytes, now + TS_WINDOW.as_secs() + 2).is_none());
        assert!(decode(&t, &bytes, now.saturating_sub(TS_WINDOW.as_secs() + 2)).is_none());
    }

    #[test]
    fn config_needs_both_fields() {
        assert!(RelayConfig::new("", "x").is_err());
        assert!(RelayConfig::new("relay.example", "").is_err());
        assert!(RelayConfig::new(" relay.example:1 ", "x").is_ok());
    }

    #[tokio::test]
    async fn resolve_adds_the_default_port() {
        let c = RelayConfig::new("127.0.0.1", "x").unwrap();
        assert_eq!(
            c.resolve().await.unwrap(),
            SocketAddr::from(([127, 0, 0, 1], DEFAULT_RELAY_PORT))
        );
        let c = RelayConfig::new("127.0.0.1:5000", "x").unwrap();
        assert_eq!(c.resolve().await.unwrap().port(), 5000);
        let c = RelayConfig::new("[::1]", "x").unwrap();
        assert_eq!(c.resolve().await.unwrap().port(), DEFAULT_RELAY_PORT);
        let c = RelayConfig::new("[::1]:7", "x").unwrap();
        assert_eq!(c.resolve().await.unwrap().port(), 7);
    }

    #[test]
    fn relay_addr_match_ignores_v4_mapping() {
        let v4: IpAddr = "203.0.113.9".parse().unwrap();
        let mapped: SocketAddr = "[::ffff:203.0.113.9]:47410".parse().unwrap();
        assert!(is_relay_addr(Some(v4), mapped));
        assert!(!is_relay_addr(None, mapped));
        assert!(!is_relay_addr(Some(v4), "203.0.113.10:1".parse().unwrap()));
    }
}
