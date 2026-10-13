//! Silence detection: the host sends nothing while the speakers are quiet.

/// Peak level below which a frame counts as silent: about -80 dBFS, under the dither and
/// noise floor of real devices but above exact digital zero.
pub const DEFAULT_THRESHOLD: f32 = 1.0e-4;

/// Quiet frames still sent after the sound stops (20 x 10 ms = 200 ms), so the Opus decoder
/// rings out naturally and short pauses between words or notes do not restart the stream.
pub const DEFAULT_HANGOVER: u32 = 20;

/// Decides per frame whether it is worth sending.
#[derive(Debug, Clone)]
pub struct SilenceGate {
    threshold: f32,
    hangover: u32,
    /// Consecutive silent frames seen so far (saturating).
    quiet_run: u32,
}

impl Default for SilenceGate {
    fn default() -> Self {
        Self::new(DEFAULT_THRESHOLD, DEFAULT_HANGOVER)
    }
}

impl SilenceGate {
    /// A gate that closes after `hangover` frames whose peak stays below `threshold`.
    pub fn new(threshold: f32, hangover: u32) -> Self {
        Self {
            threshold,
            hangover,
            // Start closed: nothing is sent until there is something to hear.
            quiet_run: hangover.saturating_add(1),
        }
    }

    /// `true` if `frame` should be encoded and sent, `false` while the gate is closed.
    pub fn should_send(&mut self, frame: &[f32]) -> bool {
        let peak = frame.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        // NaN from a broken driver compares false and counts as silence.
        if peak > self.threshold {
            self.quiet_run = 0;
            true
        } else {
            self.quiet_run = self.quiet_run.saturating_add(1);
            self.quiet_run <= self.hangover
        }
    }

    /// Whether the gate is currently closed (the last frame was not sent).
    pub fn is_closed(&self) -> bool {
        self.quiet_run > self.hangover
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_on_sound_and_closes_after_the_hangover() {
        let mut g = SilenceGate::new(0.01, 3);
        let quiet = [0.0f32; 8];
        let loud = [0.0, 0.5, -0.5, 0.0];
        assert!(!g.should_send(&quiet), "starts closed");
        assert!(g.is_closed());
        assert!(g.should_send(&loud));
        assert!(!g.is_closed());
        for _ in 0..3 {
            assert!(g.should_send(&quiet), "hangover keeps sending");
        }
        assert!(!g.should_send(&quiet));
        assert!(g.is_closed());
        assert!(g.should_send(&[0.02]), "reopens at once");
    }

    #[test]
    fn noise_floor_and_nan_count_as_silence() {
        let mut g = SilenceGate::default();
        assert!(!g.should_send(&[0.00001, -0.00002]));
        assert!(!g.should_send(&[f32::NAN]));
        assert!(!g.should_send(&[]));
    }
}
