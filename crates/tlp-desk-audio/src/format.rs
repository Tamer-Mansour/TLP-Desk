//! The one PCM format that travels between host and viewer.
//!
//! Whatever the devices use, the host converts to 48 kHz stereo float before encoding and the
//! viewer converts from it after decoding, so the wire format never depends on the hardware.

/// Wire sample rate (Opus' native rate).
pub const SAMPLE_RATE: u32 = 48_000;

/// Wire channel count (interleaved left, right).
pub const CHANNELS: u16 = 2;

/// Duration of one Opus frame in milliseconds. 10 ms keeps the encoder's share of the
/// latency budget small while Opus still codes music well at that size.
pub const FRAME_MS: u32 = 10;

/// Samples per channel in one frame (480 at 48 kHz).
pub const FRAME_SAMPLES: usize = (SAMPLE_RATE / 1000 * FRAME_MS) as usize;

/// Interleaved samples in one frame (`FRAME_SAMPLES * CHANNELS`).
pub const FRAME_LEN: usize = FRAME_SAMPLES * CHANNELS as usize;

/// Largest Opus packet we produce or accept. libopus never emits more than 1275 bytes for a
/// single 10 ms frame; anything larger on the wire is hostile and ends the stream.
pub const MAX_PACKET: usize = 1500;

/// Largest channel count accepted from a device (7.1 = 8; some pro cards report more).
pub const MAX_DEVICE_CHANNELS: u16 = 32;

/// Highest device sample rate accepted (384 kHz is the highest rate WASAPI devices report).
pub const MAX_DEVICE_RATE: u32 = 384_000;

/// Lowest device sample rate accepted.
pub const MIN_DEVICE_RATE: u32 = 8_000;
