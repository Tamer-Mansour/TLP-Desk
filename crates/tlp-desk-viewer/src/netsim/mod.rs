//! **Development only**: an impaired link for the video a viewer receives, so the host's
//! adaptive controller (bitrate, quality ladder, refresh pictures) can be exercised on one PC
//! over loopback.
//!
//! Nothing turns this on by itself: a caller must pass a [`NetSim`] to
//! [`crate::ViewerSession::set_net_sim`] (the CLI does so only for its hidden, dev-only
//! `connect --sim-loss / --sim-bandwidth-kbps` flags). It only throws away video this viewer
//! already received, exactly as a congested link would have, so the host sees lost frames in
//! `ViewerFeedback` and reacts as it does on a real network. It never touches what the viewer
//! sends, authentication or permissions.
//!
//! Model: a link of [`NetSim::bandwidth_kbps`] with a queue of at most [`MAX_QUEUE`]: a frame
//! is accepted when the queue in front of it is short enough (whatever its own size, so big
//! keyframes still get through) and then occupies the link for its serialization time; frames
//! arriving while the queue is full are dropped (tail drop). On top of that each frame is lost
//! with probability [`NetSim::loss_pct`]. Accepted frames are delivered at once: the queueing
//! delay itself is not added, only its consequence (loss).

use std::time::{Duration, Instant};

/// Longest queue in front of the simulated link before frames are dropped.
pub const MAX_QUEUE: Duration = Duration::from_millis(200);

/// How the simulated link degrades the video (development only, see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NetSim {
    /// Percentage (0-100) of video frames lost at random.
    pub loss_pct: u8,
    /// Link capacity for video in kbit/s; 0 = unlimited.
    pub bandwidth_kbps: u32,
}

impl NetSim {
    /// Whether this impairs anything at all.
    pub fn is_active(&self) -> bool {
        self.loss_pct > 0 || self.bandwidth_kbps > 0
    }
}

/// What happens to one received frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Delivered to the decoder.
    Deliver,
    /// Lost on the simulated link.
    Drop,
}

/// The simulated link's state for one session.
#[derive(Debug, Clone)]
pub(crate) struct LinkSim {
    cfg: NetSim,
    /// When the link has sent everything accepted so far.
    free_at: Option<Instant>,
    /// xorshift64 state for random loss (seeded per session; never zero).
    rng: u64,
}

impl LinkSim {
    pub(crate) fn new(cfg: NetSim, seed: u64) -> Self {
        Self {
            cfg,
            free_at: None,
            rng: seed | 1,
        }
    }

    pub(crate) fn config(&self) -> NetSim {
        self.cfg
    }

    fn next_pct(&mut self) -> u64 {
        // xorshift64: plenty for spreading simulated losses, never used for anything secret.
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x % 100
    }

    /// Decides the fate of a frame of `bytes` bytes arriving at `now`.
    pub(crate) fn admit(&mut self, bytes: usize, now: Instant) -> Verdict {
        if self.cfg.loss_pct > 0 && self.next_pct() < u64::from(self.cfg.loss_pct.min(100)) {
            return Verdict::Drop;
        }
        if self.cfg.bandwidth_kbps == 0 {
            return Verdict::Deliver;
        }
        let start = self.free_at.map_or(now, |f| f.max(now));
        if start.saturating_duration_since(now) > MAX_QUEUE {
            return Verdict::Drop;
        }
        // bits / (kbit/s) = milliseconds; in microseconds: bytes * 8 * 1000 / kbps.
        let bits = u64::try_from(bytes).unwrap_or(u64::MAX).saturating_mul(8);
        let us = bits.saturating_mul(1000) / u64::from(self.cfg.bandwidth_kbps.max(1));
        self.free_at = Some(start + Duration::from_micros(us));
        Verdict::Deliver
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn inactive_sim_delivers_everything() {
        let mut s = LinkSim::new(NetSim::default(), 7);
        assert!(!NetSim::default().is_active());
        let t0 = Instant::now();
        for i in 0..1000u64 {
            assert_eq!(s.admit(1 << 20, t0 + ms(i)), Verdict::Deliver);
        }
    }

    #[test]
    fn bandwidth_limit_drops_what_does_not_fit_and_keeps_what_does() {
        // 8000 kbit/s = 1000 bytes per millisecond.
        let cfg = NetSim {
            loss_pct: 0,
            bandwidth_kbps: 8_000,
        };
        assert!(cfg.is_active());
        let mut s = LinkSim::new(cfg, 1);
        let t0 = Instant::now();
        // 60 fps frames of 25 kB need 25 ms of a 16.7 ms frame interval: the queue grows by
        // ~8 ms per frame and frames start to drop once it passes 200 ms.
        let mut delivered = 0;
        for i in 0..600u64 {
            if s.admit(25_000, t0 + Duration::from_micros(i * 16_667)) == Verdict::Deliver {
                delivered += 1;
            }
        }
        // Ten seconds at 8 Mbit/s carry 400 frames of 25 kB (plus the 200 ms queue).
        assert!((395..=410).contains(&delivered), "{delivered}");

        // Frames that fit the link are never dropped.
        let mut s = LinkSim::new(cfg, 1);
        for i in 0..600u64 {
            assert_eq!(
                s.admit(10_000, t0 + Duration::from_micros(i * 16_667)),
                Verdict::Deliver
            );
        }
    }

    #[test]
    fn a_big_keyframe_gets_through_an_empty_queue() {
        let cfg = NetSim {
            loss_pct: 0,
            bandwidth_kbps: 1_000,
        };
        let mut s = LinkSim::new(cfg, 3);
        let t0 = Instant::now();
        // 500 kB at 1 Mbit/s is 4 s of link time: accepted, then the link is busy.
        assert_eq!(s.admit(500_000, t0), Verdict::Deliver);
        assert_eq!(s.admit(100, t0 + ms(500)), Verdict::Drop);
        // Once it has drained, frames pass again.
        assert_eq!(s.admit(100, t0 + ms(3_900)), Verdict::Deliver);
    }

    #[test]
    fn random_loss_matches_the_percentage() {
        let cfg = NetSim {
            loss_pct: 10,
            bandwidth_kbps: 0,
        };
        let mut s = LinkSim::new(cfg, 0x5eed);
        let t0 = Instant::now();
        let dropped = (0..10_000)
            .filter(|_| s.admit(1_000, t0) == Verdict::Drop)
            .count();
        assert!((800..=1_200).contains(&dropped), "{dropped}");
        // 100 % loses everything; values above 100 are treated as 100.
        let mut all = LinkSim::new(
            NetSim {
                loss_pct: 250,
                bandwidth_kbps: 0,
            },
            9,
        );
        assert!((0..100).all(|_| all.admit(1, t0) == Verdict::Drop));
    }
}
