//! Adaptive streaming: quality presets.
//!
//! * [`QualityTarget`] resolves a viewer's `SetQuality` (presets Speed / Balanced / Quality /
//!   Custom) into a size cap, frame rate and bitrate policy; the pipeline applies it live.

mod quality;

pub use quality::{capture_cap, BitrateMode, QualityTarget};
