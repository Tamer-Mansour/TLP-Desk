//! Opus encoding and decoding of 10 ms wire frames (libopus through the safe `opus` crate).

use crate::error::AudioError;
use crate::format::{CHANNELS, FRAME_LEN, FRAME_SAMPLES, MAX_PACKET, SAMPLE_RATE};

/// Default encoder bitrate: transparent for most desktop audio (music, video, calls) at a
/// fraction of the video stream's bandwidth.
pub const DEFAULT_BITRATE: u32 = 128_000;

/// Encoder complexity 0-10; 8 costs about a third less CPU than 10 for no audible change.
const COMPLEXITY: i32 = 8;

fn codec_err(op: &'static str) -> impl Fn(opus::Error) -> AudioError {
    move |e| AudioError::Codec {
        op,
        reason: e.description().to_owned(),
    }
}

fn channels() -> opus::Channels {
    if CHANNELS == 1 {
        opus::Channels::Mono
    } else {
        opus::Channels::Stereo
    }
}

/// Encodes 48 kHz stereo frames of [`FRAME_LEN`] interleaved samples into Opus packets.
pub struct OpusEncoder {
    enc: opus::Encoder,
    buf: Vec<u8>,
}

impl std::fmt::Debug for OpusEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpusEncoder").finish_non_exhaustive()
    }
}

impl OpusEncoder {
    /// A music-mode encoder at `bitrate` bits per second (clamped to 16-510 kbit/s).
    ///
    /// # Errors
    /// [`AudioError::Codec`] if libopus cannot create or configure the encoder.
    pub fn new(bitrate: u32) -> Result<Self, AudioError> {
        let mut enc = opus::Encoder::new(SAMPLE_RATE, channels(), opus::Application::Audio)
            .map_err(codec_err("create encoder"))?;
        let bits = i32::try_from(bitrate.clamp(16_000, 510_000)).unwrap_or(128_000);
        enc.set_bitrate(opus::Bitrate::Bits(bits))
            .map_err(codec_err("set bitrate"))?;
        enc.set_complexity(COMPLEXITY)
            .map_err(codec_err("set complexity"))?;
        // The audio stream is reliable (QUIC), so in-band FEC would only waste bits.
        enc.set_inband_fec(false).map_err(codec_err("set fec"))?;
        Ok(Self {
            enc,
            buf: vec![0; MAX_PACKET],
        })
    }

    /// Encodes one frame of exactly [`FRAME_LEN`] interleaved samples.
    ///
    /// # Errors
    /// [`AudioError::Codec`] for a frame of the wrong length or an encoder failure.
    pub fn encode(&mut self, frame: &[f32]) -> Result<Vec<u8>, AudioError> {
        if frame.len() != FRAME_LEN {
            return Err(AudioError::Codec {
                op: "encode",
                reason: format!("frame of {} samples, expected {FRAME_LEN}", frame.len()),
            });
        }
        let n = self
            .enc
            .encode_float(frame, &mut self.buf)
            .map_err(codec_err("encode"))?;
        Ok(self.buf.get(..n).unwrap_or_default().to_vec())
    }

    /// Samples per channel the decoder output lags the encoder input (codec look-ahead).
    pub fn lookahead(&mut self) -> usize {
        self.enc
            .get_lookahead()
            .ok()
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0)
    }
}

/// Decodes Opus packets into 48 kHz stereo frames; conceals lost packets.
pub struct OpusDecoder {
    dec: opus::Decoder,
}

impl std::fmt::Debug for OpusDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpusDecoder").finish_non_exhaustive()
    }
}

impl OpusDecoder {
    /// A 48 kHz stereo decoder.
    ///
    /// # Errors
    /// [`AudioError::Codec`] if libopus cannot create the decoder.
    pub fn new() -> Result<Self, AudioError> {
        let dec =
            opus::Decoder::new(SAMPLE_RATE, channels()).map_err(codec_err("create decoder"))?;
        Ok(Self { dec })
    }

    /// Decodes one packet from the peer into `out` (one frame, [`FRAME_LEN`] samples).
    ///
    /// # Errors
    /// [`AudioError::BadPacket`] for an empty or oversized packet, [`AudioError::Codec`] for
    /// a corrupt packet or one that is not exactly one 10 ms frame. The decoder stays usable.
    pub fn decode(&mut self, packet: &[u8], out: &mut [f32; FRAME_LEN]) -> Result<(), AudioError> {
        if packet.is_empty() || packet.len() > MAX_PACKET {
            return Err(AudioError::BadPacket {
                len: packet.len(),
                max: MAX_PACKET,
            });
        }
        let n = self
            .dec
            .decode_float(packet, out, false)
            .map_err(codec_err("decode"))?;
        if n != FRAME_SAMPLES {
            return Err(AudioError::Codec {
                op: "decode",
                reason: format!("packet holds {n} samples per channel, expected {FRAME_SAMPLES}"),
            });
        }
        Ok(())
    }

    /// Fills `out` with a concealment frame for a lost or late packet (Opus PLC: a smooth
    /// continuation that fades out), never a click.
    ///
    /// # Errors
    /// [`AudioError::Codec`] if libopus fails, in which case `out` is silence.
    pub fn conceal(&mut self, out: &mut [f32; FRAME_LEN]) -> Result<(), AudioError> {
        match self.dec.decode_float(&[], out, false) {
            Ok(_) => Ok(()),
            Err(e) => {
                out.fill(0.0);
                Err(codec_err("conceal")(e))
            }
        }
    }

    /// Forgets the decoder state (a new stream generation starts).
    pub fn reset(&mut self) {
        let _ = self.dec.reset_state();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_wrong_frame_sizes_and_hostile_packets() {
        let mut enc = OpusEncoder::new(DEFAULT_BITRATE).unwrap();
        assert!(enc.encode(&[0.0; 10]).is_err());
        let mut dec = OpusDecoder::new().unwrap();
        let mut out = [0.0f32; FRAME_LEN];
        assert!(matches!(
            dec.decode(&[], &mut out),
            Err(AudioError::BadPacket { .. })
        ));
        assert!(matches!(
            dec.decode(&[0u8; MAX_PACKET + 1], &mut out),
            Err(AudioError::BadPacket { .. })
        ));
        // Random bytes: an error or a frame, never a panic.
        let mut x: u32 = 0x1234_5678;
        for len in 1..200usize {
            let pkt: Vec<u8> = (0..len)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    x as u8
                })
                .collect();
            let _ = dec.decode(&pkt, &mut out);
        }
        // A 20 ms packet is not one wire frame.
        let mut big =
            opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::Audio).unwrap();
        let pkt = big.encode_vec_float(&[0.0; 1920], 1500).unwrap();
        assert!(dec.decode(&pkt, &mut out).is_err());
        // Still usable afterwards.
        let good = enc.encode(&[0.0; FRAME_LEN]).unwrap();
        dec.decode(&good, &mut out).unwrap();
        dec.conceal(&mut out).unwrap();
    }
}
