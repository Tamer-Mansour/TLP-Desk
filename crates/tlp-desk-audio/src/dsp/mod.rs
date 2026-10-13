//! Signal processing on interleaved `f32` PCM: format conversion and silence detection.

mod resample;

pub use resample::Resampler;
