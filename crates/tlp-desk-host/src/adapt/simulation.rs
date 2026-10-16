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
    // 3 Mbps is above the floor: no need to give up frame rate or resolution.
    assert!(late.iter().all(|t| t.decision.scale_permille == 1000));
}

#[test]
fn recovers_to_full_quality_after_congestion_ends() {
    let mut ctl = Controller::new(ControllerConfig::adaptive(MAX, 60));
    let mut link = Link::new(100_000.0, 0.5);
    let trace = run(&mut ctl, &mut link, Duration::ZERO, 70, |t| {
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
    assert_eq!(worst.scale_permille, 500, "{worst:?}");
    // Frame rate went down before resolution, one step at a time.
    let first_scale = trace
        .iter()
        .position(|t| t.decision.scale_permille < 1000)
        .unwrap();
    assert!(trace[..first_scale].iter().any(|t| t.decision.fps == 15));
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
