//! In-memory devices for tests and headless runs: they never touch a sound card.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::{AudioCapture, AudioPlayback, DeviceHandle, PcmChunk, PcmSink, PcmSource};
use crate::error::AudioError;

/// Sets a flag when dropped, which ends the device's thread.
struct StopOnDrop {
    stop: Arc<AtomicBool>,
    running: Arc<AtomicUsize>,
}

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Count the device as stopped right away; its thread exits within one period.
        self.running.fetch_sub(1, Ordering::AcqRel);
    }
}

/// What a [`FakeCapture`] produces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FakeSignal {
    /// A sine at `freq` Hz with peak `amplitude`, the same on every channel.
    Tone {
        /// Frequency in Hz.
        freq: f32,
        /// Peak level, `0.0..=1.0`.
        amplitude: f32,
    },
    /// Digital silence.
    Silence,
}

/// A loopback "device" that produces a signal in real time, in `period` chunks, on its own
/// thread. Counts how often it was started and whether it is running.
#[derive(Debug, Clone)]
pub struct FakeCapture {
    /// Device sample rate.
    pub rate: u32,
    /// Device channel count.
    pub channels: u16,
    /// Chunk duration.
    pub period: Duration,
    signal: Arc<Mutex<FakeSignal>>,
    starts: Arc<AtomicUsize>,
    running: Arc<AtomicUsize>,
}

impl FakeCapture {
    /// A device at `rate` / `channels` producing `signal` in 10 ms chunks.
    pub fn new(rate: u32, channels: u16, signal: FakeSignal) -> Self {
        Self {
            rate,
            channels,
            period: Duration::from_millis(10),
            signal: Arc::new(Mutex::new(signal)),
            starts: Arc::default(),
            running: Arc::default(),
        }
    }

    /// Changes the signal (also for a running capture).
    pub fn set_signal(&self, signal: FakeSignal) {
        *self.signal.lock().unwrap_or_else(PoisonError::into_inner) = signal;
    }

    /// How many times capture was started.
    pub fn starts(&self) -> usize {
        self.starts.load(Ordering::Acquire)
    }

    /// Whether a capture is running now.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire) > 0
    }
}

impl AudioCapture for FakeCapture {
    fn start(&self, sink: PcmSink) -> Result<DeviceHandle, AudioError> {
        self.starts.fetch_add(1, Ordering::AcqRel);
        self.running.fetch_add(1, Ordering::AcqRel);
        let stop = Arc::new(AtomicBool::new(false));
        let guard = StopOnDrop {
            stop: stop.clone(),
            running: self.running.clone(),
        };
        let (rate, channels, period) = (self.rate, self.channels, self.period);
        let signal = self.signal.clone();
        std::thread::Builder::new()
            .name("fake-audio-capture".into())
            .spawn(move || {
                let per_chunk = (u128::from(rate) * period.as_micros() / 1_000_000) as usize;
                let mut n: u64 = 0;
                let mut next = Instant::now();
                while !stop.load(Ordering::Acquire) && !sink.is_closed() {
                    let sig = *signal.lock().unwrap_or_else(PoisonError::into_inner);
                    let mut samples = Vec::with_capacity(per_chunk * usize::from(channels));
                    for _ in 0..per_chunk {
                        let v = match sig {
                            FakeSignal::Tone { freq, amplitude } => {
                                let t = n as f64 / f64::from(rate);
                                (2.0 * std::f64::consts::PI * f64::from(freq) * t).sin() as f32
                                    * amplitude
                            }
                            FakeSignal::Silence => 0.0,
                        };
                        n += 1;
                        samples.extend(std::iter::repeat_n(v, usize::from(channels)));
                    }
                    sink.send(PcmChunk {
                        rate,
                        channels,
                        samples,
                    });
                    next += period;
                    if let Some(wait) = next.checked_duration_since(Instant::now()) {
                        std::thread::sleep(wait);
                    }
                }
            })
            .map_err(|e| AudioError::Device(e.to_string()))?;
        Ok(DeviceHandle::new(guard))
    }
}

/// A "speaker" that pulls from its source in real time and records everything it played.
#[derive(Debug, Clone)]
pub struct FakePlayback {
    /// Device sample rate.
    pub rate: u32,
    /// Device channel count.
    pub channels: u16,
    /// Pull period.
    pub period: Duration,
    recorded: Arc<Mutex<Vec<f32>>>,
    starts: Arc<AtomicUsize>,
    running: Arc<AtomicUsize>,
}

impl FakePlayback {
    /// A device at `rate` / `channels` pulling every 10 ms.
    pub fn new(rate: u32, channels: u16) -> Self {
        Self {
            rate,
            channels,
            period: Duration::from_millis(10),
            recorded: Arc::default(),
            starts: Arc::default(),
            running: Arc::default(),
        }
    }

    /// Everything played so far (interleaved, device format).
    pub fn recorded(&self) -> Vec<f32> {
        self.recorded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Forgets the recording.
    pub fn clear(&self) {
        self.recorded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// How many times playback was started.
    pub fn starts(&self) -> usize {
        self.starts.load(Ordering::Acquire)
    }

    /// Whether playback is running now.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire) > 0
    }
}

impl AudioPlayback for FakePlayback {
    fn start(&self, mut source: Box<dyn PcmSource>) -> Result<DeviceHandle, AudioError> {
        self.starts.fetch_add(1, Ordering::AcqRel);
        self.running.fetch_add(1, Ordering::AcqRel);
        let stop = Arc::new(AtomicBool::new(false));
        let guard = StopOnDrop {
            stop: stop.clone(),
            running: self.running.clone(),
        };
        let (rate, channels, period) = (self.rate, self.channels, self.period);
        let recorded = self.recorded.clone();
        std::thread::Builder::new()
            .name("fake-audio-playback".into())
            .spawn(move || {
                let frames = (u128::from(rate) * period.as_micros() / 1_000_000) as usize;
                let mut buf = vec![0.0f32; frames * usize::from(channels)];
                let mut next = Instant::now();
                while !stop.load(Ordering::Acquire) {
                    source.fill(&mut buf, rate, channels);
                    recorded
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .extend_from_slice(&buf);
                    next += period;
                    if let Some(wait) = next.checked_duration_since(Instant::now()) {
                        std::thread::sleep(wait);
                    }
                }
            })
            .map_err(|e| AudioError::Device(e.to_string()))?;
        Ok(DeviceHandle::new(guard))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_capture_runs_until_the_handle_drops() {
        let cap = FakeCapture::new(
            44_100,
            1,
            FakeSignal::Tone {
                freq: 1000.0,
                amplitude: 0.5,
            },
        );
        let (sink, mut rx) = PcmSink::channel();
        let handle = cap.start(sink).unwrap();
        assert!(cap.is_running());
        let chunk = rx.blocking_recv().unwrap();
        assert_eq!(
            (chunk.rate, chunk.channels, chunk.samples.len()),
            (44_100, 1, 441)
        );
        drop(handle);
        assert!(!cap.is_running());
        assert_eq!(cap.starts(), 1);
    }

    struct Ramp(f32);
    impl PcmSource for Ramp {
        fn fill(&mut self, out: &mut [f32], _rate: u32, _channels: u16) {
            for s in out {
                self.0 += 1.0;
                *s = self.0;
            }
        }
    }

    #[test]
    fn fake_playback_records_what_it_pulls() {
        let out = FakePlayback::new(48_000, 2);
        let handle = out.start(Box::new(Ramp(0.0))).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while out.recorded().len() < 960 * 3 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(handle);
        let rec = out.recorded();
        assert!(rec.len() >= 960 * 3);
        assert_eq!(&rec[..3], &[1.0, 2.0, 3.0]);
        assert!(!out.is_running());
    }
}
