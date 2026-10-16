//! Viewer playback pipeline: packets in (network side), device samples out (audio thread).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use crate::codec::OpusDecoder;
use crate::device::PcmSource;
use crate::dsp::Resampler;
use crate::error::AudioError;
use crate::format::{CHANNELS, FRAME_LEN, SAMPLE_RATE};
use crate::jitter::{JitterBuffer, JitterConfig, JitterStats, Playout};

/// Upper bound on frames decoded per device callback (a 1 s device buffer at 10 ms frames);
/// keeps a broken device format from spinning the audio thread.
const MAX_FRAMES_PER_FILL: usize = 100;

struct State {
    jitter: JitterBuffer,
    decoder: OpusDecoder,
    /// Stream generation currently playing; packets of older generations are dropped.
    generation: Option<u64>,
    volume: f32,
    frame: [f32; FRAME_LEN],
    resampler: Option<Resampler>,
    /// Device-format samples decoded but not yet handed to the device (< one frame).
    pending: VecDeque<f32>,
    scratch: Vec<f32>,
    decode_errors: u64,
}

/// Packets from the network in, samples for the speakers out. Cheap to clone; all clones
/// share one pipeline.
#[derive(Clone)]
pub struct Player {
    state: Arc<Mutex<State>>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player").finish_non_exhaustive()
    }
}

impl Player {
    /// A silent player at full volume.
    ///
    /// # Errors
    /// [`AudioError::Codec`] if the Opus decoder cannot be created.
    pub fn new(cfg: JitterConfig) -> Result<Self, AudioError> {
        Ok(Self {
            state: Arc::new(Mutex::new(State {
                jitter: JitterBuffer::new(cfg),
                decoder: OpusDecoder::new()?,
                generation: None,
                volume: 1.0,
                frame: [0.0; FRAME_LEN],
                resampler: None,
                pending: VecDeque::new(),
                scratch: Vec::new(),
                decode_errors: 0,
            })),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues packet `seq` of stream `generation`. A newer generation restarts playback
    /// (the host paused for silence or was re-enabled); packets of older ones are dropped.
    pub fn push(&self, generation: u64, seq: u64, packet: Vec<u8>) {
        let mut s = self.lock();
        match s.generation {
            Some(g) if generation < g => return,
            Some(g) if generation == g => {}
            _ => {
                s.generation = Some(generation);
                s.jitter.reset();
                s.decoder.reset();
            }
        }
        s.jitter.push(seq, packet);
    }

    /// Sets the playback volume, `0.0` (mute) to `1.0` (as sent by the host).
    pub fn set_volume(&self, volume: f32) {
        let v = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.lock().volume = v;
    }

    /// Current volume.
    pub fn volume(&self) -> f32 {
        self.lock().volume
    }

    /// Jitter buffer counters.
    pub fn stats(&self) -> JitterStats {
        self.lock().jitter.stats()
    }

    /// Packets that failed to decode (each was concealed instead).
    pub fn decode_errors(&self) -> u64 {
        self.lock().decode_errors
    }

    /// Frames currently queued (added latency, in 10 ms units).
    pub fn queued_frames(&self) -> usize {
        self.lock().jitter.depth()
    }

    /// Drops everything queued (e.g. when the user turns audio off).
    pub fn clear(&self) {
        let mut s = self.lock();
        s.jitter.reset();
        s.decoder.reset();
        s.pending.clear();
    }

    /// The device-facing end, to hand to [`crate::AudioPlayback::start`].
    pub fn source(&self) -> Box<dyn PcmSource> {
        Box::new(self.clone())
    }
}

impl PcmSource for Player {
    fn fill(&mut self, out: &mut [f32], rate: u32, channels: u16) {
        let mut guard = self.lock();
        let s = &mut *guard;
        let matches = s
            .resampler
            .as_ref()
            .is_some_and(|r| r.out_rate() == rate && r.out_channels() == channels);
        if !matches {
            s.pending.clear();
            s.resampler = Resampler::new(SAMPLE_RATE, CHANNELS, rate, channels).ok();
        }
        let Some(resampler) = s.resampler.as_mut() else {
            out.fill(0.0);
            return;
        };
        let mut frames = 0;
        while s.pending.len() < out.len() && frames < MAX_FRAMES_PER_FILL {
            frames += 1;
            match s.jitter.pop() {
                Playout::Packet(p) => {
                    if let Err(e) = s.decoder.decode(&p, &mut s.frame) {
                        s.decode_errors += 1;
                        tracing::debug!(error = %e, "undecodable audio packet; concealing");
                        let _ = s.decoder.conceal(&mut s.frame);
                    }
                }
                Playout::Conceal => {
                    let _ = s.decoder.conceal(&mut s.frame);
                }
                Playout::Silence => s.frame.fill(0.0),
            }
            s.scratch.clear();
            resampler.process(&s.frame, &mut s.scratch);
            s.pending.extend(s.scratch.iter().copied());
        }
        let vol = s.volume;
        let n = out.len().min(s.pending.len());
        for (o, v) in out.iter_mut().zip(s.pending.drain(..n)) {
            *o = (v * vol).clamp(-1.0, 1.0);
        }
        if let Some(rest) = out.get_mut(n..) {
            rest.fill(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{OpusEncoder, DEFAULT_BITRATE};
    use crate::format::FRAME_SAMPLES;

    fn tone_packets(n: usize) -> Vec<Vec<u8>> {
        let mut enc = OpusEncoder::new(DEFAULT_BITRATE).unwrap();
        (0..n)
            .map(|f| {
                let frame: Vec<f32> = (0..FRAME_SAMPLES)
                    .flat_map(|i| {
                        let t = (f * FRAME_SAMPLES + i) as f32 / 48_000.0;
                        let v = (2.0 * std::f32::consts::PI * 500.0 * t).sin() * 0.5;
                        [v, v]
                    })
                    .collect();
                enc.encode(&frame).unwrap()
            })
            .collect()
    }

    fn peak(s: &[f32]) -> f32 {
        s.iter().fold(0.0f32, |m, x| m.max(x.abs()))
    }

    #[test]
    fn plays_packets_at_device_rate_and_volume() {
        let mut p = Player::new(JitterConfig::default()).unwrap();
        for (i, pkt) in tone_packets(10).into_iter().enumerate() {
            p.push(0, i as u64, pkt);
        }
        // A 44.1 kHz mono device pulling 10 ms at a time.
        let mut out = vec![0.0f32; 441];
        let mut all = Vec::new();
        for _ in 0..8 {
            p.fill(&mut out, 44_100, 1);
            all.extend_from_slice(&out);
        }
        assert!(peak(&all[2000..]) > 0.3, "tone audible");
        p.set_volume(0.25);
        p.fill(&mut out, 44_100, 1);
        assert!(peak(&out) < 0.2, "volume applied: {}", peak(&out));
        p.set_volume(f32::NAN);
        assert_eq!(p.volume(), 1.0);
        assert_eq!(p.decode_errors(), 0);
    }

    #[test]
    fn newer_generation_restarts_and_older_is_dropped() {
        let p = Player::new(JitterConfig::default()).unwrap();
        let pkts = tone_packets(3);
        p.push(5, 0, pkts[0].clone());
        p.push(5, 1, pkts[1].clone());
        assert_eq!(p.queued_frames(), 2);
        p.push(6, 0, pkts[2].clone());
        assert_eq!(p.queued_frames(), 1, "generation 6 replaced 5");
        p.push(5, 2, pkts[0].clone());
        assert_eq!(p.queued_frames(), 1, "stale generation dropped");
    }

    #[test]
    fn garbage_packets_are_concealed_not_fatal() {
        let mut p = Player::new(JitterConfig::default()).unwrap();
        for i in 0..4u64 {
            p.push(0, i, vec![0xff; 40]);
        }
        let mut out = vec![0.0f32; 960];
        for _ in 0..4 {
            p.fill(&mut out, 48_000, 2);
            assert!(out.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
        }
        // Silence when nothing is queued; also with an absurd device format.
        p.fill(&mut out, 0, 0);
        assert!(out.iter().all(|&s| s == 0.0));
    }
}
