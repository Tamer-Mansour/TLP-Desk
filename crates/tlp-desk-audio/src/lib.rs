//! System audio for TLP-Desk sessions: the host's speakers, heard on the viewer.
//!
//! Pipeline: loopback capture of the host's default render device -> [`Resampler`] to the
//! wire format (48 kHz stereo, see [`format`]) -> 10 ms Opus frames -> a
//! `StreamKind::Audio` side stream -> viewer jitter buffer -> Opus decode -> the viewer's
//! default output device.
//!
//! No function in this crate contacts anything outside the session it was given.
#![forbid(unsafe_code)]

mod codec;
pub mod dsp;
mod error;
pub mod format;
mod jitter;

pub use codec::{OpusDecoder, OpusEncoder, DEFAULT_BITRATE};
pub use dsp::Resampler;
pub use error::AudioError;
pub use jitter::{JitterBuffer, JitterConfig, JitterStats, Playout};
