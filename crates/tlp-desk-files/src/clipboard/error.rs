//! The crate's error type.

/// Everything that can go wrong in file transfer and clipboard sync.
///
/// Messages never contain clipboard or file *contents*, only sizes and kinds.
#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    /// The system clipboard could not be opened, read or written.
    #[error("clipboard unavailable: {0}")]
    Clipboard(String),
    /// A clipboard payload is larger than the configured cap.
    #[error("clipboard {kind} of {size} bytes exceeds the {limit} byte limit")]
    TooLarge {
        /// What was too large (`"text"`, `"image"`, `"html"`).
        kind: &'static str,
        /// Size in bytes (decoded RGBA for images).
        size: u64,
        /// The cap in bytes.
        limit: u64,
    },
    /// A clipboard image is malformed (bad PNG, sizes that do not match, zero size).
    #[error("invalid clipboard image: {0}")]
    BadImage(String),
    /// Encoding or decoding a protocol frame failed.
    #[error(transparent)]
    Proto(#[from] tlp_desk_proto::ProtoError),
    /// The session's transport failed.
    #[error(transparent)]
    Net(#[from] tlp_desk_net::NetError),
}
