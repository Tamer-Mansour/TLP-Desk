//! Signal processing on interleaved `f32` PCM: format conversion and silence detection.

mod resample;
mod silence;

pub use resample::Resampler;
pub use silence::{SilenceGate, DEFAULT_HANGOVER, DEFAULT_THRESHOLD};
