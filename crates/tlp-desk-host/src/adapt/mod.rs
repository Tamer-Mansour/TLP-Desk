//! Adaptive streaming: quality presets, the bitrate / frame-rate / resolution controller, and
//! the state the control loop shares with the video pipeline.
//!
//! * [`QualityTarget`] resolves a viewer's `SetQuality` (presets Speed / Balanced / Quality /
//!   Custom) into a size cap, frame rate and bitrate policy; the pipeline applies it live.
//! * [`Controller`] turns congestion signals (viewer `ViewerFeedback`, frames dropped by
//!   `send_video`, frames in flight, QUIC RTT and congestion events) into an encoder bitrate,
//!   a frame rate and a capture scale. See `controller` for the policy.
//! * `EncodeAdapter` runs the controller on the encode thread; `open_source` opens the capture
//!   source (one monitor or all of them) with the right size cap.
//! * `AdaptShared` carries viewer feedback from the session's control loop to the encode
//!   thread, and the session clock that `VideoFrame::timestamp_us` and `LatencyEcho` use.

mod control;
mod controller;
mod encode;
mod quality;
#[cfg(test)]
mod simulation;
mod source;

use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

pub(crate) use control::PerfControl;
pub use controller::{
    AdaptDecision, Congestion, Controller, ControllerConfig, Signals, ViewerReport,
};
pub(crate) use encode::EncodeAdapter;
pub use quality::{capture_cap, BitrateMode, QualityTarget};
pub(crate) use source::{open_source, wanted_cap, Source};

/// Shared between the control loop (writer) and the encode thread (reader).
#[derive(Debug)]
pub(crate) struct AdaptShared {
    feedback: Mutex<Option<ViewerReport>>,
    /// The session clock: `VideoFrame::timestamp_us` and `LatencyEcho` count from here.
    pub(crate) clock: Instant,
}

impl AdaptShared {
    pub(crate) fn new(clock: Instant) -> Self {
        Self {
            feedback: Mutex::new(None),
            clock,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Option<ViewerReport>> {
        self.feedback.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Records one `ViewerFeedback`: dropped frames add up until the next tick, the rest is
    /// the newest value.
    pub(crate) fn push_feedback(&self, report: ViewerReport) {
        let mut slot = self.lock();
        let dropped = slot.map_or(0, |r| r.frames_dropped);
        *slot = Some(ViewerReport {
            frames_dropped: dropped.saturating_add(report.frames_dropped),
            ..report
        });
    }

    /// Takes the feedback merged since the previous call.
    pub(crate) fn take_feedback(&self) -> Option<ViewerReport> {
        self.lock().take()
    }

    /// Microseconds on the session clock now.
    pub(crate) fn now_us(&self) -> u64 {
        u64::try_from(self.clock.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_merges_drops_and_keeps_newest_timings() {
        let s = AdaptShared::new(Instant::now());
        assert!(s.take_feedback().is_none());
        s.push_feedback(ViewerReport {
            decode_us: 5_000,
            frames_dropped: 2,
            rtt_us: 900,
        });
        s.push_feedback(ViewerReport {
            decode_us: 7_000,
            frames_dropped: u32::MAX,
            rtt_us: 1_100,
        });
        assert_eq!(
            s.take_feedback(),
            Some(ViewerReport {
                decode_us: 7_000,
                frames_dropped: u32::MAX,
                rtt_us: 1_100,
            })
        );
        assert!(s.take_feedback().is_none());
        assert!(s.now_us() < 60_000_000);
    }
}
