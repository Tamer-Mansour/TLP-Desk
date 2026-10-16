//! H.264 software encoder/decoder backed by OpenH264 (compiled from bundled source).
//!
//! The decoder uses the safe `openh264` wrapper. The encoder drives OpenH264's C API directly
//! (`ffi` below) because the wrapper cannot configure a fixed slice count, which is what lets
//! OpenH264 encode one frame on several threads, nor silence its init-time console logging.

use openh264::decoder::{Decoder, DecoderConfig, Flush};
use openh264::formats::YUVSource;
use openh264::OpenH264API;
use tlp_desk_capture::BgraFrame;

use crate::convert::{
    bgra_to_i420, bgra_to_i420_half, even_up, half_size, i420_to_bgra, I420Buffer, I420Planes,
};
use crate::{CodecError, EncodedFrame, EncoderConfig, VideoDecoder, VideoEncoder};

/// Encoder speed/quality trade-off knobs that are not part of the generic [`EncoderConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct H264Tuning {
    /// Encoder worker threads; each encodes its own slice of the frame. 0 = auto
    /// (half the logical CPUs, at most 4).
    pub threads: u16,
    /// Lower complexity = faster encode, slightly larger output.
    pub complexity: H264Complexity,
    /// Whether to encode at half resolution.
    pub downscale: Downscale,
    /// OpenH264's screen-content mode: noticeably smaller output for text/UI, but 2-3x the
    /// CPU per frame and it does not use multiple threads. Off by default (latency first).
    pub screen_content: bool,
    /// Quantiser limits; [`QpBounds::FLOOR`] by default.
    pub qp: crate::QpBounds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H264Complexity {
    Low,
    Medium,
    High,
}

/// Encoded resolution policy. Decoded frames come out at the encoded size; the viewer scales.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Downscale {
    /// Full resolution unless the source exceeds OpenH264's 3840x2160 limit, then half.
    Auto,
    /// Always full resolution (sources above the limit are rejected).
    Never,
    /// Always half resolution in each direction (2x2 box filter): ~4x cheaper to encode.
    Half,
}

impl Default for H264Tuning {
    fn default() -> Self {
        Self {
            threads: 0,
            complexity: H264Complexity::Low,
            downscale: Downscale::Auto,
            screen_content: false,
            qp: crate::QpBounds::FLOOR,
        }
    }
}

/// OpenH264 rejects pictures larger than this (either orientation).
fn fits_openh264(w: u32, h: u32) -> bool {
    let (w, h) = (even_up(w), even_up(h));
    w.max(h) <= 3840 && w.min(h) <= 2160
}

/// Encoded size for a source under [`Downscale::Auto`].
pub(crate) fn auto_encoded_size(w: u32, h: u32) -> (u32, u32) {
    if fits_openh264(w, h) {
        (even_up(w), even_up(h))
    } else {
        half_size(w, h)
    }
}

fn resolve_threads(requested: u16) -> u16 {
    if requested > 0 {
        return requested.min(16);
    }
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    (cpus / 2).clamp(1, 4) as u16
}

/// H.264 encoder: Annex-B output, constrained-baseline (no B-frames), IDR on request only,
/// bitrate rate control, never skips frames internally.
pub struct H264Encoder {
    config: EncoderConfig,
    tuning: H264Tuning,
    encoded: (u32, u32),
    half: bool,
    raw: ffi::RawEncoder,
    yuv: I420Buffer,
    force_keyframe: bool,
    /// Second OpenH264 instance for refresh pictures (fixed low quantiser), created on first
    /// use. A separate instance because OpenH264 does not reliably take quantiser changes on a
    /// running encoder; the main encoder is told to emit a keyframe after each refresh so the
    /// viewer's reference stays in sync.
    refiner: Option<ffi::RawEncoder>,
}

impl H264Encoder {
    pub fn new(config: EncoderConfig) -> Result<Self, CodecError> {
        Self::with_tuning(config, H264Tuning::default())
    }

    pub fn with_tuning(config: EncoderConfig, tuning: H264Tuning) -> Result<Self, CodecError> {
        if config.width == 0 || config.height == 0 {
            return Err(CodecError::Backend("zero-sized encoder config".into()));
        }
        let half = match tuning.downscale {
            Downscale::Half => true,
            Downscale::Never => false,
            Downscale::Auto => !fits_openh264(config.width, config.height),
        };
        let encoded = if half {
            half_size(config.width, config.height)
        } else {
            (even_up(config.width), even_up(config.height))
        };
        if !fits_openh264(encoded.0, encoded.1) {
            return Err(CodecError::Backend(format!(
                "{}x{} exceeds the OpenH264 limit of 3840x2160",
                encoded.0, encoded.1
            )));
        }
        let threads = if tuning.screen_content {
            1
        } else {
            resolve_threads(tuning.threads)
        };
        let raw = ffi::RawEncoder::new(&ffi::Params {
            width: encoded.0,
            height: encoded.1,
            bitrate_bps: config.bitrate_kbps.max(1).saturating_mul(1000),
            fps: config.fps.max(1),
            threads,
            complexity: tuning.complexity,
            screen_content: tuning.screen_content,
            qp: tuning.qp,
        })?;
        Ok(Self {
            config,
            tuning,
            encoded,
            half,
            raw,
            yuv: I420Buffer::default(),
            force_keyframe: false, // the first frame is always an IDR
            refiner: None,
        })
    }

    /// Size of the encoded (and decoded) pictures: the source size rounded up to even, or half
    /// of it when downscaling.
    pub fn encoded_size(&self) -> (u32, u32) {
        self.encoded
    }

    /// The tuning this encoder was created with.
    pub fn tuning(&self) -> H264Tuning {
        self.tuning
    }
}

impl VideoEncoder for H264Encoder {
    fn encode(&mut self, frame: &BgraFrame) -> Result<Option<EncodedFrame>, CodecError> {
        if frame.width != self.config.width || frame.height != self.config.height {
            return Err(CodecError::SizeMismatch {
                got_w: frame.width,
                got_h: frame.height,
                want_w: self.config.width,
                want_h: self.config.height,
            });
        }
        if !frame.is_well_formed() {
            return Err(CodecError::Backend(
                "BGRA frame buffer too small for its geometry".into(),
            ));
        }
        if self.half {
            bgra_to_i420_half(frame, &mut self.yuv);
        } else {
            bgra_to_i420(frame, &mut self.yuv);
        }
        if self.force_keyframe {
            self.raw.force_idr()?;
            self.force_keyframe = false;
        }
        let mut data = Vec::new();
        let kind = self
            .raw
            .encode(&self.yuv, (frame.timestamp_us / 1000) as i64, &mut data)?;
        if kind == ffi::FrameKind::Skipped || data.is_empty() {
            return Ok(None);
        }
        Ok(Some(EncodedFrame {
            keyframe: kind == ffi::FrameKind::Intra,
            timestamp_us: frame.timestamp_us,
            data,
        }))
    }

    fn request_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    fn encode_refine(&mut self, frame: &BgraFrame) -> Result<Option<EncodedFrame>, CodecError> {
        if frame.width != self.config.width || frame.height != self.config.height {
            return Err(CodecError::SizeMismatch {
                got_w: frame.width,
                got_h: frame.height,
                want_w: self.config.width,
                want_h: self.config.height,
            });
        }
        if !frame.is_well_formed() {
            return Err(CodecError::Backend(
                "BGRA frame buffer too small for its geometry".into(),
            ));
        }
        if self.refiner.is_none() {
            let q = crate::refine::REFINE_QP as u8;
            self.refiner = Some(ffi::RawEncoder::new(&ffi::Params {
                width: self.encoded.0,
                height: self.encoded.1,
                // Effectively unlimited: the quantiser, not the rate, decides the size.
                bitrate_bps: 100_000_000,
                fps: self.config.fps.max(1),
                threads: resolve_threads(self.tuning.threads),
                complexity: H264Complexity::Low,
                screen_content: false,
                qp: crate::QpBounds { min: q, max: q + 1 },
            })?);
        }
        if self.half {
            bgra_to_i420_half(frame, &mut self.yuv);
        } else {
            bgra_to_i420(frame, &mut self.yuv);
        }
        let Some(refiner) = self.refiner.as_mut() else {
            return Ok(None);
        };
        refiner.force_idr()?;
        let mut data = Vec::new();
        let kind = refiner.encode(&self.yuv, (frame.timestamp_us / 1000) as i64, &mut data)?;
        // The viewer now holds this picture, not the main encoder's reference: restart the
        // main stream with a keyframe on the next change.
        self.force_keyframe = true;
        if kind == ffi::FrameKind::Skipped || data.is_empty() {
            return Ok(None);
        }
        Ok(Some(EncodedFrame {
            keyframe: true,
            timestamp_us: frame.timestamp_us,
            data,
        }))
    }

    fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError> {
        let kbps = kbps.max(1);
        self.raw.set_bitrate(kbps.saturating_mul(1000))?;
        self.config.bitrate_kbps = kbps;
        Ok(())
    }

    fn config(&self) -> EncoderConfig {
        self.config
    }

    fn backend_label(&self) -> String {
        "software: OpenH264".into()
    }
}

/// Minimal safe wrapper over OpenH264's `ISVCEncoder` C interface.
#[allow(unsafe_code)]
mod ffi {
    use std::os::raw::{c_int, c_void};
    use std::ptr;

    use openh264_sys2::source::APILoader;
    use openh264_sys2::{
        videoFormatI420, videoFrameTypeI, videoFrameTypeIDR, videoFrameTypeInvalid,
        videoFrameTypeSkip, ISVCEncoder, ISVCEncoderVtbl, SBitrateInfo, SEncParamExt, SFrameBSInfo,
        SSourcePicture, CAMERA_VIDEO_REAL_TIME, CONSTANT_ID, DEBLOCKING_IDC_0, ENCODER_OPTION,
        ENCODER_OPTION_BITRATE, ENCODER_OPTION_DATAFORMAT, ENCODER_OPTION_MAX_BITRATE,
        ENCODER_OPTION_TRACE_LEVEL, HIGH_COMPLEXITY, LOW_COMPLEXITY, MEDIUM_COMPLEXITY,
        RC_BITRATE_MODE, SCREEN_CONTENT_REAL_TIME, SM_FIXEDSLCNUM_SLICE, SM_SINGLE_SLICE,
        SPATIAL_LAYER_0, SPATIAL_LAYER_ALL, WELS_LOG_QUIET,
    };

    use super::H264Complexity;
    use crate::convert::I420Buffer;
    use crate::CodecError;

    pub(super) struct Params {
        pub width: u32,
        pub height: u32,
        pub bitrate_bps: u32,
        pub fps: u32,
        pub threads: u16,
        pub complexity: H264Complexity,
        pub screen_content: bool,
        pub qp: crate::QpBounds,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum FrameKind {
        Intra,
        Predicted,
        Skipped,
    }

    type FnSetOption = unsafe extern "C" fn(*mut ISVCEncoder, ENCODER_OPTION, *mut c_void) -> c_int;
    type FnEncode =
        unsafe extern "C" fn(*mut ISVCEncoder, *const SSourcePicture, *mut SFrameBSInfo) -> c_int;
    type FnForceIntra = unsafe extern "C" fn(*mut ISVCEncoder, bool) -> c_int;
    type FnUninit = unsafe extern "C" fn(*mut ISVCEncoder) -> c_int;

    pub(super) struct RawEncoder {
        enc: *mut ISVCEncoder,
        set_option: FnSetOption,
        encode_frame: FnEncode,
        force_intra: FnForceIntra,
        uninitialize: FnUninit,
        /// Large (128 layer slots); boxed so the encoder struct stays small.
        info: Box<SFrameBSInfo>,
        width: u32,
        height: u32,
        /// Current target in bits per second.
        bitrate: i32,
    }

    // SAFETY: an OpenH264 encoder instance has no thread affinity; it is only ever used through
    // `&mut self`, so it is never accessed from two threads at once.
    unsafe impl Send for RawEncoder {}

    fn check(rc: c_int, what: &str) -> Result<(), CodecError> {
        if rc == 0 {
            Ok(())
        } else {
            Err(CodecError::Backend(format!(
                "OpenH264 {what} failed ({rc})"
            )))
        }
    }

    impl RawEncoder {
        pub(super) fn new(p: &Params) -> Result<Self, CodecError> {
            let mut enc: *mut ISVCEncoder = ptr::null_mut();
            // SAFETY: WelsCreateSVCEncoder writes a fresh encoder pointer into `enc`.
            let rc = unsafe { APILoader::WelsCreateSVCEncoder(&mut enc) };
            check(rc, "create")?;
            if enc.is_null() {
                return Err(CodecError::Backend(
                    "OpenH264 returned a null encoder".into(),
                ));
            }
            // SAFETY: `enc` points at the encoder's vtable pointer, valid until destroy.
            let vtbl: ISVCEncoderVtbl = unsafe { **enc };
            let destroy_on_err = |e: CodecError| {
                // SAFETY: `enc` came from WelsCreateSVCEncoder and is destroyed exactly once
                // (this closure is only used on paths that return without building `Self`).
                unsafe { APILoader::WelsDestroySVCEncoder(enc) };
                e
            };
            let (
                Some(get_default),
                Some(init_ext),
                Some(set_option),
                Some(encode_frame),
                Some(force_intra),
                Some(uninitialize),
            ) = (
                vtbl.GetDefaultParams,
                vtbl.InitializeExt,
                vtbl.SetOption,
                vtbl.EncodeFrame,
                vtbl.ForceIntraFrame,
                vtbl.Uninitialize,
            )
            else {
                return Err(destroy_on_err(CodecError::Backend(
                    "OpenH264 vtable incomplete".into(),
                )));
            };

            // Silence OpenH264's console logging; failures are reported via return codes.
            let mut level: c_int = WELS_LOG_QUIET as c_int;
            // SAFETY: TRACE_LEVEL takes an int* and may be set before initialisation.
            unsafe {
                set_option(
                    enc,
                    ENCODER_OPTION_TRACE_LEVEL,
                    (&mut level as *mut c_int).cast(),
                )
            };

            let mut params = SEncParamExt::default();
            // SAFETY: fills `params` with OpenH264's defaults.
            check(unsafe { get_default(enc, &mut params) }, "GetDefaultParams")
                .map_err(destroy_on_err)?;
            let bitrate = i32::try_from(p.bitrate_bps).unwrap_or(i32::MAX / 2);
            params.iUsageType = if p.screen_content {
                SCREEN_CONTENT_REAL_TIME
            } else {
                CAMERA_VIDEO_REAL_TIME
            };
            params.iPicWidth = p.width as c_int;
            params.iPicHeight = p.height as c_int;
            params.iTargetBitrate = bitrate;
            params.iMaxBitrate = peak_bitrate(bitrate);
            params.iRCMode = RC_BITRATE_MODE;
            params.fMaxFrameRate = p.fps as f32;
            params.iTemporalLayerNum = 1;
            params.iSpatialLayerNum = 1;
            params.iComplexityMode = match p.complexity {
                H264Complexity::Low => LOW_COMPLEXITY,
                H264Complexity::Medium => MEDIUM_COMPLEXITY,
                H264Complexity::High => HIGH_COMPLEXITY,
            };
            params.uiIntraPeriod = 0; // IDR only on request
            params.iNumRefFrame = 1;
            params.eSpsPpsIdStrategy = CONSTANT_ID;
            params.bEnableFrameSkip = false; // the session layer decides what to drop
            params.iMultipleThreadIdc = p.threads;
            params.iLoopFilterDisableIdc = DEBLOCKING_IDC_0 as c_int;
            params.bEnableDenoise = false;
            params.bEnableBackgroundDetection = false;
            params.bEnableAdaptiveQuant = false;
            // OpenH264 requires scene-change detection in screen mode; elsewhere it would turn
            // big content changes into IDRs, which cost latency, so keep it off.
            params.bEnableSceneChangeDetect = p.screen_content;
            params.bEnableLongTermReference = false;
            // Quality floor: rate control never quantises coarser than max_qp (H.264 range
            // 0..=51; OpenH264 itself accepts 0..=51 with min <= max).
            let max_qp = i32::from(p.qp.max.clamp(1, 51));
            params.iMaxQp = max_qp;
            params.iMinQp = i32::from(p.qp.min).clamp(0, max_qp);
            params.iEntropyCodingModeFlag = 0; // CAVLC (constrained baseline)

            let layer = &mut params.sSpatialLayers[0];
            layer.iVideoWidth = p.width as c_int;
            layer.iVideoHeight = p.height as c_int;
            layer.fFrameRate = p.fps as f32;
            layer.iSpatialBitrate = bitrate;
            layer.iMaxSpatialBitrate = peak_bitrate(bitrate);
            if p.threads > 1 {
                layer.sSliceArgument.uiSliceMode = SM_FIXEDSLCNUM_SLICE;
                layer.sSliceArgument.uiSliceNum = p.threads as u32;
            } else {
                layer.sSliceArgument.uiSliceMode = SM_SINGLE_SLICE;
                layer.sSliceArgument.uiSliceNum = 1;
            }
            // VUI: BT.709, limited range (matches crate::convert).
            layer.bVideoSignalTypePresent = true;
            layer.uiVideoFormat = 5; // unspecified
            layer.bFullRange = false;
            layer.bColorDescriptionPresent = true;
            layer.uiColorPrimaries = 1;
            layer.uiTransferCharacteristics = 1;
            layer.uiColorMatrix = 1;

            // SAFETY: `params` is fully initialised (defaults + overrides) and outlives the call.
            check(unsafe { init_ext(enc, &params) }, "InitializeExt").map_err(destroy_on_err)?;

            let raw = Self {
                enc,
                set_option,
                encode_frame,
                force_intra,
                uninitialize,
                info: Box::default(),
                width: p.width,
                height: p.height,
                bitrate,
            };
            // From here on `raw`'s Drop owns cleanup.
            let mut format: c_int = videoFormatI420 as c_int;
            // SAFETY: DATAFORMAT takes an int*; the encoder is initialised.
            let rc = unsafe {
                set_option(
                    raw.enc,
                    ENCODER_OPTION_DATAFORMAT,
                    (&mut format as *mut c_int).cast(),
                )
            };
            check(rc, "SetOption(DATAFORMAT)")?;
            Ok(raw)
        }

        /// Encodes one I420 picture (must match the configured size), appending Annex-B NAL
        /// units to `out`.
        pub(super) fn encode(
            &mut self,
            yuv: &I420Buffer,
            timestamp_ms: i64,
            out: &mut Vec<u8>,
        ) -> Result<FrameKind, CodecError> {
            let (w, h) = (yuv.width, yuv.height);
            if w != self.width as usize
                || h != self.height as usize
                || yuv.y.len() < w * h
                || yuv.u.len() < w * h / 4
                || yuv.v.len() < w * h / 4
            {
                return Err(CodecError::Backend(
                    "I420 buffer does not match encoder".into(),
                ));
            }
            let pic = SSourcePicture {
                iColorFormat: videoFormatI420 as c_int,
                iStride: [w as c_int, (w / 2) as c_int, (w / 2) as c_int, 0],
                // OpenH264 only reads the source planes; the mut cast is an API artefact.
                pData: [
                    yuv.y.as_ptr().cast_mut(),
                    yuv.u.as_ptr().cast_mut(),
                    yuv.v.as_ptr().cast_mut(),
                    ptr::null_mut(),
                ],
                iPicWidth: w as c_int,
                iPicHeight: h as c_int,
                uiTimeStamp: timestamp_ms,
                bPsnrY: false,
                bPsnrU: false,
                bPsnrV: false,
            };
            // SAFETY: `pic` describes planes of at least w*h and (w/2)*(h/2) bytes (checked
            // above) that stay borrowed for the call; `info` is a valid output struct we own.
            let rc = unsafe { (self.encode_frame)(self.enc, &pic, &mut *self.info) };
            check(rc, "EncodeFrame")?;
            let info = &*self.info;
            let kind = match info.eFrameType {
                t if t == videoFrameTypeIDR || t == videoFrameTypeI => FrameKind::Intra,
                t if t == videoFrameTypeSkip || t == videoFrameTypeInvalid => {
                    return Ok(FrameKind::Skipped)
                }
                _ => FrameKind::Predicted,
            };
            let layers = (info.iLayerNum.max(0) as usize).min(info.sLayerInfo.len());
            for layer in &info.sLayerInfo[..layers] {
                if layer.pBsBuf.is_null() || layer.pNalLengthInByte.is_null() {
                    continue;
                }
                let nals = layer.iNalCount.max(0) as usize;
                // SAFETY: OpenH264 guarantees `pNalLengthInByte` has `iNalCount` entries and
                // `pBsBuf` holds their concatenation (start codes included) until the next
                // EncodeFrame call; we copy out before returning.
                let lens = unsafe { std::slice::from_raw_parts(layer.pNalLengthInByte, nals) };
                let total: usize = lens.iter().map(|&l| l.max(0) as usize).sum();
                // SAFETY: as above, `pBsBuf` has `total` valid bytes.
                let bytes = unsafe { std::slice::from_raw_parts(layer.pBsBuf, total) };
                out.extend_from_slice(bytes);
            }
            Ok(kind)
        }

        pub(super) fn force_idr(&mut self) -> Result<(), CodecError> {
            // SAFETY: the encoder is initialised; ForceIntraFrame only sets a flag.
            check(
                unsafe { (self.force_intra)(self.enc, true) },
                "ForceIntraFrame",
            )
        }

        fn set_bitrate_option(
            &mut self,
            option: ENCODER_OPTION,
            layer: c_int,
            bps: i32,
        ) -> Result<(), CodecError> {
            let mut info = SBitrateInfo {
                iLayer: layer,
                iBitrate: bps,
            };
            // SAFETY: these option ids take an `SBitrateInfo*`; `info` outlives the call.
            let rc = unsafe {
                (self.set_option)(self.enc, option, (&mut info as *mut SBitrateInfo).cast())
            };
            check(rc, "SetOption(bitrate)")
        }

        /// Changes the target bitrate mid-stream (no keyframe). OpenH264 requires the layer's
        /// max bitrate to stay above the target at every step, so the order depends on the
        /// direction of the change.
        pub(super) fn set_bitrate(&mut self, bps: u32) -> Result<(), CodecError> {
            let target = i32::try_from(bps).unwrap_or(i32::MAX / 2);
            let max = peak_bitrate(target);
            if target >= self.bitrate {
                self.set_bitrate_option(ENCODER_OPTION_MAX_BITRATE, SPATIAL_LAYER_0, max)?;
                self.set_bitrate_option(ENCODER_OPTION_BITRATE, SPATIAL_LAYER_ALL, target)?;
            } else {
                self.set_bitrate_option(ENCODER_OPTION_BITRATE, SPATIAL_LAYER_ALL, target)?;
                self.set_bitrate_option(ENCODER_OPTION_MAX_BITRATE, SPATIAL_LAYER_0, max)?;
            }
            self.bitrate = target;
            Ok(())
        }
    }

    /// Peak (max) bitrate allowed for a target: 1.5x, as OpenH264 undershoots when max == target.
    fn peak_bitrate(target: i32) -> i32 {
        target.saturating_add(target / 2)
    }

    impl Drop for RawEncoder {
        fn drop(&mut self) {
            // SAFETY: `enc` is initialised and owned by us; uninitialise then destroy, once.
            unsafe {
                (self.uninitialize)(self.enc);
                APILoader::WelsDestroySVCEncoder(self.enc);
            }
        }
    }
}

/// H.264 decoder producing tight-stride BGRA frames (alpha = 255).
pub struct H264Decoder {
    decoder: Decoder,
    spare: Option<Vec<u8>>,
}

impl H264Decoder {
    pub fn new() -> Result<Self, CodecError> {
        let decoder = Decoder::with_api_config(
            OpenH264API::from_source(),
            DecoderConfig::new().flush_after_decode(Flush::NoFlush),
        )
        .map_err(|e| CodecError::Backend(e.to_string()))?;
        Ok(Self {
            decoder,
            spare: None,
        })
    }
}

impl VideoDecoder for H264Decoder {
    fn decode(&mut self, data: &[u8]) -> Result<Option<BgraFrame>, CodecError> {
        let Some(yuv) = self
            .decoder
            .decode(data)
            .map_err(|_| CodecError::BadBitstream)?
        else {
            return Ok(None);
        };
        let (w, h) = yuv.dimensions();
        let (ys, us, _) = yuv.strides();
        let ts = yuv.timestamp().as_millis() * 1000;
        let planes = I420Planes {
            width: w,
            height: h,
            y: yuv.y(),
            u: yuv.u(),
            v: yuv.v(),
            y_stride: ys,
            uv_stride: us,
        };
        let mut out = BgraFrame {
            width: 0,
            height: 0,
            stride: 0,
            data: self.spare.take().unwrap_or_default(),
            timestamp_us: ts,
        };
        i420_to_bgra(planes, &mut out);
        Ok(Some(out))
    }

    fn recycle(&mut self, frame: BgraFrame) {
        self.spare = Some(frame.data);
    }
}
