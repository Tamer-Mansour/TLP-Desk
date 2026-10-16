//! A deterministic system-audio source.
//!
//! There is no audio-source trait in the workspace yet; the audio lane wraps this in its own.
//! It produces what a WASAPI loopback capture would: interleaved `f32` PCM in fixed chunks.

use std::f32::consts::TAU;

/// A sine tone (or silence) in fixed-size chunks, sample-exact and repeatable.
#[derive(Debug, Clone)]
pub struct FakeAudioSource {
    sample_rate: u32,
    channels: u16,
    freq_hz: f32,
    amplitude: f32,
    /// Frames (samples per channel) produced so far.
    position: u64,
}

impl FakeAudioSource {
    /// A 440 Hz tone at half amplitude.
    pub fn sine(sample_rate: u32, channels: u16) -> Self {
        Self {
            sample_rate: sample_rate.max(1),
            channels: channels.max(1),
            freq_hz: 440.0,
            amplitude: 0.5,
            position: 0,
        }
    }

    /// All-zero samples (a silent PC).
    pub fn silence(sample_rate: u32, channels: u16) -> Self {
        Self {
            amplitude: 0.0,
            ..Self::sine(sample_rate, channels)
        }
    }

    /// Samples per second per channel.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Interleaved channels.
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Frames per chunk of `ms` milliseconds (20 ms = one Opus packet at 48 kHz: 960).
    pub fn frames_per(&self, ms: u32) -> usize {
        usize::try_from(u64::from(self.sample_rate) * u64::from(ms) / 1000).unwrap_or(0)
    }

    /// The next `frames` frames, interleaved (`frames * channels` samples in `-1.0..=1.0`).
    pub fn next_chunk(&mut self, frames: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames * usize::from(self.channels));
        for _ in 0..frames {
            let t = (self.position % u64::from(self.sample_rate)) as f32 / self.sample_rate as f32;
            let v = self.amplitude * (TAU * self.freq_hz * t).sin();
            for _ in 0..self.channels {
                out.push(v);
            }
            self.position += 1;
        }
        out
    }
}

impl Iterator for FakeAudioSource {
    type Item = Vec<f32>;

    /// Endless 20 ms chunks.
    fn next(&mut self) -> Option<Vec<f32>> {
        let n = self.frames_per(20);
        Some(self.next_chunk(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_are_sized_bounded_and_repeatable() {
        let mut a = FakeAudioSource::sine(48_000, 2);
        let mut b = FakeAudioSource::sine(48_000, 2);
        assert_eq!(a.frames_per(20), 960);
        let c = a.next().unwrap();
        assert_eq!(c.len(), 960 * 2);
        assert!(c.iter().all(|s| s.abs() <= 0.5));
        assert!(c.iter().any(|s| s.abs() > 0.1));
        assert_eq!(b.next().unwrap(), c);
        let mut s = FakeAudioSource::silence(44_100, 1);
        assert!(s.next_chunk(441).iter().all(|x| *x == 0.0));
    }
}
