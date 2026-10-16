//! tlp-desk wire protocol: message types, `PROTOCOL_VERSION` and length-prefixed framing.
//!
//! Every message on a stream is a [`Message`] serialized with `postcard` and prefixed by its
//! length as a little-endian `u32`. See `docs/protocol.md`.
#![forbid(unsafe_code)]
// Never-crash set (docs/CODE_STYLE.md, "Lints"): everything here parses hostile input.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable,
    clippy::indexing_slicing
)]

mod framing;
mod messages;
mod stream;

pub use framing::{decode_message, encode_frame, encode_message, FrameDecoder, MAX_FRAME_LEN};
pub use messages::*;
pub use stream::{
    FileDataHeader, StreamHeader, StreamKind, FILE_DATA_HEADER_LEN, STREAM_HEADER_LEN,
};

/// Wire protocol version. Bump on every incompatible change to [`Message`] and update
/// `docs/protocol.md`.
pub const PROTOCOL_VERSION: u32 = 5;

/// Errors produced while encoding or decoding protocol frames.
#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("serialization failed: {0}")]
    Serde(#[from] postcard::Error),
    /// The length prefix is above the decoder's limit ([`MAX_FRAME_LEN`] or
    /// [`FrameDecoder::with_max_len`]).
    #[error("frame of {0} bytes exceeds the size limit")]
    FrameTooLarge(usize),
    #[error("unknown stream kind {0}")]
    UnknownStreamKind(u8),
}
