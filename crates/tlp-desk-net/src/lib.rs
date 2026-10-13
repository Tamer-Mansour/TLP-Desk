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
//! * [`Discovery`]: mDNS `_tlpdesk._udp.local.` + broadcast fallback on UDP 47401.
//! * [`send_wake_on_lan`].
//!
//! No function in this crate contacts anything outside the local network.
#![forbid(unsafe_code)]

mod allow;
mod discovery;
mod endpoint;
mod error;
mod feature;
mod session;
mod tls;
mod wol;

pub use allow::{normalize, AllowList, BadCidr, Cidr};
pub use discovery::{
    broadcast_targets, Advertisement, BroadcastPacket, DiscoveredDevice, Discovery,
    DiscoveryConfig, DiscoveryEvent, PacketKind, BROADCAST_MAGIC, MDNS_SERVICE_TYPE,
};
pub use endpoint::{
    default_device_name, AuthKind, AuthMethod, AwaitingAcceptFn, Endpoint, EndpointConfig,
    PendingSession,
};
pub use error::NetError;
pub use feature::{
    Feature, FeatureCtx, FeatureDispatch, FeatureEvent, FeatureEventSink, FeatureSet,
};
pub use quinn::{RecvStream, SendStream};
pub use session::{
    stream_priority, PeerInfo, Session, SideStream, VideoSendStatus, DEFAULT_MAX_VIDEO_IN_FLIGHT,
    STREAM_REFUSED,
};
pub use tlp_desk_proto::{StreamHeader, StreamKind};
pub use tls::{Pin, ALPN};
pub use wol::{magic_packet, parse_mac, send_wake_on_lan, WOL_PORT};

/// QUIC (session) UDP port.
pub const DEFAULT_PORT: u16 = 47400;
/// Broadcast discovery fallback UDP port.
pub const BROADCAST_PORT: u16 = 47401;
