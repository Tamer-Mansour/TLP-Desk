//! QUIC transport, mDNS discovery, broadcast fallback, Wake-on-LAN.
//!
//! * [`Endpoint`]: one UDP socket (default `[::]:47400`) that accepts sessions (host) and dials
//!   (viewer). TLS 1.3 with mutual auth and key pinning (see [`Pin`]); peers outside the
//!   [`AllowList`] are refused before the handshake.
//! * Host flow: [`Endpoint::accept`] -> [`PendingSession`] -> `accept` / `reject` /
//!   `verify_code` / `verify_password` -> [`Session`].
//! * Viewer flow: [`Endpoint::connect`] (or [`Endpoint::connect_pinned`]) -> [`Session`].
//! * Feature data: [`Session::open_uni`] / [`Session::open_bi`] /
//!   [`Session::accept_side_stream`] carry typed side streams ([`StreamKind`]) next to the
//!   control and video streams.
//! * [`Feature`] / [`FeatureSet`] / [`FeatureDispatch`]: how feature lanes plug into sessions.
//! * [`ChatFeature`]: chat between the viewer and the host user (both roles).
//! * [`Discovery`]: mDNS `_tlpdesk._udp.local.` + broadcast fallback on UDP 47401.
//! * [`send_wake_on_lan`]; [`neighbor_mac`] learns a device's MAC from the OS neighbour cache.
//! * Port forwarding helpers: [`pipe_tcp`] (TCP <-> tunnel stream) and [`resolve_target`]
//!   (LAN-only target check).
//!
//! No function in this crate contacts anything outside the local network.
//!
//! `unsafe` is denied crate-wide; only the Windows neighbour-cache wrapper
//! (`neighbor::windows`) opts back in, with a `// SAFETY:` note on every block.
#![deny(unsafe_code)]
// Never-crash set (docs/CODE_STYLE.md, "Lints"): every byte here comes from a peer.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable,
    clippy::indexing_slicing
)]

mod allow;
mod chat;
mod discovery;
mod endpoint;
mod error;
mod feature;
mod neighbor;
mod session;
mod tls;
mod tunnel;
mod wol;

pub use allow::{normalize, AllowList, BadCidr, Cidr};
pub use chat::{
    ChatAuthor, ChatEntry, ChatEvent, ChatFeature, ChatLimits, ChatRejected, CHAT_FEATURE,
};
pub use discovery::{
    broadcast_targets, Advertisement, BroadcastPacket, DiscoveredDevice, Discovery,
    DiscoveryConfig, DiscoveryEvent, PacketKind, BROADCAST_MAGIC, MDNS_SERVICE_TYPE,
};
pub use endpoint::{
    default_device_name, sanitize_peer_name, AuthKind, AuthMethod, AwaitingAcceptFn, Endpoint,
    EndpointConfig, PendingSession, MAX_PEER_NAME_CHARS,
};
pub use error::NetError;
pub use feature::{
    Feature, FeatureCtx, FeatureDispatch, FeatureEvent, FeatureEventSink, FeatureSet,
};
pub use neighbor::{format_mac, neighbor_mac, neighbor_table, Neighbor};
pub use quinn::{RecvStream, SendStream};
pub use session::{
    stream_priority, PeerInfo, Session, SideStream, TransportStats, VideoSendStatus,
    DEFAULT_MAX_VIDEO_IN_FLIGHT, HANDSHAKE_MAX_FRAME, STREAM_REFUSED,
};
pub use tlp_desk_proto::{StreamHeader, StreamKind};
pub use tls::{Pin, ALPN};
pub use tunnel::{
    close_reason, pipe_tcp, resolve_target, sanitize_close_text, PipeEnd, TargetError,
    TunnelCounters, MAX_CLOSE_TEXT, MAX_TARGET_HOST_LEN, TUNNEL_BUF,
};
pub use wol::{magic_packet, parse_mac, send_wake_on_lan, WOL_PORT};

/// QUIC (session) UDP port.
pub const DEFAULT_PORT: u16 = 47400;
/// Broadcast discovery fallback UDP port.
pub const BROADCAST_PORT: u16 = 47401;
