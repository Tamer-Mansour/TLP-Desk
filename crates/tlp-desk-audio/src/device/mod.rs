//! Audio devices behind traits: the system's loopback capture and speakers
//! ([`SystemCapture`], [`SystemPlayback`]) and in-memory fakes for tests ([`fake`]).
//!
//! Capture *pushes* chunks into a bounded queue ([`PcmSink`]); playback *pulls* from a
//! [`PcmSource`] on the device's own clock. Both stop when their handle is dropped.

pub mod fake;
mod system;

use tokio::sync::mpsc;

use crate::error::AudioError;

pub use system::{SystemCapture, SystemPlayback};

/// Interleaved `f32` samples as the device delivered them.
#[derive(Debug, Clone, PartialEq)]
pub struct PcmChunk {
    /// Samples per second per channel.
    pub rate: u32,
    /// Channels per frame.
    pub channels: u16,
    /// Interleaved samples, nominally in `-1.0..=1.0`.
    pub samples: Vec<f32>,
}

/// Chunks queued between a capture callback and the encoder. At 10 ms per device period this
/// is 160 ms; a full queue drops new chunks (the encoder fell behind), it never grows.
pub const CAPTURE_QUEUE: usize = 16;

/// Where a capture device delivers its chunks. Never blocks the device's thread.
#[derive(Debug, Clone)]
pub struct PcmSink(mpsc::Sender<PcmChunk>);

impl PcmSink {
    /// A sink and the receiver the encoder reads from, holding at most [`CAPTURE_QUEUE`].
    pub fn channel() -> (Self, mpsc::Receiver<PcmChunk>) {
        let (tx, rx) = mpsc::channel(CAPTURE_QUEUE);
        (Self(tx), rx)
    }

    /// Queues a chunk; `false` if it was dropped (queue full or the encoder stopped).
    pub fn send(&self, chunk: PcmChunk) -> bool {
        self.0.try_send(chunk).is_ok()
    }

    /// Whether the encoder has gone away (capture can stop).
    pub fn is_closed(&self) -> bool {
        self.0.is_closed()
    }
}

/// Stops the device when dropped.
pub struct DeviceHandle {
    _guard: Box<dyn Send>,
}

impl DeviceHandle {
    /// Wraps whatever keeps the device running (a stream, a thread's stop signal...).
    pub fn new<T: Send + 'static>(guard: T) -> Self {
        Self {
            _guard: Box::new(guard),
        }
    }
}

impl std::fmt::Debug for DeviceHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceHandle")
    }
}

/// A source of the host's system audio.
pub trait AudioCapture: Send + Sync {
    /// Starts capturing into `sink` until the returned handle is dropped.
    ///
    /// # Errors
    /// [`AudioError::NoDevice`] / [`AudioError::Device`] if capture cannot start.
    fn start(&self, sink: PcmSink) -> Result<DeviceHandle, AudioError>;
}

/// Produces the samples an output device plays.
pub trait PcmSource: Send {
    /// Fills `out` completely with interleaved samples at `rate` / `channels` (the device's
    /// format). Called on the device's audio thread: must not block.
    fn fill(&mut self, out: &mut [f32], rate: u32, channels: u16);
}

/// The viewer's speakers.
pub trait AudioPlayback: Send + Sync {
    /// Plays what `source` produces until the returned handle is dropped.
    ///
    /// # Errors
    /// [`AudioError::NoDevice`] / [`AudioError::Device`] if playback cannot start.
    fn start(&self, source: Box<dyn PcmSource>) -> Result<DeviceHandle, AudioError>;
}
