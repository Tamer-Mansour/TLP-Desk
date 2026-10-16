//! Viewer-side jitter buffer: reorders packets, hides small network hiccups and keeps the
//! added latency bounded.
//!
//! The buffer works in whole 10 ms frames identified by a sequence number. The playback side
//! calls [`JitterBuffer::pop`] once per frame it needs (driven by the output device's clock);
//! the network side calls [`JitterBuffer::push`] whenever a packet arrives.
//!
//! * **Start / re-buffer**: nothing plays until `target_frames` are queued (30 ms default).
//! * **Late** packets (older than what already played) and duplicates are dropped.
//! * **Lost** packets (a gap with newer packets queued) become one concealment frame.
//! * **Latency bound**: more than `max_frames` queued drops the oldest; latency never grows.
//! * **Clock drift**: the host's and viewer's sound cards never run at exactly the same
//!   rate. If the queue stays above target for `drift_window` frames one frame is skipped; if
//!   it stays below, one concealment frame is inserted (the "duplicate 10 ms" case).

use std::collections::BTreeMap;

/// Tuning of a [`JitterBuffer`], in 10 ms frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JitterConfig {
    /// Frames queued before playback (re)starts. Also the level drift control aims for.
    pub target_frames: usize,
    /// Most frames ever queued; the oldest beyond it are dropped (latency bound).
    pub max_frames: usize,
    /// Consecutive frames the queue must stay off target before drift is corrected.
    pub drift_window: u32,
    /// Consecutive empty pops concealed before playback stops and re-buffers.
    pub max_conceal: u32,
}

impl Default for JitterConfig {
    /// 30 ms target, 60 ms ceiling: LAN jitter is small, and the whole audio path then
    /// stays well inside the 60 ms latency budget.
    fn default() -> Self {
        Self {
            target_frames: 3,
            max_frames: 6,
            drift_window: 100,
            max_conceal: 3,
        }
    }
}

/// What the player should output for the next 10 ms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Playout {
    /// Decode this packet.
    Packet(Vec<u8>),
    /// A packet is missing (lost, late or drift stretch): run packet loss concealment.
    Conceal,
    /// Nothing to play (not started, re-buffering, or the host went silent).
    Silence,
}

/// Counters for diagnostics and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JitterStats {
    /// Packets accepted into the queue.
    pub received: u64,
    /// Packets handed out for decoding.
    pub played: u64,
    /// Packets that arrived after their slot was played or concealed.
    pub late: u64,
    /// Packets with a sequence number already queued.
    pub duplicate: u64,
    /// Packets dropped because the queue exceeded `max_frames`.
    pub overflow: u64,
    /// Frames skipped to correct a fast sender clock.
    pub drift_skipped: u64,
    /// Concealment frames inserted to correct a slow sender clock.
    pub drift_stretched: u64,
    /// Concealment frames for missing packets.
    pub concealed: u64,
    /// Times playback ran dry and re-buffered.
    pub underruns: u64,
}

/// See the [module docs](self).
#[derive(Debug, Clone)]
pub struct JitterBuffer {
    cfg: JitterConfig,
    packets: BTreeMap<u64, Vec<u8>>,
    /// Sequence number of the next frame to play; `None` before the first playout.
    next: Option<u64>,
    buffering: bool,
    over_run: u32,
    under_run: u32,
    missing_run: u32,
    stats: JitterStats,
}

impl Default for JitterBuffer {
    fn default() -> Self {
        Self::new(JitterConfig::default())
    }
}

impl JitterBuffer {
    /// An empty buffer. `target_frames` is at least 1 and `max_frames` at least the target.
    pub fn new(cfg: JitterConfig) -> Self {
        let target_frames = cfg.target_frames.max(1);
        let cfg = JitterConfig {
            target_frames,
            max_frames: cfg.max_frames.max(target_frames),
            drift_window: cfg.drift_window.max(1),
            max_conceal: cfg.max_conceal,
        };
        Self {
            cfg,
            packets: BTreeMap::new(),
            next: None,
            buffering: true,
            over_run: 0,
            under_run: 0,
            missing_run: 0,
            stats: JitterStats::default(),
        }
    }

    /// The configuration in use (after clamping).
    pub fn config(&self) -> JitterConfig {
        self.cfg
    }

    /// Counters since creation (not cleared by [`JitterBuffer::reset`]).
    pub fn stats(&self) -> JitterStats {
        self.stats
    }

    /// Frames between the next one to play and the newest queued, gaps included: the
    /// latency the queue currently adds, in 10 ms frames.
    pub fn depth(&self) -> usize {
        let (Some((&first, _)), Some((&last, _))) = (
            self.packets.first_key_value(),
            self.packets.last_key_value(),
        ) else {
            return 0;
        };
        // While (re)buffering nothing is scheduled yet: playback starts at the oldest queued
        // packet, so a gap before it adds no latency.
        let base = match self.next {
            Some(n) if !self.buffering => n.min(first),
            _ => first,
        };
        usize::try_from(last.saturating_sub(base).saturating_add(1)).unwrap_or(usize::MAX)
    }

    /// Whether playback is waiting for the queue to fill.
    pub fn is_buffering(&self) -> bool {
        self.buffering
    }

    /// Adds a packet that arrived from the network.
    pub fn push(&mut self, seq: u64, packet: Vec<u8>) {
        if self.next.is_some_and(|n| seq < n) {
            self.stats.late += 1;
            return;
        }
        if self.packets.contains_key(&seq) {
            self.stats.duplicate += 1;
            return;
        }
        self.packets.insert(seq, packet);
        self.stats.received += 1;
        while self.depth() > self.cfg.max_frames {
            let Some((dropped, _)) = self.packets.pop_first() else {
                break;
            };
            self.stats.overflow += 1;
            // Never play anything older than what was just dropped.
            let after = dropped.saturating_add(1);
            self.next = Some(self.next.map_or(after, |n| n.max(after)));
        }
    }

    /// What to play for the next 10 ms.
    pub fn pop(&mut self) -> Playout {
        if self.buffering {
            if self.packets.len() < self.cfg.target_frames {
                return Playout::Silence;
            }
            self.buffering = false;
            self.next = self.packets.first_key_value().map(|(&k, _)| k);
            self.over_run = 0;
            self.under_run = 0;
        }
        let Some(next) = self.next else {
            self.buffering = true;
            return Playout::Silence;
        };
        if self.packets.is_empty() {
            self.missing_run = self.missing_run.saturating_add(1);
            if self.missing_run > self.cfg.max_conceal {
                // The stream really stopped (silence on the host, or a long outage): wait
                // for a full target again instead of concealing forever.
                self.buffering = true;
                self.stats.underruns += 1;
                return Playout::Silence;
            }
            self.next = Some(next.saturating_add(1));
            self.stats.concealed += 1;
            return Playout::Conceal;
        }
        if let Some(stretch) = self.correct_drift() {
            return stretch;
        }
        let Some(next) = self.next else {
            return Playout::Silence;
        };
        self.next = Some(next.saturating_add(1));
        match self.packets.remove(&next) {
            Some(packet) => {
                self.missing_run = 0;
                self.stats.played += 1;
                Playout::Packet(packet)
            }
            None => {
                self.stats.concealed += 1;
                Playout::Conceal
            }
        }
    }

    /// Clock-drift control, run once per pop while playing. Skips one frame (returns `None`
    /// after advancing) or stretches by one frame (returns `Some(Conceal)`).
    fn correct_drift(&mut self) -> Option<Playout> {
        let depth = self.depth();
        let target = self.cfg.target_frames;
        if depth > target + 1 {
            self.over_run = self.over_run.saturating_add(1);
            self.under_run = 0;
        } else if depth + 1 < target {
            self.under_run = self.under_run.saturating_add(1);
            self.over_run = 0;
        } else {
            self.over_run = 0;
            self.under_run = 0;
        }
        if self.over_run >= self.cfg.drift_window {
            self.over_run = 0;
            if let Some(next) = self.next {
                self.packets.remove(&next);
                self.next = Some(next.saturating_add(1));
                self.stats.drift_skipped += 1;
            }
        } else if self.under_run >= self.cfg.drift_window {
            self.under_run = 0;
            self.stats.drift_stretched += 1;
            return Some(Playout::Conceal);
        }
        None
    }

    /// Drops everything queued and waits for a fresh start (a new stream generation).
    pub fn reset(&mut self) {
        self.packets.clear();
        self.next = None;
        self.buffering = true;
        self.over_run = 0;
        self.under_run = 0;
        self.missing_run = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(seq: u64) -> Vec<u8> {
        seq.to_le_bytes().to_vec()
    }

    fn seq_of(p: &Playout) -> Option<u64> {
        match p {
            Playout::Packet(b) => Some(u64::from_le_bytes(b.as_slice().try_into().unwrap())),
            _ => None,
        }
    }

    #[test]
    fn waits_for_target_then_plays_in_order() {
        let mut jb = JitterBuffer::default();
        jb.push(0, pkt(0));
        jb.push(1, pkt(1));
        assert_eq!(jb.pop(), Playout::Silence, "still buffering");
        jb.push(2, pkt(2));
        for want in 0..3 {
            assert_eq!(seq_of(&jb.pop()), Some(want));
            jb.push(want + 3, pkt(want + 3));
        }
        assert_eq!(jb.stats().concealed, 0);
    }

    #[test]
    fn reorders_packets() {
        let mut jb = JitterBuffer::default();
        for s in [1, 0, 3, 2] {
            jb.push(s, pkt(s));
        }
        let got: Vec<_> = (0..4).map(|_| seq_of(&jb.pop())).collect();
        assert_eq!(got, [Some(0), Some(1), Some(2), Some(3)]);
    }

    #[test]
    fn conceals_a_lost_packet_and_drops_it_when_late() {
        let mut jb = JitterBuffer::default();
        for s in [0, 1, 3, 4] {
            jb.push(s, pkt(s));
        }
        assert_eq!(seq_of(&jb.pop()), Some(0));
        assert_eq!(seq_of(&jb.pop()), Some(1));
        assert_eq!(jb.pop(), Playout::Conceal, "2 is missing");
        jb.push(2, pkt(2));
        assert_eq!(jb.stats().late, 1, "2 arrived after its slot");
        assert_eq!(seq_of(&jb.pop()), Some(3));
        assert_eq!(seq_of(&jb.pop()), Some(4));
        let st = jb.stats();
        assert_eq!((st.played, st.concealed), (4, 1));
    }

    #[test]
    fn duplicates_are_ignored() {
        let mut jb = JitterBuffer::default();
        jb.push(5, pkt(5));
        jb.push(5, pkt(5));
        assert_eq!(jb.stats().duplicate, 1);
        assert_eq!(jb.depth(), 1);
    }

    #[test]
    fn a_burst_never_exceeds_the_latency_bound() {
        let mut jb = JitterBuffer::default();
        for s in 0..100 {
            jb.push(s, pkt(s));
            assert!(jb.depth() <= jb.config().max_frames);
        }
        // Only the newest frames survive; playback starts with them.
        assert_eq!(seq_of(&jb.pop()), Some(94));
        assert_eq!(jb.stats().overflow, 94);
        // A straggler from before the drop is late, not played.
        jb.push(50, pkt(50));
        assert_eq!(jb.stats().late, 1);
    }

    #[test]
    fn underrun_conceals_briefly_then_rebuffers() {
        let mut jb = JitterBuffer::default();
        for s in 0..3 {
            jb.push(s, pkt(s));
        }
        for _ in 0..3 {
            assert!(seq_of(&jb.pop()).is_some());
        }
        for _ in 0..3 {
            assert_eq!(jb.pop(), Playout::Conceal);
        }
        assert_eq!(jb.pop(), Playout::Silence);
        assert!(jb.is_buffering());
        assert_eq!(jb.stats().underruns, 1);
        // The host resumes later; playback restarts from its new packets.
        for s in 20..23 {
            jb.push(s, pkt(s));
        }
        assert_eq!(seq_of(&jb.pop()), Some(20));
    }

    /// Sender `num/den` times as fast as the player; queue depth after `pops` pops.
    fn run_drift(num: u64, den: u64, pops: u64) -> (JitterBuffer, usize) {
        let mut jb = JitterBuffer::default();
        let mut sent = 0u64;
        let mut max_depth = 0;
        for t in 0..pops {
            // Packets the sender has produced by "time" t, at its own clock.
            let due = (t + 3) * num / den;
            while sent < due {
                jb.push(sent, pkt(sent));
                sent += 1;
            }
            max_depth = max_depth.max(jb.depth());
            jb.pop();
        }
        (jb, max_depth)
    }

    #[test]
    fn fast_sender_clock_is_corrected_by_skipping() {
        // Host card 1% fast: 20 s of audio would add 200 ms of latency uncorrected.
        let (jb, max_depth) = run_drift(101, 100, 2000);
        assert!(jb.stats().drift_skipped + jb.stats().overflow > 0);
        assert!(max_depth <= jb.config().max_frames, "{max_depth}");
        assert!(
            jb.depth() <= jb.config().target_frames + 2,
            "{}",
            jb.depth()
        );
    }

    #[test]
    fn slow_sender_clock_is_corrected_by_stretching() {
        let (jb, max_depth) = run_drift(99, 100, 2000);
        let st = jb.stats();
        assert!(max_depth <= jb.config().max_frames);
        assert!(st.drift_stretched + st.concealed > 0, "{st:?}");
        // Most frames are real audio, not concealment.
        assert!(st.played > 1900, "{st:?}");
    }

    #[test]
    fn reset_starts_over() {
        let mut jb = JitterBuffer::default();
        for s in 0..3 {
            jb.push(s, pkt(s));
        }
        jb.pop();
        jb.reset();
        assert_eq!(jb.depth(), 0);
        assert!(jb.is_buffering());
        // Sequence numbers of a new generation start at 0 again and are not "late".
        for s in 0..3 {
            jb.push(s, pkt(s));
        }
        assert_eq!(seq_of(&jb.pop()), Some(0));
        assert_eq!(jb.stats().late, 0);
    }
}
