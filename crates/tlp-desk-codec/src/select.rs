//! Encoder selection: hardware first, OpenH264 as the always-available fallback.
//!
//! [`new_encoder`](crate::new_encoder) reads [`EncoderOptions::from_env`] and builds a
//! [`FallbackEncoder`]. With [`EncoderPreference::Auto`] a hardware encoder that fails to open
//! is skipped, and one that fails mid-stream is replaced by OpenH264 for the rest of the
//! encoder's life: the failed frame is re-encoded in software as an IDR, so the viewer sees
//! one keyframe and no gap.

use std::str::FromStr;

use tlp_desk_capture::BgraFrame;

use crate::{CodecError, EncodedFrame, EncoderConfig, H264Encoder, VideoEncoder};

/// Environment variable that overrides the encoder choice: `auto`, `hardware` or `software`.
pub const ENCODER_ENV: &str = "TLP_DESK_ENCODER";
/// Environment variable that picks the GPU for hardware encoding: `auto`, `intel`, `nvidia`,
/// `amd` or a hex PCI vendor id such as `0x10de`.
pub const ADAPTER_ENV: &str = "TLP_DESK_HW_ADAPTER";

/// Which encoder implementation to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EncoderPreference {
    /// Hardware when it works, software otherwise (also after a mid-stream failure).
    #[default]
    Auto,
    /// Hardware only: creation fails when no hardware encoder opens. Mid-stream failures still
    /// fall back to software, so a running session never dies.
    Hardware,
    /// OpenH264 only.
    Software,
}

impl FromStr for EncoderPreference {
    type Err = CodecError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Ok(Self::Auto),
            "hw" | "hardware" => Ok(Self::Hardware),
            "sw" | "software" => Ok(Self::Software),
            other => Err(CodecError::Backend(format!(
                "unknown encoder preference {other:?} (expected auto, hardware or software)"
            ))),
        }
    }
}

/// Encoder selection settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EncoderOptions {
    /// Hardware, software or automatic.
    pub preference: EncoderPreference,
    /// GPU for the hardware encoder (Windows).
    #[cfg(windows)]
    pub adapter: crate::mf::HwAdapter,
}

impl EncoderOptions {
    /// Defaults overridden by [`ENCODER_ENV`] and [`ADAPTER_ENV`]; unknown values are logged
    /// and ignored.
    pub fn from_env() -> Self {
        let mut o = Self::default();
        if let Ok(v) = std::env::var(ENCODER_ENV) {
            match v.parse() {
                Ok(p) => o.preference = p,
                Err(e) => tracing::warn!(%e, "ignoring {ENCODER_ENV}"),
            }
        }
        #[cfg(windows)]
        if let Ok(v) = std::env::var(ADAPTER_ENV) {
            match crate::mf::HwAdapter::parse(&v) {
                Some(a) => o.adapter = a,
                None => tracing::warn!(value = %v, "ignoring unknown {ADAPTER_ENV}"),
            }
        }
        o
    }
}

/// Opens the hardware encoder for `options`, if this platform has one.
#[cfg(windows)]
fn open_hardware(
    config: EncoderConfig,
    options: &EncoderOptions,
) -> Result<Box<dyn VideoEncoder>, CodecError> {
    Ok(Box::new(crate::mf::MfEncoder::new(
        config,
        options.adapter,
    )?))
}

#[cfg(not(windows))]
fn open_hardware(
    _config: EncoderConfig,
    _options: &EncoderOptions,
) -> Result<Box<dyn VideoEncoder>, CodecError> {
    Err(CodecError::Backend(
        "hardware encoding is only implemented on Windows".into(),
    ))
}

/// Creates an encoder as `options` asks, wrapped in a [`FallbackEncoder`].
///
/// # Errors
/// Fails when the software encoder cannot be created, or with
/// [`EncoderPreference::Hardware`] when no hardware encoder opens.
pub fn new_encoder_with(
    config: EncoderConfig,
    options: &EncoderOptions,
) -> Result<Box<dyn VideoEncoder>, CodecError> {
    let primary = match options.preference {
        EncoderPreference::Software => None,
        EncoderPreference::Auto => match open_hardware(config, options) {
            Ok(enc) => Some(enc),
            Err(e) => {
                tracing::info!(%e, "hardware encoder unavailable, using OpenH264");
                None
            }
        },
        EncoderPreference::Hardware => Some(open_hardware(config, options)?),
    };
    Ok(Box::new(FallbackEncoder::new(config, primary)?))
}

/// Runs a primary (hardware) encoder and swaps in OpenH264 the moment it fails.
pub struct FallbackEncoder {
    config: EncoderConfig,
    active: Box<dyn VideoEncoder>,
    /// Whether `active` is still the primary encoder.
    on_primary: bool,
}

impl FallbackEncoder {
    /// Uses `primary` until it fails; with `None`, starts on OpenH264 directly.
    ///
    /// # Errors
    /// Fails when `primary` is `None` and OpenH264 cannot be created.
    pub fn new(
        config: EncoderConfig,
        primary: Option<Box<dyn VideoEncoder>>,
    ) -> Result<Self, CodecError> {
        let (active, on_primary) = match primary {
            Some(p) => (p, true),
            None => (
                Box::new(H264Encoder::new(config)?) as Box<dyn VideoEncoder>,
                false,
            ),
        };
        Ok(Self {
            config,
            active,
            on_primary,
        })
    }

    /// Whether the primary (hardware) encoder is still in use.
    pub fn on_primary(&self) -> bool {
        self.on_primary
    }

    /// Replaces a failed primary with OpenH264 at the current bitrate; the next frame is an
    /// IDR. Returns the error to report when even that fails.
    fn fall_back(&mut self, cause: &CodecError) -> Result<(), CodecError> {
        tracing::warn!(
            encoder = %self.active.backend_label(),
            error = %cause,
            "hardware encoder failed, switching to OpenH264"
        );
        let mut sw = H264Encoder::new(self.config)?;
        sw.request_keyframe();
        self.active = Box::new(sw);
        self.on_primary = false;
        Ok(())
    }
}

impl VideoEncoder for FallbackEncoder {
    fn encode(&mut self, frame: &BgraFrame) -> Result<Option<EncodedFrame>, CodecError> {
        match self.active.encode(frame) {
            Ok(out) => Ok(out),
            // A wrong-size frame is the caller's mistake, not an encoder failure.
            Err(e @ CodecError::SizeMismatch { .. }) => Err(e),
            Err(e) if self.on_primary => {
                self.fall_back(&e)?;
                self.active.encode(frame)
            }
            Err(e) => Err(e),
        }
    }

    fn request_keyframe(&mut self) {
        self.active.request_keyframe();
    }

    fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError> {
        let kbps = kbps.max(1);
        self.config.bitrate_kbps = kbps;
        match self.active.set_bitrate(kbps) {
            Ok(()) => Ok(()),
            Err(e) if self.on_primary => {
                self.fall_back(&e)?;
                self.active.set_bitrate(kbps)
            }
            Err(e) => Err(e),
        }
    }

    fn config(&self) -> EncoderConfig {
        self.config
    }

    fn backend_label(&self) -> String {
        self.active.backend_label()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{H264Decoder, VideoDecoder};

    fn frame(w: u32, h: u32, t: u32) -> BgraFrame {
        let mut f = BgraFrame::new_tight(w, h);
        f.timestamp_us = u64::from(t) * 16_667;
        for y in 0..h {
            for (x, px) in f.row_mut(y).chunks_exact_mut(4).enumerate() {
                let x = x as u32;
                px.copy_from_slice(&[(x + t) as u8, y as u8, (x ^ y) as u8, 255]);
            }
        }
        f
    }

    fn cfg(w: u32, h: u32) -> EncoderConfig {
        EncoderConfig {
            width: w,
            height: h,
            fps: 30,
            bitrate_kbps: 2_000,
        }
    }

    /// Stands in for a hardware encoder: works for `ok_frames` frames, then fails.
    struct FlakyEncoder {
        config: EncoderConfig,
        inner: H264Encoder,
        ok_frames: u32,
        fail_bitrate: bool,
    }

    impl VideoEncoder for FlakyEncoder {
        fn encode(&mut self, f: &BgraFrame) -> Result<Option<EncodedFrame>, CodecError> {
            if self.ok_frames == 0 {
                return Err(CodecError::Backend("simulated device removal".into()));
            }
            self.ok_frames -= 1;
            self.inner.encode(f)
        }
        fn request_keyframe(&mut self) {
            self.inner.request_keyframe();
        }
        fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError> {
            if self.fail_bitrate {
                return Err(CodecError::Backend("simulated bitrate failure".into()));
            }
            self.inner.set_bitrate(kbps)
        }
        fn config(&self) -> EncoderConfig {
            self.config
        }
        fn backend_label(&self) -> String {
            "flaky".into()
        }
    }

    fn flaky(c: EncoderConfig, ok_frames: u32, fail_bitrate: bool) -> Box<dyn VideoEncoder> {
        Box::new(FlakyEncoder {
            config: c,
            inner: H264Encoder::new(c).unwrap(),
            ok_frames,
            fail_bitrate,
        })
    }

    #[test]
    fn preference_parses() {
        assert_eq!(
            "auto".parse::<EncoderPreference>().unwrap(),
            EncoderPreference::Auto
        );
        assert_eq!(
            " HW ".parse::<EncoderPreference>().unwrap(),
            EncoderPreference::Hardware
        );
        assert_eq!(
            "hardware".parse::<EncoderPreference>().unwrap(),
            EncoderPreference::Hardware
        );
        assert_eq!(
            "software".parse::<EncoderPreference>().unwrap(),
            EncoderPreference::Software
        );
        assert!("gpu".parse::<EncoderPreference>().is_err());
    }

    #[test]
    fn software_preference_uses_openh264() {
        let options = EncoderOptions {
            preference: EncoderPreference::Software,
            ..EncoderOptions::default()
        };
        let enc = new_encoder_with(cfg(64, 48), &options).unwrap();
        assert!(
            enc.backend_label().contains("OpenH264"),
            "{}",
            enc.backend_label()
        );
    }

    #[test]
    fn mid_stream_failure_falls_back_with_a_keyframe() {
        let (w, h) = (160, 96);
        let c = cfg(w, h);
        let mut enc = FallbackEncoder::new(c, Some(flaky(c, 3, false))).unwrap();
        let mut dec = H264Decoder::new().unwrap();
        for t in 0..8 {
            let pkt = enc
                .encode(&frame(w, h, t))
                .unwrap()
                .expect("one packet per frame");
            if t == 3 {
                assert!(!enc.on_primary(), "switched to software at the failure");
                assert!(pkt.keyframe, "first software frame must be an IDR");
                // A fresh decoder (viewer re-sync) can start from it.
                dec = H264Decoder::new().unwrap();
            }
            if t > 3 {
                assert!(!pkt.keyframe);
            }
            let out = dec.decode(&pkt.data).unwrap().expect("picture");
            assert_eq!((out.width, out.height), (w, h));
        }
        assert!(enc.backend_label().contains("OpenH264"));
    }

    #[test]
    fn creation_failure_starts_on_software() {
        let c = cfg(64, 48);
        let mut enc = FallbackEncoder::new(c, None).unwrap();
        assert!(!enc.on_primary());
        assert!(enc.encode(&frame(64, 48, 0)).unwrap().unwrap().keyframe);
    }

    #[test]
    fn bitrate_failure_falls_back_and_keeps_the_new_rate() {
        let c = cfg(64, 48);
        let mut enc = FallbackEncoder::new(c, Some(flaky(c, 10, true))).unwrap();
        enc.set_bitrate(750).unwrap();
        assert!(!enc.on_primary());
        assert_eq!(enc.config().bitrate_kbps, 750);
        assert!(enc.encode(&frame(64, 48, 0)).unwrap().unwrap().keyframe);
    }

    #[test]
    fn size_mismatch_does_not_trigger_fallback() {
        let c = cfg(64, 48);
        let mut enc = FallbackEncoder::new(c, Some(flaky(c, 10, false))).unwrap();
        assert!(matches!(
            enc.encode(&frame(32, 32, 0)),
            Err(CodecError::SizeMismatch { .. })
        ));
        assert!(enc.on_primary());
    }
}
