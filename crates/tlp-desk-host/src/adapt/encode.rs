//! The encode thread's side of adaptive streaming: gathers the signals for the controller,
//! applies its decisions to the encoder and tells the capture thread the resolution step.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tlp_desk_codec::{EncoderConfig, VideoEncoder};
use tlp_desk_net::{Session, TransportStats};
use tokio::sync::watch;

use super::controller::{Controller, ControllerConfig, Signals, TICK};
use super::quality::{BitrateMode, QualityTarget};
use super::AdaptShared;

/// Largest adaptive bitrate ceiling, kbps (Quality preset at 4K).
const MAX_CEILING_KBPS: u64 = 80_000;

/// Re-tune the encoder only when the target moved by more than this (per mille), so rate
/// control is not reset for noise.
const RETUNE_PERMILLE: u64 = 30;

/// Controller bounds for a quality target and the *full-size* frame (the capture cap, before
/// the ladder shrinks it).
pub(crate) fn controller_config(q: &QualityTarget, size: (u32, u32)) -> ControllerConfig {
    match q.bitrate {
        BitrateMode::Fixed(kbps) => ControllerConfig::fixed(kbps, q.fps),
        BitrateMode::Adaptive { ceiling_pct } => {
            let base = EncoderConfig::for_screen(size.0.max(2), size.1.max(2), q.fps).bitrate_kbps;
            let ceiling = (u64::from(base) * u64::from(ceiling_pct) / 100).min(MAX_CEILING_KBPS);
            let pixels = u64::from(size.0.max(2)) * u64::from(size.1.max(2));
            ControllerConfig::adaptive(ceiling as u32, q.fps).with_frame_pixels(pixels)
        }
    }
}

/// Owned by the encode thread.
pub(crate) struct EncodeAdapter {
    ctl: Controller,
    quality_rx: watch::Receiver<QualityTarget>,
    quality: QualityTarget,
    size: (u32, u32),
    next_tick: Instant,
    last_tick: Instant,
    last_stats: TransportStats,
    send_drops: u32,
    applied_kbps: u32,
    scale: Arc<AtomicU32>,
}

impl EncodeAdapter {
    /// `scale` is shared with the capture thread (per mille of the quality cap).
    pub(crate) fn new(
        mut quality_rx: watch::Receiver<QualityTarget>,
        scale: Arc<AtomicU32>,
        session: &Session,
    ) -> Self {
        let quality = *quality_rx.borrow_and_update();
        let cfg = controller_config(&quality, quality.max_size);
        let now = Instant::now();
        Self {
            ctl: Controller::new(cfg),
            quality_rx,
            quality,
            size: (0, 0),
            next_tick: now + TICK,
            last_tick: now,
            last_stats: session.transport_stats(),
            send_drops: 0,
            applied_kbps: cfg.max_kbps,
            scale,
        }
    }

    /// Frames per second to encode now.
    pub(crate) fn fps(&self) -> u32 {
        self.quality.fps.min(self.ctl.decision().fps).max(1)
    }

    /// When [`EncodeAdapter::tick`] is due.
    pub(crate) fn next_tick(&self) -> Instant {
        self.next_tick
    }

    /// `send_video` dropped a frame.
    pub(crate) fn frame_dropped(&mut self) {
        self.send_drops = self.send_drops.saturating_add(1);
    }

    fn refresh_quality(&mut self) -> bool {
        if !self.quality_rx.has_changed().unwrap_or(false) {
            return false;
        }
        let q = *self.quality_rx.borrow_and_update();
        if q == self.quality {
            return false;
        }
        tracing::info!(preset = ?q.preset, cap = ?q.max_size, fps = q.fps, bitrate = ?q.bitrate, "quality target changed");
        self.quality = q;
        true
    }

    /// Configuration for a new encoder for `size` frames, at the current target bitrate.
    /// `size` is the captured (possibly ladder-scaled) frame; the controller works with the
    /// full-size frame it came from.
    pub(crate) fn encoder_config(&mut self, size: (u32, u32)) -> EncoderConfig {
        self.refresh_quality();
        let scale = u64::from(self.ctl.decision().scale_permille.clamp(100, 1000));
        let up = |v: u32| u32::try_from(u64::from(v) * 1000 / scale).unwrap_or(u32::MAX);
        self.size = (up(size.0), up(size.1));
        self.ctl
            .reconfigure(controller_config(&self.quality, self.size));
        let d = self.ctl.decision();
        self.applied_kbps = d.bitrate_kbps;
        EncoderConfig {
            width: size.0,
            height: size.1,
            fps: self.quality.fps,
            bitrate_kbps: d.bitrate_kbps,
        }
    }

    /// One controller tick: read the signals, decide, apply. Call when
    /// [`EncodeAdapter::next_tick`] has passed.
    pub(crate) fn tick(
        &mut self,
        session: &Session,
        shared: &AdaptShared,
        enc: Option<&mut (dyn VideoEncoder + 'static)>,
    ) {
        let now = Instant::now();
        self.next_tick = now + TICK;
        if self.refresh_quality() && self.size != (0, 0) {
            self.ctl
                .reconfigure(controller_config(&self.quality, self.size));
        }
        let stats = session.transport_stats();
        let dt = now
            .saturating_duration_since(self.last_tick)
            .max(Duration::from_millis(1));
        let sent_bytes = stats.sent_bytes.saturating_sub(self.last_stats.sent_bytes);
        let sent_kbps = sent_bytes.saturating_mul(8) / (dt.as_millis() as u64).max(1);
        let signals = Signals {
            at: now.saturating_duration_since(shared.clock),
            viewer: shared.take_feedback(),
            send_drops: std::mem::take(&mut self.send_drops),
            in_flight: session.video_in_flight(),
            rtt: stats.rtt,
            congestion_events: stats
                .congestion_events
                .saturating_sub(self.last_stats.congestion_events),
            sent_kbps: Some(u32::try_from(sent_kbps).unwrap_or(u32::MAX)),
        };
        self.last_stats = stats;
        self.last_tick = now;
        let before = self.ctl.decision();
        let d = self.ctl.tick(&signals);
        if d != before {
            tracing::debug!(
                kbps = d.bitrate_kbps,
                fps = d.fps,
                scale = d.scale_permille,
                congestion = ?self.ctl.last_congestion(),
                "adaptive decision"
            );
        }
        self.scale.store(d.scale_permille, Ordering::Release);
        if let Some(enc) = enc {
            let (a, b) = (u64::from(self.applied_kbps), u64::from(d.bitrate_kbps));
            if a.abs_diff(b) * 1000 > a * RETUNE_PERMILLE {
                match enc.set_bitrate(d.bitrate_kbps) {
                    Ok(()) => self.applied_kbps = d.bitrate_kbps,
                    Err(e) => tracing::debug!(%e, "encoder refused the new bitrate"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tlp_desk_proto::QualityPreset;

    #[test]
    fn ceilings_follow_preset_and_frame_size() {
        let d = QualityTarget::host_default((1920, 1200), 60);
        let c = controller_config(&d, (1920, 1080));
        assert_eq!(c.max_kbps, 14_929);
        // The ladder works from the full-size frame.
        assert_eq!(c.frame_pixels, 1920 * 1080);
        assert!(c.adaptive);
        let speed = QualityTarget {
            preset: QualityPreset::Speed,
            bitrate: BitrateMode::Adaptive { ceiling_pct: 60 },
            ..d
        };
        assert_eq!(controller_config(&speed, (1280, 720)).max_kbps, 3_981);
        let quality = QualityTarget {
            bitrate: BitrateMode::Adaptive { ceiling_pct: 160 },
            ..d
        };
        // 4K60: ~60 Mbps default, x1.6 is capped at 80 Mbps.
        assert_eq!(controller_config(&quality, (3840, 2160)).max_kbps, 80_000);
        let fixed = QualityTarget {
            bitrate: BitrateMode::Fixed(2_500),
            fps: 30,
            ..d
        };
        let c = controller_config(&fixed, (1920, 1080));
        assert_eq!((c.max_kbps, c.max_fps, c.adaptive), (2_500, 30, false));
        // A zero size never divides by zero.
        assert!(controller_config(&d, (0, 0)).max_kbps >= 300);
    }
}
