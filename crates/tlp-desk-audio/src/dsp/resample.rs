//! Streaming sample-rate and channel-layout conversion for interleaved `f32` PCM.
//!
//! Linear interpolation: cheap (a few multiply-adds per sample), no added latency and good
//! enough for the common 44.1/48/96 kHz conversions of desktop audio. Downsampling by more
//! than 2x aliases a little above the new Nyquist rate; desktop devices almost always run at
//! 44.1 or 48 kHz, so this is a deliberate trade for simplicity.

use crate::error::AudioError;
use crate::format::{MAX_DEVICE_CHANNELS, MAX_DEVICE_RATE, MIN_DEVICE_RATE};

/// -3 dB: how much of a centre/surround channel goes into each stereo side.
const SURROUND_GAIN: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Converts a continuous stream of interleaved frames from one rate/layout to another.
///
/// Keeps the last input frame and the fractional read position between calls, so chunks of
/// any size produce one seamless output stream.
#[derive(Debug, Clone)]
pub struct Resampler {
    in_rate: u32,
    in_channels: u16,
    out_rate: u32,
    out_channels: u16,
    /// Input frames advanced per output frame (`in_rate / out_rate`).
    step: f64,
    /// Position of the next output frame between `prev` (0.0) and the next input frame (1.0).
    phase: f64,
    /// Previous input frame, already mapped to the output layout.
    prev: Vec<f32>,
    /// Scratch: the current input frame mapped to the output layout.
    cur: Vec<f32>,
}

fn check(rate: u32, channels: u16) -> Result<(), AudioError> {
    if (MIN_DEVICE_RATE..=MAX_DEVICE_RATE).contains(&rate)
        && (1..=MAX_DEVICE_CHANNELS).contains(&channels)
    {
        Ok(())
    } else {
        Err(AudioError::UnsupportedFormat { rate, channels })
    }
}

impl Resampler {
    /// A converter from `in_rate`/`in_channels` to `out_rate`/`out_channels`.
    ///
    /// # Errors
    /// [`AudioError::UnsupportedFormat`] for a rate outside 8-384 kHz or a channel count
    /// outside 1-32.
    pub fn new(
        in_rate: u32,
        in_channels: u16,
        out_rate: u32,
        out_channels: u16,
    ) -> Result<Self, AudioError> {
        check(in_rate, in_channels)?;
        check(out_rate, out_channels)?;
        let n = usize::from(out_channels);
        Ok(Self {
            in_rate,
            in_channels,
            out_rate,
            out_channels,
            step: f64::from(in_rate) / f64::from(out_rate),
            phase: 0.0,
            prev: vec![0.0; n],
            cur: vec![0.0; n],
        })
    }

    /// Input sample rate.
    pub fn in_rate(&self) -> u32 {
        self.in_rate
    }

    /// Input channel count.
    pub fn in_channels(&self) -> u16 {
        self.in_channels
    }

    /// Output sample rate.
    pub fn out_rate(&self) -> u32 {
        self.out_rate
    }

    /// Output channel count.
    pub fn out_channels(&self) -> u16 {
        self.out_channels
    }

    /// Converts `input` (interleaved, `in_channels` per frame) and appends the result to `out`.
    /// A trailing partial frame in `input` is ignored.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let in_ch = usize::from(self.in_channels);
        let frames = input.len() / in_ch;
        // Expected output size plus one frame of rounding slack.
        let estimate = (frames as f64 / self.step).ceil() as usize + 1;
        out.reserve(estimate.saturating_mul(usize::from(self.out_channels)));
        for frame in input.chunks_exact(in_ch) {
            map_channels(frame, &mut self.cur);
            while self.phase < 1.0 {
                let t = self.phase as f32;
                out.extend(
                    self.prev
                        .iter()
                        .zip(&self.cur)
                        .map(|(&a, &b)| a + (b - a) * t),
                );
                self.phase += self.step;
            }
            self.phase -= 1.0;
            std::mem::swap(&mut self.prev, &mut self.cur);
        }
    }

    /// Forgets the stream history (call after a gap, e.g. when the device restarts).
    pub fn reset(&mut self) {
        self.phase = 0.0;
        self.prev.iter_mut().for_each(|s| *s = 0.0);
    }
}

/// Maps one input frame to the output layout.
///
/// * same count: copied;
/// * to mono: average of all channels;
/// * mono to many: the sample on the first two channels;
/// * many to stereo/more: front left/right plus -3 dB of the centre and surround channels
///   (WAVE order FL FR FC LFE BL BR SL SR; LFE is dropped), extra output channels silent.
fn map_channels(input: &[f32], out: &mut [f32]) {
    let (i, o) = (input.len(), out.len());
    if i == o {
        out.copy_from_slice(input);
        return;
    }
    if o == 1 {
        let sum: f32 = input.iter().sum();
        if let Some(s) = out.first_mut() {
            *s = sum / i as f32;
        }
        return;
    }
    out.iter_mut().for_each(|s| *s = 0.0);
    if i == 1 {
        let v = input.first().copied().unwrap_or(0.0);
        out.iter_mut().take(2).for_each(|s| *s = v);
        return;
    }
    let ch = |n: usize| input.get(n).copied().unwrap_or(0.0);
    let (mut left, mut right) = (ch(0), ch(1));
    let mut extra = 0usize;
    if i > 2 {
        let centre = ch(2) * SURROUND_GAIN;
        left += centre;
        right += centre;
        extra += 1;
    }
    // Rear and side pairs (indices 4/5 and 6/7); index 3 is LFE.
    for pair in [4usize, 6] {
        if i > pair + 1 {
            left += ch(pair) * SURROUND_GAIN;
            right += ch(pair + 1) * SURROUND_GAIN;
            extra += 1;
        }
    }
    // Keep the downmix from clipping when every channel is loud.
    let norm = 1.0 / (1.0 + extra as f32 * SURROUND_GAIN);
    if let Some(s) = out.get_mut(0) {
        *s = left * norm;
    }
    if let Some(s) = out.get_mut(1) {
        *s = right * norm;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, channels: u16, freq: f32, frames: usize) -> Vec<f32> {
        let mut v = Vec::with_capacity(frames * usize::from(channels));
        for n in 0..frames {
            let s = (2.0 * std::f32::consts::PI * freq * n as f32 / rate as f32).sin() * 0.5;
            for _ in 0..channels {
                v.push(s);
            }
        }
        v
    }

    /// Frequency estimate from upward zero crossings of channel 0.
    fn freq(samples: &[f32], channels: usize, rate: u32) -> f32 {
        let ch0: Vec<f32> = samples.iter().step_by(channels).copied().collect();
        let crossings = ch0.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        crossings as f32 * rate as f32 / ch0.len() as f32
    }

    #[test]
    fn converts_44k1_to_48k_keeping_pitch_and_length() {
        let mut r = Resampler::new(44_100, 2, 48_000, 2).unwrap();
        let input = sine(44_100, 2, 1000.0, 44_100);
        let mut out = Vec::new();
        // Odd chunk sizes: the stream must stay seamless across calls.
        for chunk in input.chunks(2 * 441 + 6) {
            r.process(chunk, &mut out);
        }
        let frames = out.len() / 2;
        assert!((47_990..=48_010).contains(&frames), "{frames} frames");
        let f = freq(&out, 2, 48_000);
        assert!((f - 1000.0).abs() < 5.0, "{f} Hz");
    }

    #[test]
    fn same_rate_is_a_one_frame_delayed_copy() {
        let mut r = Resampler::new(48_000, 2, 48_000, 2).unwrap();
        let input: Vec<f32> = (0..20).map(|n| n as f32 / 20.0).collect();
        let mut out = Vec::new();
        r.process(&input, &mut out);
        assert_eq!(out.len(), input.len());
        assert_eq!(&out[2..], &input[..18]);
    }

    #[test]
    fn downsamples_96k() {
        let mut r = Resampler::new(96_000, 2, 48_000, 2).unwrap();
        let mut out = Vec::new();
        r.process(&sine(96_000, 2, 440.0, 96_000), &mut out);
        assert_eq!(out.len() / 2, 48_000);
        assert!((freq(&out, 2, 48_000) - 440.0).abs() < 3.0);
    }

    #[test]
    fn channel_maps() {
        let mut o = [0.0f32; 2];
        map_channels(&[0.4], &mut o);
        assert_eq!(o, [0.4, 0.4]);
        let mut m = [0.0f32; 1];
        map_channels(&[0.2, 0.6], &mut m);
        assert!((m[0] - 0.4).abs() < 1e-6);
        // 5.1: FL FR FC LFE BL BR; LFE must not leak, sides stay on their side.
        map_channels(&[1.0, 0.0, 0.0, 1.0, 0.0, 0.0], &mut o);
        assert!(o[0] > 0.0 && o[1] == 0.0, "{o:?}");
        map_channels(&[0.0, 0.0, 0.0, 0.0, 0.0, 1.0], &mut o);
        assert!(o[0] == 0.0 && o[1] > 0.0, "{o:?}");
        // Full-scale on every channel stays within [-1, 1].
        map_channels(&[1.0; 8], &mut o);
        assert!(o.iter().all(|s| s.abs() <= 1.0 + 1e-6), "{o:?}");
        // Stereo to 6 outputs: front pair only.
        let mut six = [9.0f32; 6];
        map_channels(&[0.1, 0.2], &mut six);
        assert_eq!(six, [0.1, 0.2, 0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn rejects_bad_formats_and_partial_frames() {
        assert!(Resampler::new(0, 2, 48_000, 2).is_err());
        assert!(Resampler::new(48_000, 0, 48_000, 2).is_err());
        assert!(Resampler::new(48_000, 2, 48_000, 33).is_err());
        assert!(Resampler::new(1_000_000, 2, 48_000, 2).is_err());
        let mut r = Resampler::new(48_000, 2, 48_000, 2).unwrap();
        let mut out = Vec::new();
        r.process(&[0.1, 0.2, 0.3], &mut out);
        assert_eq!(out.len(), 2);
        r.process(&[], &mut out);
        assert_eq!(out.len(), 2);
    }
}
