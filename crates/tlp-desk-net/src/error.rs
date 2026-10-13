use std::net::SocketAddr;

use tlp_desk_crypto::{DeviceId, Fingerprint};
use tlp_desk_proto::RejectReason;

/// Errors from the network layer.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TLS configuration error: {0}")]
    Tls(#[from] rustls::Error),
    #[error("QUIC connect error: {0}")]
    Connect(#[from] quinn::ConnectError),
    #[error("QUIC connection error: {0}")]
    Connection(#[from] quinn::ConnectionError),
    #[error("stream write error: {0}")]
    Write(#[from] quinn::WriteError),
    #[error("stream read error: {0}")]
    Read(#[from] quinn::ReadError),
    #[error("stream read error: {0}")]
    ReadToEnd(#[from] quinn::ReadToEndError),
    #[error("protocol error: {0}")]
    Proto(#[from] tlp_desk_proto::ProtoError),
    #[error("crypto error: {0}")]
    Crypto(#[from] tlp_desk_crypto::CryptoError),
    /// The host presented a key that does not match the dialed device ID / fingerprint.
    /// Possible man-in-the-middle: do not retry silently.
    #[error("host key mismatch: expected {expected}, host presented {actual_id} ({actual_fp})")]
    PinMismatch {
        expected: String,
        actual_id: DeviceId,
        actual_fp: Fingerprint,
    },
    #[error("peer {0} is not in the allow-list")]
    NotAllowed(SocketAddr),
    #[error("protocol version mismatch: ours {ours}, theirs {theirs}")]
    VersionMismatch { ours: u32, theirs: u32 },
    /// The host rejected the session.
    #[error("rejected by host: {0:?}")]
    Rejected(RejectReason),
    /// Host side: the viewer proved the wrong code/password (or tampered with the exchange).
    #[error("authentication failed")]
    AuthFailed,
    /// Viewer side: the host's key-confirmation MAC was wrong.
    #[error("host failed key confirmation")]
    HostAuthFailed,
    /// Host side: the peer is locked out by the rate limiter.
    #[error("rate limited")]
    RateLimited,
    #[error("unexpected message: {0}")]
    Unexpected(String),
    #[error("this operation does not match the requested auth method")]
    WrongAuthKind,
    #[error("timed out")]
    Timeout,
    #[error("connection or stream closed")]
    Closed,
    #[error("discovery error: {0}")]
    Discovery(String),
}

impl From<tokio::time::error::Elapsed> for NetError {
    fn from(_: tokio::time::error::Elapsed) -> Self {
        NetError::Timeout
    }
}
