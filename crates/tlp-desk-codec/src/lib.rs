//! Video encode/decode traits and implementations.
//!
//! Backends: software H.264 via OpenH264 ([`H264Encoder`], [`H264Decoder`]), compiled from the
//! bundled source at build time (no runtime download), and on Windows hardware H.264 through
//! Media Foundation encoder MFTs (`MfEncoder`: Intel Quick Sync, NVIDIA, AMD). Colour
//! conversion uses BT.709 limited range, which the software encoder signals in the SPS VUI.
//!
//! [`new_encoder`] tries hardware first and falls back to OpenH264 ([`FallbackEncoder`]); the
//! [`ENCODER_ENV`] environment variable (`auto`, `hardware`, `software`) overrides the choice.
//!
//! `unsafe` is denied crate-wide; the exceptions are the OpenH264 encoder FFI in `h264::ffi`
//! and the Windows Media Foundation / DXGI wrappers in `mf`, which document their
//! `// SAFETY:` invariants.
#![deny(unsafe_code)]

use tlp_desk_capture::BgraFrame;

pub mod convert;
mod h264;
#[cfg(windows)]
mod mf;
mod qp;
mod select;

pub use h264::{Downscale, H264Complexity, H264Decoder, H264Encoder, H264Tuning};
#[cfg(windows)]
pub use mf::{
    list_adapters, list_hw_encoders, GpuAdapter, HwAdapter, HwEncoderInfo, MfEncoder, VENDOR_AMD,
    VENDOR_INTEL, VENDOR_NVIDIA,
};
pub use qp::QpProbe;
pub use select::{
    new_encoder_with, EncoderOptions, EncoderPreference, FallbackEncoder, ADAPTER_ENV, ENCODER_ENV,
};

/// Quality floor: the coarsest quantiser (H.264 QP, 0-51) the encoders may use. About 32 keeps
/// UI text legible; a stream that cannot afford it is made smaller (lower resolution) by the
/// host's adaptive controller instead of blurrier.
pub const QUALITY_MAX_QP: u8 = 32;
/// The finest quantiser the encoders use (below this the bits buy nothing visible).
pub const QUALITY_MIN_QP: u8 = 12;

/// Encoder settings. `width`/`height` are the size of the *input* frames (the captured
/// monitor); the encoded size may be smaller, see [`H264Encoder::encoded_size`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
}

impl EncoderConfig {
    /// A sensible default for LAN screen sharing: `fps` frames per second and a bitrate that
    /// scales with the encoded resolution under [`Downscale::Auto`]: about 12.4 Mbps for
    /// 1080p60, 13.8 Mbps for 1920x1200@60, 6.2 Mbps for 1080p30.
    ///
    /// For frames already downscaled by the capturer (`CaptureOptions::max_size`), pass the
    /// *frame* size: it fits the encoder, so it is encoded as is (no second downscale).
    pub fn for_screen(width: u32, height: u32, fps: u32) -> Self {
        let (ew, eh) = h264::auto_encoded_size(width, height);
        let pixels_per_sec = ew as u64 * eh as u64 * fps.max(1) as u64;
        // ~0.1 bits per pixel (sharp text when scrolling at 60 fps), clamped to a range that
        // suits wired and Wi-Fi LANs. Rate control spends far less on static screens.
        let kbps = (pixels_per_sec / 10_000).clamp(1_000, 40_000) as u32;
        Self {
            width,
            height,
            fps,
            bitrate_kbps: kbps,
        }
    }
}

/// Encoder output for one input frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    pub keyframe: bool,
    pub timestamp_us: u64,
    /// Annex-B bitstream for H.264.
    pub data: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("frame size {got_w}x{got_h} does not match configured {want_w}x{want_h}")]
    SizeMismatch {
        got_w: u32,
        got_h: u32,
        want_w: u32,
        want_h: u32,
    },
    #[error("corrupt or unsupported bitstream")]
    BadBitstream,
    #[error("codec backend error: {0}")]
    Backend(String),
}

pub trait VideoEncoder: Send {
    /// Encodes one frame. `Ok(None)` means the encoder dropped or buffered it.
    fn encode(&mut self, frame: &BgraFrame) -> Result<Option<EncodedFrame>, CodecError>;

    /// The next encoded frame will be a keyframe (viewer joined or lost packets).
    fn request_keyframe(&mut self);

    /// Changes bitrate on the fly for adaptive quality.
    fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError>;

    fn config(&self) -> EncoderConfig;

    /// Short human-readable name of the implementation in use (logs, stats overlay).
    fn backend_label(&self) -> String {
        "H.264".into()
    }
}

pub trait VideoDecoder: Send {
    /// Decodes one access unit. `Ok(None)` means more data is needed before a picture.
    fn decode(&mut self, data: &[u8]) -> Result<Option<BgraFrame>, CodecError>;

    /// Hands a decoded frame's buffer back for reuse by the next decode. Optional.
    fn recycle(&mut self, frame: BgraFrame) {
        drop(frame);
    }
}

/// Creates the default encoder: hardware H.264 when available, else OpenH264, switching to
/// OpenH264 transparently if the hardware fails mid-stream. Honours [`ENCODER_ENV`] and
/// [`ADAPTER_ENV`] (see [`EncoderOptions::from_env`]).
pub fn new_encoder(config: EncoderConfig) -> Result<Box<dyn VideoEncoder>, CodecError> {
    new_encoder_with(config, &EncoderOptions::from_env())
}

/// Creates the default (software H.264) decoder.
pub fn new_decoder() -> Result<Box<dyn VideoDecoder>, CodecError> {
    Ok(Box::new(H264Decoder::new()?))
}

#[cfg(test)]
mod tests;
