//! Viewer side of adaptive streaming and the latency overlay.
//!
//! * **Feedback**: about four times per second the viewer sends `ViewerFeedback` (average
//!   decode time, frames lost or skipped, RTT) so the host's controller can adapt bitrate,
//!   frame rate and resolution.
//! * **Clock sync**: every 500 ms a `LatencyProbe { t_viewer_us }` goes out; the host echoes
//!   it with its session clock (`LatencyEcho::t_host_capture_us`, the clock of
//!   `VideoFrame::timestamp_us`). [`LatencyClock`] keeps the offset of the probe with the
//!   smallest round trip among the last 16 (NTP-style: the smallest RTT has the least queueing,
//!   so the "half the RTT each way" assumption is most accurate; on a LAN the error is well
//!   under a millisecond).
//! * **Glass-to-glass estimate**: each frame's host timestamp (when the oldest change in it
//!   was captured) is mapped to the viewer's clock; the latency is measured when the UI takes
//!   the frame to draw it (`FrameSlot::take`). What is *not* included: the time before the
//!   capture API delivered the change (up to one refresh interval) and the time from the UI's
//!   draw call to light leaving the screen (vsync + panel, typically one or two refreshes).

use std::collections::VecDeque;

/// Probes kept for the minimum-RTT filter.
const PROBE_WINDOW: usize = 16;

/// Estimates `host clock - viewer clock` from probe echoes.
#[derive(Debug, Clone, Default)]
pub struct LatencyClock {
    /// (round trip, offset), microseconds.
    samples: VecDeque<(u64, i64)>,
}

impl LatencyClock {
    /// A clock with no samples yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one echo: the probe left at `sent_us` and the echo arrived at `received_us`
    /// (viewer clock); `host_us` is the host's clock when it answered. Echoes that arrive
    /// before they were sent (a forged or corrupted echo) are ignored.
    pub fn on_echo(&mut self, sent_us: u64, host_us: u64, received_us: u64) {
        let Some(rtt) = received_us.checked_sub(sent_us) else {
            return;
        };
        let mid = i128::from(sent_us) + i128::from(rtt / 2);
        let offset = i128::from(host_us) - mid;
        let Ok(offset) = i64::try_from(offset) else {
            return;
        };
        if self.samples.len() == PROBE_WINDOW {
            self.samples.pop_front();
        }
        self.samples.push_back((rtt, offset));
    }

    /// `host clock - viewer clock` in microseconds, from the best probe; `None` until the
    /// first echo.
    pub fn offset_us(&self) -> Option<i64> {
        self.samples
            .iter()
            .min_by_key(|(rtt, _)| *rtt)
            .map(|&(_, off)| off)
    }

    /// The smallest probe round trip seen in the window, microseconds.
    pub fn best_rtt_us(&self) -> Option<u64> {
        self.samples.iter().map(|&(rtt, _)| rtt).min()
    }

    /// Maps a host timestamp to the viewer clock (microseconds since the viewer session
    /// started). `None` before the first echo or if it falls before the session start.
    pub fn to_viewer_us(&self, host_us: u64) -> Option<u64> {
        let v = i128::from(host_us) - i128::from(self.offset_us()?);
        u64::try_from(v).ok()
    }
}

/// Counts what the next `ViewerFeedback` reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct FeedbackWindow {
    decode_us: u64,
    decoded: u64,
    dropped: u64,
    last_seq: Option<u64>,
}

impl FeedbackWindow {
    /// A frame arrived from the network: sequence gaps are frames lost on the way (dropped by
    /// the host under congestion or by our receive queue).
    pub(crate) fn on_received(&mut self, seq: u64) {
        if let Some(last) = self.last_seq {
            if seq > last {
                self.dropped = self.dropped.saturating_add(seq - last - 1);
            }
        }
        if self.last_seq.is_none_or(|l| seq > l) {
            self.last_seq = Some(seq);
        }
    }

    /// A frame was decoded in `us` microseconds.
    pub(crate) fn on_decoded(&mut self, us: u64) {
        self.decode_us = self.decode_us.saturating_add(us);
        self.decoded += 1;
    }

    /// A received frame could not be shown (decode error, waiting for a keyframe).
    pub(crate) fn on_skipped(&mut self) {
        self.dropped = self.dropped.saturating_add(1);
    }

    /// `(average decode time, frames dropped)` since the previous call; resets the counters
    /// (the sequence position is kept).
    pub(crate) fn take(&mut self) -> (u32, u32) {
        let avg = self.decode_us.checked_div(self.decoded).unwrap_or(0);
        let out = (
            u32::try_from(avg).unwrap_or(u32::MAX),
            u32::try_from(self.dropped).unwrap_or(u32::MAX),
        );
        self.decode_us = 0;
        self.decoded = 0;
        self.dropped = 0;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_offset_comes_from_the_fastest_probe() {
        let mut c = LatencyClock::new();
        assert_eq!(c.offset_us(), None);
        assert_eq!(c.to_viewer_us(1_000), None);
        // Host clock is 5 s ahead. A slow probe (asymmetric queueing) and a fast one.
        c.on_echo(1_000_000, 6_000_000 + 9_000, 1_010_000); // rtt 10 ms, skewed
        c.on_echo(2_000_000, 7_000_000 + 250, 2_000_500); // rtt 0.5 ms
        assert_eq!(c.best_rtt_us(), Some(500));
        assert_eq!(c.offset_us(), Some(5_000_000));
        assert_eq!(c.to_viewer_us(7_500_000), Some(2_500_000));
        // Before our session started: unknown.
        assert_eq!(c.to_viewer_us(10), None);
    }

    #[test]
    fn hostile_echoes_are_ignored_or_harmless() {
        let mut c = LatencyClock::new();
        // Arrived before it was sent.
        c.on_echo(5_000, 1, 4_000);
        assert_eq!(c.offset_us(), None);
        // Extreme values do not overflow.
        c.on_echo(0, u64::MAX, u64::MAX);
        c.on_echo(u64::MAX - 1, 0, u64::MAX);
        c.on_echo(0, 0, u64::MAX);
        let _ = c.to_viewer_us(u64::MAX);
        let _ = c.to_viewer_us(0);
        // The window is bounded.
        for i in 0..100 {
            c.on_echo(i, i, i + 1);
        }
        assert_eq!(c.samples.len(), PROBE_WINDOW);
        assert_eq!(c.offset_us(), Some(0));
    }

    #[test]
    fn feedback_counts_gaps_skips_and_decode_time() {
        let mut w = FeedbackWindow::default();
        assert_eq!(w.take(), (0, 0));
        w.on_received(10);
        w.on_received(11);
        w.on_received(15); // 12, 13, 14 lost
        w.on_received(14); // stale: not a gap, not a new position
        w.on_decoded(4_000);
        w.on_decoded(6_000);
        w.on_skipped();
        assert_eq!(w.take(), (5_000, 4));
        assert_eq!(w.take(), (0, 0));
        w.on_received(16);
        assert_eq!(w.take(), (0, 0));
        w.on_received(u64::MAX);
        assert_eq!(w.take().1, u32::MAX);
    }
}
