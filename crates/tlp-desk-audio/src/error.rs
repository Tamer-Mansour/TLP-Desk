//! The crate's one error type.

/// Everything that can go wrong while capturing, coding, sending or playing audio.
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    /// A PCM format this crate cannot convert (zero rate, zero or too many channels).
    #[error("unsupported audio format: {rate} Hz with {channels} channels")]
    UnsupportedFormat {
        /// Samples per second per channel.
        rate: u32,
        /// Interleaved channel count.
        channels: u16,
    },
    /// The Opus encoder or decoder refused an operation.
    #[error("opus {op} failed: {reason}")]
    Codec {
        /// What was attempted (`"encode"`, `"decode"`, ...).
        op: &'static str,
        /// libopus' description of the failure.
        reason: String,
    },
    /// A packet on the audio stream is empty or larger than any packet we produce.
    #[error("audio packet of {len} bytes is outside the 1..={max} byte range")]
    BadPacket {
        /// Length announced by the peer.
        len: usize,
        /// Largest accepted length.
        max: usize,
    },
    /// The system has no default device of the requested kind.
    #[error("no default audio {0} device")]
    NoDevice(&'static str),
    /// The OS audio API failed (device unplugged, exclusive mode, driver error).
    #[error("audio device error: {0}")]
    Device(String),
    /// The session or a side stream failed.
    #[error("audio stream failed: {0}")]
    Stream(String),
}
