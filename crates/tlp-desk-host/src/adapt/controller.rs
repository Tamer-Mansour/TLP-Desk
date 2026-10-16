//! The adaptive bitrate / frame-rate / resolution controller.
//!
//! A pure state machine (no clocks, no I/O): the encode thread feeds it [`Signals`] about four
//! times per second and applies the returned [`AdaptDecision`]. That makes it testable against
//! simulated network traces (see `adapt::simulation`).
//!
//! # Policy: quality first, smaller before blurrier
//!
//! *Latency first*: queueing delay is the main congestion signal, so the stream backs off
//! before buffers fill instead of after packets are lost.
//!
//! * **Congestion level** per tick: *severe* when the host had to drop frames, the viewer lost
//!   several or the RTT rose more than 40 ms over its 10-second minimum; *mild* when two or
//!   more frames are in flight, the viewer lost a frame, the RTT rose more than 12 ms (+ half
//!   the base RTT, for Wi-Fi jitter), or QUIC reduced its congestion window while video was
//!   waiting. Isolated packet loss alone is not congestion (Wi-Fi loses packets without being
//!   full, QUIC retransmits, and the loss-driven window reductions it causes are ignored while
//!   no video frame is waiting).
//! * **Bitrate, AIMD**: mild x0.85, severe x0.6 (and, when frames were dropped, never above
//!   85 % of what QUIC actually sent during that tick), at most once per 400 ms. After 1.5 s without congestion
//!   the target grows additively (6 % of the ceiling + 10 % of the current target per second).
//! * **The ladder**: the picture never gets blurrier to fit a smaller link. What a bitrate buys
//!   is measured in *bits per pixel* (bpp = kbps / (pixels x fps)); text stays sharp above
//!   about 0.07 bpp and goes soft below. When congestion has pushed the target so low that the
//!   current size and rate would fall under that floor for 2.5 s, the stream moves down a
//!   [`LADDER`] rung (1920x1200 -> 1600x1000 -> 1280x800 -> 1024x640, with 60 -> 45 -> 30 -> 15
//!   fps interleaved) to the first rung that restores the floor, in one step. Every change
//!   rebuilds the encoder at the new size and costs a keyframe.
//! * **Back up** one rung only after 10 s without congestion and when the bigger picture would
//!   still get at least 0.10 bpp at the current target (the gap to the 0.07 floor is the
//!   hysteresis). A step down soon after a step up doubles the wait for the next step up (up
//!   to 60 s), so a link that cannot carry the bigger picture is not retried every ten seconds.
//! * **Decoder overload**: when the viewer's decoder needs more than 85 % of the frame interval
//!   twice in a row the frame rate is halved, and restored after 8 s without congestion.
//!
//! Structural changes (rungs, decoder frame rate) are at least 4 s apart.

use std::collections::VecDeque;
use std::time::Duration;

/// How often the encode thread ticks the controller.
pub(crate) const TICK: Duration = Duration::from_millis(250);

/// Window of the minimum-RTT ("base RTT") estimate.
const RTT_WINDOW: Duration = Duration::from_secs(10);
/// Queueing delay above base RTT that counts as mild congestion (plus half the base RTT).
const QUEUE_MILD: Duration = Duration::from_millis(12);
/// Queueing delay above base RTT that counts as severe congestion.
const QUEUE_SEVERE: Duration = Duration::from_millis(40);
/// Minimum time between two bitrate decreases (one feedback round trip).
const DECREASE_HOLD: Duration = Duration::from_millis(400);
/// No increase this soon after a decrease.
const INCREASE_HOLD: Duration = Duration::from_millis(1500);
/// Mild / severe multiplicative decrease, in per mille.
const MILD_DECREASE: u64 = 850;
const SEVERE_DECREASE: u64 = 600;
/// Minimum time between structural (rung / decoder frame rate) changes.
const STRUCTURAL_HOLD: Duration = Duration::from_secs(4);
/// Clear this long before the decoder-limited frame rate goes up again.
const FPS_UP_AFTER: Duration = Duration::from_secs(8);
/// The frame rate is never adapted below this.
const MIN_FPS: u32 = 15;

/// One step of the quality ladder: resolution scale (per mille of the capture cap, applied
/// to both dimensions) and frame rate (percent of the requested rate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rung {
    /// Per mille of the capture cap (834 turns 1920x1200 into 1600x1000).
    pub scale_permille: u32,
    /// Percent of the requested frame rate.
    pub fps_pct: u32,
}

const fn rung(scale_permille: u32, fps_pct: u32) -> Rung {
    Rung {
        scale_permille,
        fps_pct,
    }
}

/// The quality ladder, best first. Resolution drops first (text is mostly flat, so fewer
/// pixels is the cheapest way to save bits), then the frame rate, alternating so neither is
/// pushed to the extreme before the other has given something.
pub const LADDER: [Rung; 7] = [
    rung(1000, 100),
    rung(834, 100),
    rung(834, 75),
    rung(667, 75),
    rung(667, 50),
    rung(534, 50),
    rung(534, 25),
];

/// Below this many bits per pixel (at the current size and frame rate) text starts to smear.
pub const MIN_BPP: f64 = 0.07;
/// A bigger picture is only tried again if it would still get this many bits per pixel.
pub const UP_BPP: f64 = 0.10;
/// Under the floor and congested this long before the ladder moves.
const BELOW_FOR: Duration = Duration::from_millis(2500);
/// Congestion this recent still counts as "the link is the problem" for the ladder.
const RECENT_CONGESTION: Duration = Duration::from_secs(3);
/// Congestion-free time before the first step up.
const UP_AFTER: Duration = Duration::from_secs(10);
/// Longest wait before a step up after repeated failed attempts.
const UP_AFTER_MAX: Duration = Duration::from_secs(60);
/// A step down this soon after a step up counts as a failed attempt.
const FLAP_WINDOW: Duration = Duration::from_secs(30);

/// Bounds and mode of the controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerConfig {
    /// Bitrate floor, kbps.
    pub min_kbps: u32,
    /// Bitrate ceiling, kbps.
    pub max_kbps: u32,
    /// Frame rate the viewer asked for.
    pub max_fps: u32,
    /// `false`: fixed bitrate `max_kbps`, full frame rate and resolution, no adaptation.
    pub adaptive: bool,
    /// Pixels of a full-size frame (the capture cap), for bits-per-pixel; 0 disables the
    /// ladder.
    pub frame_pixels: u64,
}

impl ControllerConfig {
    /// Adaptive bounds for a ceiling: the floor is 1/16 of it (at least 300 kbps). The frame
    /// size defaults to the one a ceiling of 0.1 bpp implies; use
    /// [`ControllerConfig::with_frame_pixels`] for the real one.
    pub fn adaptive(max_kbps: u32, max_fps: u32) -> Self {
        let max_kbps = max_kbps.max(300);
        let max_fps = max_fps.max(1);
        Self {
            min_kbps: (max_kbps / 16).max(300).min(max_kbps),
            max_kbps,
            max_fps,
            adaptive: true,
            frame_pixels: u64::from(max_kbps) * 10_000 / u64::from(max_fps),
        }
    }

    /// A fixed bitrate (no adaptation).
    pub fn fixed(kbps: u32, fps: u32) -> Self {
        Self {
            min_kbps: kbps.max(1),
            max_kbps: kbps.max(1),
            max_fps: fps.max(1),
            adaptive: false,
            frame_pixels: 0,
        }
    }

    /// Sets the pixel count of a full-size frame.
    pub fn with_frame_pixels(mut self, pixels: u64) -> Self {
        self.frame_pixels = pixels;
        self
    }
}

/// What the viewer reported since the previous tick (`Message::ViewerFeedback`, merged).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ViewerReport {
    /// Average decode time per frame, microseconds.
    pub decode_us: u32,
    /// Frames the viewer lost or skipped.
    pub frames_dropped: u32,
    /// The viewer's view of the RTT, microseconds.
    pub rtt_us: u32,
}

/// Inputs of one controller tick.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Signals {
    /// Monotonic time (e.g. since the session started).
    pub at: Duration,
    /// Viewer feedback received since the previous tick, if any.
    pub viewer: Option<ViewerReport>,
    /// Frames `send_video` dropped since the previous tick.
    pub send_drops: u32,
    /// Video frames not yet acknowledged by the viewer, now.
    pub in_flight: usize,
    /// QUIC smoothed RTT (host side); zero if unknown.
    pub rtt: Duration,
    /// QUIC congestion events since the previous tick.
    pub congestion_events: u64,
    /// What QUIC actually sent since the previous tick, kbps (`None` if unknown).
    pub sent_kbps: Option<u32>,
}

/// The controller's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdaptDecision {
    /// Encoder target bitrate, kbps.
    pub bitrate_kbps: u32,
    /// Frames per second to encode.
    pub fps: u32,
    /// Capture cap scale, per mille (1000 = the cap the quality preset asked for).
    pub scale_permille: u32,
}

/// How congested the link looked during one tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Congestion {
    /// No sign of congestion.
    Clear,
    /// Queues are building: back off a little.
    Mild,
    /// Frames are being dropped or delay is high: back off hard.
    Severe,
}

/// AIMD controller with the quality ladder (see the module docs).
#[derive(Debug, Clone)]
pub struct Controller {
    cfg: ControllerConfig,
    target: u32,
    /// Frame rate limit from the viewer's decoder.
    dec_fps: u32,
    rung: usize,
    rtts: VecDeque<(Duration, Duration)>,
    last_at: Option<Duration>,
    last_decrease: Option<Duration>,
    last_structural: Option<Duration>,
    last_up: Option<Duration>,
    up_wait: Duration,
    clear_since: Option<Duration>,
    below_since: Option<Duration>,
    slow_decodes: u32,
    last_level: Congestion,
}

impl Controller {
    /// A controller starting at the ceiling (a LAN is usually fast; congestion is detected
    /// within one feedback round trip).
    pub fn new(cfg: ControllerConfig) -> Self {
        Self {
            cfg,
            target: cfg.max_kbps,
            dec_fps: cfg.max_fps,
            rung: 0,
            rtts: VecDeque::new(),
            last_at: None,
            last_decrease: None,
            last_structural: None,
            last_up: None,
            up_wait: UP_AFTER,
            clear_since: None,
            below_since: None,
            slow_decodes: 0,
            last_level: Congestion::Clear,
        }
    }

    /// New bounds (quality preset changed, or the encoder was rebuilt for another size).
    /// The current target is kept within the new bounds; a new frame-rate ceiling resets the
    /// decoder frame-rate limit.
    pub fn reconfigure(&mut self, cfg: ControllerConfig) {
        if cfg.max_fps != self.cfg.max_fps || !cfg.adaptive {
            self.dec_fps = cfg.max_fps;
        }
        if !cfg.adaptive || cfg.frame_pixels == 0 {
            self.rung = 0;
        }
        // Coming from a fixed bitrate, or running unconstrained at the ceiling: follow the new
        // ceiling. A target lowered by congestion stays where it is.
        if (!self.cfg.adaptive && cfg.adaptive) || self.target >= self.cfg.max_kbps {
            self.target = cfg.max_kbps;
        }
        self.cfg = cfg;
        self.target = self.target.clamp(cfg.min_kbps, cfg.max_kbps);
        self.dec_fps = self.dec_fps.clamp(1, cfg.max_fps);
    }

    /// The bounds in use.
    pub fn config(&self) -> ControllerConfig {
        self.cfg
    }

    /// The ladder rung in use (0 = full quality).
    pub fn rung(&self) -> usize {
        self.rung
    }

    fn rung_fps(&self, rung: usize) -> u32 {
        let pct = LADDER.get(rung).map_or(100, |r| r.fps_pct);
        (self.cfg.max_fps * pct / 100)
            .max(self.min_fps())
            .min(self.cfg.max_fps)
    }

    fn rung_scale(rung: usize) -> u32 {
        LADDER.get(rung).map_or(1000, |r| r.scale_permille)
    }

    /// Bits per pixel `kbps` buys at `rung`; infinite when the ladder is disabled.
    fn bpp(&self, kbps: u32, rung: usize) -> f64 {
        if self.cfg.frame_pixels == 0 {
            return f64::INFINITY;
        }
        let scale = f64::from(Self::rung_scale(rung)) / 1000.0;
        let pixels = self.cfg.frame_pixels as f64 * scale * scale;
        f64::from(kbps) * 1000.0 / (pixels * f64::from(self.rung_fps(rung)).max(1.0))
    }

    /// Floor and step-up thresholds, scaled down for ceilings that never reach the defaults
    /// (a "Speed" preset capped at 60 % must still be able to climb back).
    fn thresholds(&self) -> (f64, f64) {
        let reference = self.bpp(self.cfg.max_kbps, 0);
        (MIN_BPP.min(0.8 * reference), UP_BPP.min(0.95 * reference))
    }

    /// The current decision.
    pub fn decision(&self) -> AdaptDecision {
        if !self.cfg.adaptive {
            return AdaptDecision {
                bitrate_kbps: self.cfg.max_kbps,
                fps: self.cfg.max_fps,
                scale_permille: 1000,
            };
        }
        AdaptDecision {
            bitrate_kbps: self.target,
            fps: self.rung_fps(self.rung).min(self.dec_fps).max(1),
            scale_permille: Self::rung_scale(self.rung),
        }
    }

    /// Congestion level of the last tick.
    pub fn last_congestion(&self) -> Congestion {
        self.last_level
    }

    /// Base RTT: the minimum over the last 10 s.
    fn base_rtt(&self) -> Option<Duration> {
        self.rtts.iter().map(|&(_, r)| r).min()
    }

    fn classify(&mut self, s: &Signals) -> Congestion {
        let rtt = if s.rtt.is_zero() {
            s.viewer
                .map(|v| Duration::from_micros(u64::from(v.rtt_us)))
                .unwrap_or_default()
        } else {
            s.rtt
        };
        if !rtt.is_zero() {
            self.rtts.push_back((s.at, rtt));
            while self
                .rtts
                .front()
                .is_some_and(|&(t, _)| s.at.saturating_sub(t) > RTT_WINDOW)
            {
                self.rtts.pop_front();
            }
        }
        let queue = self
            .base_rtt()
            .map_or(Duration::ZERO, |b| rtt.saturating_sub(b));
        let mild_queue = QUEUE_MILD + self.base_rtt().unwrap_or_default() / 2;
        let dropped = s.viewer.map_or(0, |v| v.frames_dropped);
        let window_cut = s.congestion_events > 0 && (s.in_flight >= 1 || queue > mild_queue / 2);
        if s.send_drops > 0 || dropped > 2 || queue > QUEUE_SEVERE {
            Congestion::Severe
        } else if s.in_flight >= 2 || dropped > 0 || queue > mild_queue || window_cut {
            Congestion::Mild
        } else {
            Congestion::Clear
        }
    }

    /// Feeds one tick of signals and returns the new decision.
    pub fn tick(&mut self, s: &Signals) -> AdaptDecision {
        let dt = self
            .last_at
            .map_or(TICK, |t| s.at.saturating_sub(t))
            .min(Duration::from_secs(2));
        self.last_at = Some(s.at);
        let level = self.classify(s);
        self.last_level = level;
        if !self.cfg.adaptive {
            return self.decision();
        }
        self.adapt_bitrate(s, level, dt);
        self.adapt_structure(s, level);
        self.decision()
    }

    fn adapt_bitrate(&mut self, s: &Signals, level: Congestion, dt: Duration) {
        let (min, max) = (self.cfg.min_kbps, self.cfg.max_kbps);
        match level {
            Congestion::Clear => {
                self.clear_since.get_or_insert(s.at);
                let calm = self
                    .last_decrease
                    .is_none_or(|t| s.at.saturating_sub(t) >= INCREASE_HOLD);
                if calm {
                    let per_sec = u64::from(max) * 6 / 100 + u64::from(self.target) / 10;
                    let inc = per_sec * dt.as_millis() as u64 / 1000;
                    let next = u64::from(self.target).saturating_add(inc.max(1));
                    self.target = u32::try_from(next).unwrap_or(u32::MAX).min(max);
                }
            }
            Congestion::Mild | Congestion::Severe => {
                self.clear_since = None;
                let may = self
                    .last_decrease
                    .is_none_or(|t| s.at.saturating_sub(t) >= DECREASE_HOLD);
                if may {
                    let factor = if level == Congestion::Severe {
                        SEVERE_DECREASE
                    } else {
                        MILD_DECREASE
                    };
                    let mut next = u64::from(self.target) * factor / 1000;
                    if s.send_drops > 0 {
                        // Video did not fit: what actually got through is the best capacity
                        // estimate (on a mostly static screen the sent rate says nothing).
                        if let Some(sent) = s.sent_kbps.filter(|&k| k > min) {
                            next = next.min(u64::from(sent) * 85 / 100);
                        }
                    }
                    self.target = (next as u32).clamp(min, max);
                    self.last_decrease = Some(s.at);
                }
            }
        }
    }

    fn structural_allowed(&self, at: Duration) -> bool {
        self.last_structural
            .is_none_or(|t| at.saturating_sub(t) >= STRUCTURAL_HOLD)
    }

    fn min_fps(&self) -> u32 {
        MIN_FPS.min(self.cfg.max_fps)
    }

    fn adapt_structure(&mut self, s: &Signals, level: Congestion) {
        let can_change = self.structural_allowed(s.at);
        let since_change = self
            .last_structural
            .map_or(Duration::MAX, |t| s.at.saturating_sub(t));
        let clear_for = self
            .clear_since
            .map_or(Duration::ZERO, |t| s.at.saturating_sub(t));

        // Decoder overload: the viewer cannot decode this many frames per second.
        if let Some(v) = s.viewer {
            let budget = 1_000_000 / u64::from(self.dec_fps.max(1));
            if u64::from(v.decode_us) * 100 > budget * 85 {
                self.slow_decodes += 1;
            } else {
                self.slow_decodes = 0;
            }
        }
        if can_change && self.dec_fps > self.min_fps() && self.slow_decodes >= 2 {
            self.dec_fps = (self.dec_fps / 2).max(self.min_fps());
            self.slow_decodes = 0;
            self.last_structural = Some(s.at);
            return;
        }

        let (floor, up_bpp) = self.thresholds();
        // Ladder down: the target is so low that this rung would fall under the bpp floor.
        // (The AIMD sawtooth alternates clear and congested ticks, so "congested" means
        // "within the last few seconds".)
        let recently_congested = clear_for < RECENT_CONGESTION;
        if recently_congested && self.bpp(self.target, self.rung) < floor {
            self.below_since.get_or_insert(s.at);
        } else {
            self.below_since = None;
        }
        let below_for = self
            .below_since
            .map_or(Duration::ZERO, |t| s.at.saturating_sub(t));
        if can_change && below_for >= BELOW_FOR && self.rung + 1 < LADDER.len() {
            // The first rung that restores the floor with a margin, else the smallest.
            let next = (self.rung + 1..LADDER.len())
                .find(|&r| self.bpp(self.target, r) >= floor * 1.15)
                .unwrap_or(LADDER.len() - 1);
            self.rung = next;
            self.last_structural = Some(s.at);
            self.below_since = None;
            if self
                .last_up
                .is_some_and(|t| s.at.saturating_sub(t) <= FLAP_WINDOW)
            {
                self.up_wait = (self.up_wait * 2).min(UP_AFTER_MAX);
            }
            return;
        }
        if level != Congestion::Clear {
            return;
        }
        if since_change >= Duration::from_secs(120) {
            self.up_wait = UP_AFTER;
        }

        // Decoder-limited frame rate recovers on its own clock.
        let room = |pct: u64| u64::from(self.target) * 100 >= u64::from(self.cfg.max_kbps) * pct;
        if self.dec_fps < self.cfg.max_fps
            && clear_for >= FPS_UP_AFTER
            && since_change >= FPS_UP_AFTER
            && room(50)
        {
            let next = self.dec_fps.saturating_mul(2).min(self.cfg.max_fps);
            // Only if the viewer's decoder has headroom for the higher rate.
            let decode_ok = s
                .viewer
                .is_none_or(|v| u64::from(v.decode_us) * u64::from(next) < 600_000);
            if decode_ok {
                self.dec_fps = next;
                self.last_structural = Some(s.at);
                return;
            }
        }
        // Ladder up: calm for a while and the bigger picture still gets enough bits.
        if self.rung > 0
            && can_change
            && clear_for >= self.up_wait
            && since_change >= self.up_wait
            && self.bpp(self.target, self.rung - 1) >= up_bpp
        {
            self.rung -= 1;
            self.last_up = Some(s.at);
            self.last_structural = Some(s.at);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    fn clear(ms: u64) -> Signals {
        Signals {
            at: at(ms),
            rtt: at(1),
            ..Signals::default()
        }
    }

    #[test]
    fn bounds_for_adaptive_and_fixed() {
        let c = ControllerConfig::adaptive(16_000, 60);
        assert_eq!((c.min_kbps, c.max_kbps), (1_000, 16_000));
        let c = ControllerConfig::adaptive(100, 60);
        assert_eq!((c.min_kbps, c.max_kbps), (300, 300));
        let mut ctl = Controller::new(ControllerConfig::fixed(4_000, 30));
        // A fixed bitrate never moves, whatever happens.
        for i in 0..20 {
            let d = ctl.tick(&Signals {
                at: at(i * 250),
                send_drops: 5,
                in_flight: 6,
                rtt: at(200),
                ..Signals::default()
            });
            assert_eq!(
                d,
                AdaptDecision {
                    bitrate_kbps: 4_000,
                    fps: 30,
                    scale_permille: 1000
                }
            );
        }
    }

    #[test]
    fn decreases_are_rate_limited_and_increases_wait() {
        let mut ctl = Controller::new(ControllerConfig::adaptive(10_000, 60));
        ctl.tick(&clear(0));
        let drop = |ms| Signals {
            at: at(ms),
            send_drops: 1,
            rtt: at(1),
            ..Signals::default()
        };
        assert_eq!(ctl.tick(&drop(250)).bitrate_kbps, 6_000);
        // Same congestion episode 250 ms later: held.
        assert_eq!(ctl.tick(&drop(500)).bitrate_kbps, 6_000);
        assert_eq!(ctl.tick(&drop(750)).bitrate_kbps, 3_600);
        // Clear, but within the increase hold: unchanged.
        assert_eq!(ctl.tick(&clear(1000)).bitrate_kbps, 3_600);
        assert_eq!(ctl.tick(&clear(2000)).bitrate_kbps, 3_600);
        assert!(ctl.tick(&clear(2500)).bitrate_kbps > 3_600);
    }

    #[test]
    fn severe_congestion_caps_at_what_was_sent() {
        let mut ctl = Controller::new(ControllerConfig::adaptive(20_000, 60));
        let d = ctl.tick(&Signals {
            at: at(0),
            send_drops: 3,
            sent_kbps: Some(5_000),
            ..Signals::default()
        });
        assert_eq!(d.bitrate_kbps, 4_250);
    }

    #[test]
    fn rtt_growth_over_base_is_congestion() {
        let mut ctl = Controller::new(ControllerConfig::adaptive(10_000, 60));
        for i in 0..8 {
            ctl.tick(&Signals {
                at: at(i * 250),
                rtt: at(2),
                ..Signals::default()
            });
        }
        assert_eq!(ctl.last_congestion(), Congestion::Clear);
        let rise = |ms, rtt| Signals {
            at: at(ms),
            rtt: at(rtt),
            ..Signals::default()
        };
        ctl.tick(&rise(2000, 16));
        assert_eq!(ctl.last_congestion(), Congestion::Mild);
        ctl.tick(&rise(2250, 60));
        assert_eq!(ctl.last_congestion(), Congestion::Severe);
        // Viewer RTT is used when the host has none.
        let mut ctl = Controller::new(ControllerConfig::adaptive(10_000, 60));
        ctl.tick(&Signals {
            at: at(0),
            viewer: Some(ViewerReport {
                rtt_us: 1_000,
                ..ViewerReport::default()
            }),
            ..Signals::default()
        });
        ctl.tick(&Signals {
            at: at(250),
            viewer: Some(ViewerReport {
                rtt_us: 80_000,
                ..ViewerReport::default()
            }),
            ..Signals::default()
        });
        assert_eq!(ctl.last_congestion(), Congestion::Severe);
    }

    #[test]
    fn window_cuts_count_only_while_video_waits() {
        let mut ctl = Controller::new(ControllerConfig::adaptive(10_000, 60));
        // Random Wi-Fi loss made QUIC cut its window, but no frame is waiting: ignored.
        let d = ctl.tick(&Signals {
            at: at(0),
            rtt: at(2),
            congestion_events: 1,
            sent_kbps: Some(500),
            ..Signals::default()
        });
        assert_eq!(ctl.last_congestion(), Congestion::Clear);
        assert_eq!(d.bitrate_kbps, 10_000);
        // A frame is waiting: mild, and no sent-rate cap without drops.
        let d = ctl.tick(&Signals {
            at: at(250),
            rtt: at(2),
            in_flight: 1,
            congestion_events: 1,
            sent_kbps: Some(1_000),
            ..Signals::default()
        });
        assert_eq!(ctl.last_congestion(), Congestion::Mild);
        assert_eq!(d.bitrate_kbps, 8_500);
    }

    #[test]
    fn slow_decoder_halves_the_frame_rate() {
        let mut ctl = Controller::new(ControllerConfig::adaptive(10_000, 60));
        let slow = |ms| Signals {
            at: at(ms),
            rtt: at(1),
            viewer: Some(ViewerReport {
                decode_us: 25_000,
                ..ViewerReport::default()
            }),
            ..Signals::default()
        };
        assert_eq!(ctl.tick(&slow(0)).fps, 60);
        assert_eq!(ctl.tick(&slow(250)).fps, 30);
        // 25 ms fits 30 fps (33 ms): stays at 30 and does not go back to 60 either.
        for i in 2..80 {
            assert_eq!(ctl.tick(&slow(i * 250)).fps, 30);
        }
    }

    #[test]
    fn reconfigure_keeps_target_within_new_bounds() {
        let mut ctl = Controller::new(ControllerConfig::adaptive(20_000, 60));
        ctl.reconfigure(ControllerConfig::adaptive(5_000, 60));
        assert_eq!(ctl.decision().bitrate_kbps, 5_000);
        // Unconstrained at the ceiling: follows a higher ceiling.
        ctl.reconfigure(ControllerConfig::adaptive(6_000, 60));
        assert_eq!(ctl.decision().bitrate_kbps, 6_000);
        // Lowered by congestion: stays put when the ceiling rises.
        ctl.tick(&Signals {
            at: at(0),
            send_drops: 1,
            ..Signals::default()
        });
        assert_eq!(ctl.decision().bitrate_kbps, 3_600);
        ctl.reconfigure(ControllerConfig::adaptive(8_000, 30));
        assert_eq!(ctl.decision().bitrate_kbps, 3_600);
        assert_eq!(ctl.decision().fps, 30);
        ctl.reconfigure(ControllerConfig::fixed(2_000, 24));
        assert_eq!(ctl.decision().bitrate_kbps, 2_000);
        ctl.reconfigure(ControllerConfig::adaptive(8_000, 60));
        assert_eq!(ctl.decision().bitrate_kbps, 8_000);
        assert_eq!(ctl.config().max_fps, 60);
    }
}
