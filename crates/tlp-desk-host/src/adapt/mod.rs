//! Adaptive streaming: quality presets and the bitrate / frame-rate / resolution controller.
//!
//! * [`QualityTarget`] resolves a viewer's `SetQuality` (presets Speed / Balanced / Quality /
//!   Custom) into a size cap, frame rate and bitrate policy; the pipeline applies it live.
//! * [`Controller`] turns congestion signals (viewer `ViewerFeedback`, frames dropped by
//!   `send_video`, frames in flight, QUIC RTT and congestion events) into an encoder bitrate,
//!   a frame rate and a capture scale. See `controller` for the policy.

mod controller;
mod quality;
#[cfg(test)]
mod simulation;

pub use controller::{
    AdaptDecision, Congestion, Controller, ControllerConfig, Signals, ViewerReport,
};
pub use quality::{capture_cap, BitrateMode, QualityTarget};
