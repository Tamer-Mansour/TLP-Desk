//! The session feature: host streaming ([`AudioHost`]), viewer playback ([`AudioViewer`])
//! and the audio stream format.
//!
//! Control flow (protocol v4, `AudioControl`, permission `AUDIO`):
//!
//! 1. The viewer sends `AudioControl { enabled: true, 48000, 2 }` when the user wants audio
//!    and the session holds `AUDIO` (again whenever `AUDIO` is granted later).
//! 2. The host starts loopback capture only now (zero CPU otherwise) and answers with the
//!    `AudioControl` it actually uses. Sound goes out on `StreamKind::Audio` uni streams;
//!    each stream is one *generation* (its id) that ends when the host goes silent and a
//!    new one starts when sound resumes.
//! 3. `enabled: false` from the viewer, revoking `AUDIO`, or the session ending stops
//!    capture at once; the host answers `AudioControl { enabled: false, .. }`.

mod host;
mod player;
mod viewer;
mod wire;

use tlp_desk_proto::Message;

use crate::format::{CHANNELS, SAMPLE_RATE};

pub use host::AudioHost;
pub use player::Player;
pub use viewer::AudioViewer;

/// [`tlp_desk_net::Feature::name`] of both audio features; UI events carry it.
pub const FEATURE_NAME: &str = "audio";

/// What the audio feature tells the UI (payload of a `FeatureEvent` named
/// [`FEATURE_NAME`]).
#[derive(Debug, Clone, PartialEq)]
pub enum AudioEvent {
    /// Viewer side: the session's audio state changed.
    Viewer(ViewerAudioState),
    /// Host side: loopback capture for this session started (`true`) or stopped.
    HostStreaming(bool),
    /// A device could not be opened (no speakers, device in exclusive use, ...). The text is
    /// for logs; the UI shows its own translated message.
    DeviceError(String),
}

/// Viewer-side audio state of one session, for the session toolbar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewerAudioState {
    /// The host currently grants `AUDIO`.
    pub permitted: bool,
    /// The user wants to hear the host (toolbar toggle).
    pub enabled: bool,
    /// Sound is being played (host confirmed and the output device is open).
    pub playing: bool,
    /// Playback volume, `0.0..=1.0`.
    pub volume: f32,
}

/// The `AudioControl` message for our one wire format.
pub(crate) fn control(enabled: bool) -> Message {
    Message::AudioControl {
        enabled,
        sample_rate: SAMPLE_RATE,
        channels: u8::try_from(CHANNELS).unwrap_or(2),
    }
}
