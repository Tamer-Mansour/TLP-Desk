//! Controller simulations against synthetic network traces (unit tests only).
//!
//! The link is a single bottleneck queue: the encoder sends at the controller's bitrate, the
//! link drains `capacity` kbps, the queue adds delay to the RTT, and the host's
//! drop-instead-of-queue rule (3 frames in flight) bounds the queue by dropping frames, which
//! the viewer then reports as lost. Randomness (Wi-Fi jitter and delay spikes) is seeded.

use std::time::Duration;

use super::controller::{AdaptDecision, Controller, ControllerConfig, Signals, ViewerReport, TICK};

/// xorshift64*: deterministic, good enough for jitter.
struct Rng(u64);

impl Rng {
    fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
}

struct Link {
    capacity_kbps: f64,
    base_rtt_ms: f64,
    /// Uniform extra delay 0..jitter on each RTT sample.
    jitter_ms: f64,
    /// Probability per tick of a delay spike (Wi-Fi retransmissions, a neighbour's burst).
    spike_prob: f64,
    spike_ms: f64,
    queue_kbit: f64,
    rng: Rng,
}

/// What one tick looked like (for assertions).
#[derive(Debug, Clone, Copy)]
struct Tick {
    at: Duration,
    decision: AdaptDecision,
    queue_ms: f64,
    drops: u32,
}

impl Link {
    fn new(capacity_kbps: f64, base_rtt_ms: f64) -> Self {
        Self {
            capacity_kbps,
            base_rtt_ms,
            jitter_ms: 0.0,
            spike_prob: 0.0,
            spike_ms: 0.0,
            queue_kbit: 0.0,
            rng: Rng(0x5EED_1234_ABCD_0001),
        }
    }

    fn wifi(mut self, jitter_ms: f64, spike_prob: f64, spike_ms: f64) -> Self {
        self.jitter_ms = jitter_ms;
        self.spike_prob = spike_prob;
        self.spike_ms = spike_ms;
        self
    }

    /// Sends at `d` for one tick; returns the controller's next inputs and the queue delay.
    fn step(&mut self, at: Duration, d: AdaptDecision) -> (Signals, f64, u32) {
        let dt = TICK.as_secs_f64();
        let send = f64::from(d.bitrate_kbps);
        let before = self.queue_kbit;
        self.queue_kbit = (self.queue_kbit + (send - self.capacity_kbps) * dt).max(0.0);
        let drained = (before + send * dt - self.queue_kbit) / dt;
        // Drop-instead-of-queue: at most 3 frames may be in flight.
        let frame_kbit = send / f64::from(d.fps.max(1));
        let max_queue = 3.0 * frame_kbit;
        let mut drops = 0u32;
        if self.queue_kbit > max_queue {
            drops = ((self.queue_kbit - max_queue) / frame_kbit.max(1e-9)).ceil() as u32;
            self.queue_kbit = max_queue;
        }
        let queue_ms = self.queue_kbit / self.capacity_kbps * 1000.0;
        let mut rtt = self.base_rtt_ms + queue_ms + self.rng.next_f64() * self.jitter_ms;
        if self.rng.next_f64() < self.spike_prob {
            rtt += self.spike_ms;
        }
        let frame_ms = 1000.0 / f64::from(d.fps.max(1));
        let in_flight = ((queue_ms + self.base_rtt_ms) / frame_ms).floor() as usize;
        let signals = Signals {
            at,
            viewer: Some(ViewerReport {
                decode_us: 4_000,
                frames_dropped: drops,
                rtt_us: (rtt * 1000.0) as u32,
            }),
            send_drops: drops,
            in_flight,
            rtt: Duration::from_secs_f64(rtt / 1000.0),
            congestion_events: 0,
            sent_kbps: Some(drained.min(self.capacity_kbps) as u32),
        };
        (signals, queue_ms, drops)
    }
}

/// Runs `secs` seconds; `capacity(t)` may change the link over time.
fn run(
    ctl: &mut Controller,
    link: &mut Link,
    from: Duration,
    secs: u64,
    mut capacity: impl FnMut(Duration) -> Option<f64>,
) -> Vec<Tick> {
    let ticks = secs * 1000 / TICK.as_millis() as u64;
    let mut out = Vec::new();
    let mut d = ctl.decision();
    for i in 0..ticks {
        let at = from + TICK * (i as u32);
        if let Some(c) = capacity(at) {
            link.capacity_kbps = c;
        }
        let (s, queue_ms, drops) = link.step(at, d);
        d = ctl.tick(&s);
        out.push(Tick {
            at,
            decision: d,
            queue_ms,
            drops,
        });
    }
    out
}

fn after(trace: &[Tick], secs: u64) -> impl Iterator<Item = &Tick> {
    trace
        .iter()
        .filter(move |t| t.at >= Duration::from_secs(secs))
}

const MAX: u32 = 14_000;

#[test]
fn good_lan_runs_at_the_ceiling_untouched() {
    let mut ctl = Controller::new(ControllerConfig::adaptive(MAX, 60));
    let mut link = Link::new(900_000.0, 0.3).wifi(0.2, 0.0, 0.0);
    let trace = run(&mut ctl, &mut link, Duration::ZERO, 60, |_| None);
    assert!(trace.iter().all(|t| t.decision
        == AdaptDecision {
            bitrate_kbps: MAX,
            fps: 60,
            scale_permille: 1000
        }));
    assert!(trace.iter().all(|t| t.drops == 0 && t.queue_ms < 0.1));
}

#[test]
fn wifi_with_jitter_and_spikes_stays_high_without_collapsing() {
    let mut ctl = Controller::new(ControllerConfig::adaptive(MAX, 60));
    // 40 Mbps of Wi-Fi, 3 ms base RTT, up to 8 ms jitter, a 25 ms spike in 5 % of the ticks.
    let mut link = Link::new(40_000.0, 3.0).wifi(8.0, 0.05, 25.0);
    let trace = run(&mut ctl, &mut link, Duration::ZERO, 120, |_| None);
    let tail: Vec<_> = after(&trace, 30).collect();
    let avg = tail
        .iter()
        .map(|t| f64::from(t.decision.bitrate_kbps))
        .sum::<f64>()
        / tail.len() as f64;
    assert!(avg >= 0.6 * f64::from(MAX), "average {avg:.0} kbps");
    // Spikes are not a reason to lower frame rate or resolution.
    assert!(trace
        .iter()
        .all(|t| t.decision.fps == 60 && t.decision.scale_permille == 1000));
    assert!(trace.iter().all(|t| t.drops == 0));
}

#[test]
fn sudden_congestion_backs_off_fast_and_keeps_latency_bounded() {
    let mut ctl = Controller::new(ControllerConfig::adaptive(MAX, 60));
    let mut link = Link::new(100_000.0, 0.5);
    let drop_at = Duration::from_secs(20);
    let trace = run(&mut ctl, &mut link, Duration::ZERO, 40, |t| {
        (t >= drop_at).then_some(3_000.0)
    });
    // Before: full rate.
    assert!(trace
        .iter()
        .filter(|t| t.at < drop_at)
        .all(|t| t.decision.bitrate_kbps == MAX));
    // Within 1.5 s the target is below the new capacity.
    let settled = trace
        .iter()
        .find(|t| t.at >= drop_at && t.decision.bitrate_kbps <= 3_000)
        .expect("never backed off");
    assert!(
        settled.at - drop_at <= Duration::from_millis(1500),
        "took {:?}",
        settled.at - drop_at
    );
    // Afterwards the queue never builds up: latency does not grow.
    let late: Vec<_> = after(&trace, 23).collect();
    let worst = late.iter().map(|t| t.queue_ms).fold(0.0, f64::max);
    assert!(worst < 60.0, "queue delay reached {worst:.1} ms");
    let mean = late.iter().map(|t| t.queue_ms).sum::<f64>() / late.len() as f64;
    assert!(mean < 15.0, "mean queue delay {mean:.1} ms");
    // The link is still used well (AIMD sawtooth around capacity).
    let avg = late
        .iter()
        .map(|t| f64::from(t.decision.bitrate_kbps))
        .sum::<f64>()
        / late.len() as f64;
    assert!(avg >= 1_500.0, "average {avg:.0} kbps on a 3 Mbps link");
    // 3 Mbps is far under the bits-per-pixel floor of a ~2.3 Mpx frame at 60 fps (0.02 bpp):
    // the picture gets smaller instead of blurrier.
    assert!(
        late.last().unwrap().decision.scale_permille < 1000,
        "never stepped the resolution down"
    );
}

#[test]
fn recovers_to_full_quality_after_congestion_ends() {
    let mut ctl = Controller::new(ControllerConfig::adaptive(MAX, 60));
    let mut link = Link::new(100_000.0, 0.5);
    let trace = run(&mut ctl, &mut link, Duration::ZERO, 160, |t| {
        let s = t.as_secs();
        Some(if (20..40).contains(&s) {
            3_000.0
        } else {
            100_000.0
        })
    });
    let back = trace
        .iter()
        .find(|t| t.at >= Duration::from_secs(40) && t.decision.bitrate_kbps >= MAX * 9 / 10)
        .expect("never recovered");
    assert!(
        back.at <= Duration::from_secs(60),
        "recovered only at {:?}",
        back.at
    );
    let last = trace.last().unwrap().decision;
    assert_eq!(last.fps, 60);
    assert_eq!(last.scale_permille, 1000);
}

#[test]
fn starved_link_lowers_fps_then_resolution_and_recovers() {
    let mut ctl = Controller::new(ControllerConfig::adaptive(MAX, 60));
    let mut link = Link::new(100_000.0, 0.5);
    // 400 kbps is below the bitrate floor (875 kbps): only fewer frames and pixels help.
    let trace = run(&mut ctl, &mut link, Duration::ZERO, 40, |_| Some(400.0));
    let worst = trace.last().unwrap().decision;
    assert_eq!(worst.fps, 15, "{worst:?}");
    assert_eq!(worst.scale_permille, 534, "{worst:?}");
    // Resolution and frame rate both stepped down, never faster than every 4 s.
    assert_structural_spacing(&trace);
    assert!(trace.iter().any(|t| t.decision.scale_permille < 1000));
    // Latency stays bounded by the in-flight limit even when starved (the first ticks still
    // drain the three full-rate frames that were in flight when the link collapsed).
    assert!(after(&trace, 2).all(|t| t.queue_ms < 600.0));
    let late_worst = after(&trace, 30).map(|t| t.queue_ms).fold(0.0, f64::max);
    let early_worst = after(&trace, 2)
        .filter(|t| t.at < Duration::from_secs(10))
        .map(|t| t.queue_ms)
        .fold(0.0, f64::max);
    assert!(
        late_worst <= early_worst,
        "latency grew: {early_worst:.0} -> {late_worst:.0} ms"
    );

    // The link comes back: full quality within a minute.
    let trace = run(&mut ctl, &mut link, Duration::from_secs(40), 90, |_| {
        Some(100_000.0)
    });
    let ok = trace
        .iter()
        .find(|t| {
            t.decision
                == AdaptDecision {
                    bitrate_kbps: MAX,
                    fps: 60,
                    scale_permille: 1000,
                }
        })
        .expect("never recovered");
    assert!(ok.at <= Duration::from_secs(40 + 60), "at {:?}", ok.at);
}

/// 1920x1200 frames at 60 fps, the ceiling at 0.12 bpp (the `for_screen` default).
const FRAME_PX: u64 = 1920 * 1200;
const CEILING: u32 = 16_589;

fn ladder_config() -> ControllerConfig {
    ControllerConfig::adaptive(CEILING, 60).with_frame_pixels(FRAME_PX)
}

/// Bits per pixel the stream gets at a decision.
fn bpp_of(d: AdaptDecision) -> f64 {
    let scale = f64::from(d.scale_permille) / 1000.0;
    f64::from(d.bitrate_kbps) * 1000.0 / (FRAME_PX as f64 * scale * scale * f64::from(d.fps))
}

/// Typical H.264 QP for text-heavy screens at a bit budget, fitted to the codec benchmark
/// (`bench --quality`: QP 12 at 0.2 bpp, 17 at 0.1, 22.5 at 0.04, 27 at 0.02: about 4.5 QP
/// per halving), capped by the encoder's quality floor of 32.
fn modelled_qp(bpp: f64) -> f64 {
    (12.0 + 4.5 * (0.2 / bpp.max(1e-4)).log2()).clamp(12.0, 32.0)
}

fn changes(trace: &[Tick]) -> Vec<(Duration, AdaptDecision)> {
    let mut out = Vec::new();
    let mut prev: Option<AdaptDecision> = None;
    for t in trace {
        let key = |d: AdaptDecision| (d.scale_permille, d.fps);
        if prev.is_some_and(|p| key(p) != key(t.decision)) {
            out.push((t.at, t.decision));
        }
        prev = Some(t.decision);
    }
    out
}

fn assert_structural_spacing(trace: &[Tick]) {
    let ch = changes(trace);
    for w in ch.windows(2) {
        assert!(
            w[1].0 - w[0].0 >= Duration::from_secs(4),
            "structural changes {:?} and {:?} are too close",
            w[0],
            w[1]
        );
    }
}

#[test]
fn congestion_lowers_the_resolution_and_keeps_the_quantiser_low() {
    let mut ctl = Controller::new(ladder_config());
    let mut link = Link::new(100_000.0, 0.5);
    let drop_at = Duration::from_secs(20);
    // 100 Mbps, then 2.5 Mbps: 0.018 bpp at 1920x1200@60, far under the 0.07 floor.
    let trace = run(&mut ctl, &mut link, Duration::ZERO, 90, |t| {
        (t >= drop_at).then_some(2_500.0)
    });
    assert!(trace
        .iter()
        .filter(|t| t.at < drop_at)
        .all(|t| t.decision.scale_permille == 1000));
    // Within 15 s the picture is smaller (1280x800 or less) ...
    let settled: Vec<_> = after(&trace, 35).collect();
    assert!(
        settled.iter().all(|t| t.decision.scale_permille <= 667),
        "{:?}",
        settled.first()
    );
    // ... and from then on the stream never goes under the bits-per-pixel floor by much,
    // so the quantiser stays well below the ceiling of 32.
    let worst_bpp = settled
        .iter()
        .map(|t| bpp_of(t.decision))
        .fold(f64::MAX, f64::min);
    assert!(worst_bpp >= 0.055, "worst bpp {worst_bpp:.3}");
    let worst_qp = settled
        .iter()
        .map(|t| modelled_qp(bpp_of(t.decision)))
        .fold(0.0, f64::max);
    assert!(worst_qp <= 24.0, "modelled QP {worst_qp:.1}");
    // Without the ladder the same link would have meant 0.018 bpp.
    assert!(modelled_qp(0.018) > 27.0);
    assert_structural_spacing(&trace);
    // Latency still bounded.
    assert!(after(&trace, 25).all(|t| t.queue_ms < 300.0));
    // The picture went down in few steps, not a staircase of one rung per second.
    assert!(changes(&trace).len() <= 4, "{:?}", changes(&trace));
}

#[test]
fn recovery_steps_back_up_slowly_without_flapping() {
    let mut ctl = Controller::new(ladder_config());
    let mut link = Link::new(100_000.0, 0.5);
    let trace = run(&mut ctl, &mut link, Duration::ZERO, 200, |t| {
        let s = t.as_secs();
        Some(if (20..80).contains(&s) {
            2_500.0
        } else {
            100_000.0
        })
    });
    assert_structural_spacing(&trace);
    let ch = changes(&trace);
    // Down at the congestion, then only ups: no step down after the first step up.
    let first_up = ch
        .iter()
        .position(|(at, _)| *at >= Duration::from_secs(80))
        .expect("never stepped back up");
    let ups = &ch[first_up..];
    assert!(ups
        .iter()
        .all(|(_, d)| d.scale_permille >= 667 || d.fps >= 30));
    // Steps up are at least 10 s apart (the hysteresis wait).
    for w in ups.windows(2) {
        assert!(w[1].0 - w[0].0 >= Duration::from_secs(10), "{w:?}");
    }
    // Back to full quality within a couple of minutes.
    let last = trace.last().unwrap().decision;
    assert_eq!((last.scale_permille, last.fps), (1000, 60), "{last:?}");
    assert!(last.bitrate_kbps >= CEILING * 9 / 10);
}

#[test]
fn a_borderline_link_settles_instead_of_flapping() {
    // 6 Mbps carries 1600x1000@60 at 0.062 bpp ... but not 1920x1200: the controller must find
    // a rung and stay, not retry the bigger picture every ten seconds.
    let mut ctl = Controller::new(ladder_config());
    let mut link = Link::new(6_000.0, 1.0).wifi(2.0, 0.03, 15.0);
    let trace = run(&mut ctl, &mut link, Duration::ZERO, 600, |_| None);
    let ch = changes(&trace);
    let late = ch
        .iter()
        .filter(|(at, _)| *at >= Duration::from_secs(180))
        .count();
    assert!(ch.len() <= 8, "{} structural changes: {ch:?}", ch.len());
    assert!(late <= 2, "{late} changes after settling: {ch:?}");
    assert_structural_spacing(&trace);
    // Quality stays above the floor most of the time.
    let tail: Vec<_> = after(&trace, 60).collect();
    let low = tail.iter().filter(|t| bpp_of(t.decision) < 0.05).count();
    assert!(
        low * 10 <= tail.len(),
        "{low} of {} ticks under 0.05 bpp",
        tail.len()
    );
}

#[test]
fn a_capped_ceiling_still_climbs_back() {
    // The Speed preset's ceiling is only 60 % of the default bitrate: the thresholds scale down
    // with it, so the ladder still steps up after congestion ends.
    let cfg = ControllerConfig::adaptive(CEILING * 6 / 10, 60).with_frame_pixels(FRAME_PX / 2);
    let mut ctl = Controller::new(cfg);
    let mut link = Link::new(100_000.0, 0.5);
    let trace = run(&mut ctl, &mut link, Duration::ZERO, 160, |t| {
        let s = t.as_secs();
        Some(if (10..50).contains(&s) {
            800.0
        } else {
            100_000.0
        })
    });
    assert!(trace.iter().any(|t| t.decision.scale_permille < 1000));
    assert_eq!(trace.last().unwrap().decision.scale_permille, 1000);
}
