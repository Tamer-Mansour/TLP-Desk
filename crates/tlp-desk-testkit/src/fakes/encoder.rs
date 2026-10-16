//! An encoder that does no video work.

use std::sync::Arc;

use tlp_desk_capture::BgraFrame;
use tlp_desk_codec::{CodecError, EncodedFrame, EncoderConfig, VideoEncoder};
use tlp_desk_host::EncoderFactory;

use super::{Alive, AliveGuard, Kind};

/// An [`EncoderFactory`] making [`FakeEncoder`]s.
///
/// The viewer cannot decode their output (it is not H.264), so use it for tests that count or
/// inspect frames on the wire, run long soaks cheaply, or exercise the host alone. Tests that
/// need decoded pictures keep the default OpenH264 encoder (software, no hardware needed).
pub fn encoder_factory(alive: Arc<Alive>) -> EncoderFactory {
    Arc::new(move |cfg| Ok(Box::new(FakeEncoder::new(cfg, alive.clone())) as Box<dyn VideoEncoder>))
}

/// Emits a 16-byte "frame" per input: `b"TLPFAKE\0"`, then the frame number (u32 LE), then
/// width and height as u16 LE. The first frame and the one after
/// [`VideoEncoder::request_keyframe`] are keyframes.
#[derive(Debug)]
pub struct FakeEncoder {
    cfg: EncoderConfig,
    n: u32,
    want_keyframe: bool,
    _alive: AliveGuard,
}

impl FakeEncoder {
    /// Marker at the start of every fake frame.
    pub const MAGIC: &'static [u8; 8] = b"TLPFAKE\0";

    /// A fake encoder for frames of `cfg.width` x `cfg.height`.
    pub fn new(cfg: EncoderConfig, alive: Arc<Alive>) -> Self {
        Self {
            cfg,
            n: 0,
            want_keyframe: true,
            _alive: AliveGuard::new(&alive, Kind::Encoder),
        }
    }
}

impl VideoEncoder for FakeEncoder {
    fn encode(&mut self, frame: &BgraFrame) -> Result<Option<EncodedFrame>, CodecError> {
        if (frame.width, frame.height) != (self.cfg.width, self.cfg.height) {
            return Err(CodecError::SizeMismatch {
                got_w: frame.width,
                got_h: frame.height,
                want_w: self.cfg.width,
                want_h: self.cfg.height,
            });
        }
        self.n = self.n.wrapping_add(1);
        let mut data = Self::MAGIC.to_vec();
        data.extend_from_slice(&self.n.to_le_bytes());
        data.extend_from_slice(&u16::try_from(frame.width).unwrap_or(u16::MAX).to_le_bytes());
        data.extend_from_slice(
            &u16::try_from(frame.height)
                .unwrap_or(u16::MAX)
                .to_le_bytes(),
        );
        let keyframe = std::mem::take(&mut self.want_keyframe);
        Ok(Some(EncodedFrame {
            keyframe,
            timestamp_us: frame.timestamp_us,
            data,
        }))
    }

    fn request_keyframe(&mut self) {
        self.want_keyframe = true;
    }

    fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError> {
        self.cfg.bitrate_kbps = kbps;
        Ok(())
    }

    fn config(&self) -> EncoderConfig {
        self.cfg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyframes_on_start_and_on_request() {
        let alive = Arc::new(Alive::default());
        let mut e = FakeEncoder::new(EncoderConfig::for_screen(64, 32, 30), alive.clone());
        let f = BgraFrame::new_tight(64, 32);
        let a = e.encode(&f).unwrap().unwrap();
        let b = e.encode(&f).unwrap().unwrap();
        e.request_keyframe();
        let c = e.encode(&f).unwrap().unwrap();
        assert_eq!((a.keyframe, b.keyframe, c.keyframe), (true, false, true));
        assert!(a.data.starts_with(FakeEncoder::MAGIC));
        assert_eq!(a.data.len(), 16);
        assert!(e.encode(&BgraFrame::new_tight(8, 8)).is_err());
        assert_eq!(alive.encoders(), 1);
    }
}
