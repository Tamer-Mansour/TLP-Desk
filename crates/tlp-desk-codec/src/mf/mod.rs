//! Hardware H.264 encoding through Media Foundation encoder MFTs (Intel Quick Sync, NVIDIA
//! NVENC, AMD AMF all ship one), behind [`VideoEncoder`].
//!
//! The MFT lives on a dedicated worker thread that owns its COM apartment, so callers on any
//! thread can use [`MfEncoder`]. Each frame is converted to NV12 on the calling thread (the
//! same BT.709 limited-range math and the same size policy as the software encoder, so the two
//! are interchangeable mid-stream), handed to the worker, and the coded access unit comes back
//! as Annex-B with SPS/PPS repeated in front of every IDR.
//!
//! Every wait is bounded: a stalled or removed GPU turns into a [`CodecError`] that the
//! selection layer (`crate::select`) answers by switching to OpenH264.

#[allow(unsafe_code)]
mod adapter;
pub(crate) mod annexb;
#[allow(unsafe_code)]
mod mft;

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use tlp_desk_capture::BgraFrame;

pub use adapter::{list_adapters, GpuAdapter};

use crate::convert::{bgra_to_i420, bgra_to_i420_half, even_up, I420Buffer};
use crate::h264::auto_encoded_size;
use crate::{CodecError, EncodedFrame, EncoderConfig, VideoEncoder};
use annexb::ParamSets;
use mft::{Candidate, ComApartment, MfRuntime, Mft, MftParams};

/// PCI vendor id of Intel.
pub const VENDOR_INTEL: u32 = 0x8086;
/// PCI vendor id of NVIDIA.
pub const VENDOR_NVIDIA: u32 = 0x10DE;
/// PCI vendor id of AMD.
pub const VENDOR_AMD: u32 = 0x1002;

/// Which GPU the hardware encoder should run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HwAdapter {
    /// The adapter driving the primary monitor, else any adapter with an encoder.
    #[default]
    Auto,
    /// The adapter driving the monitor that contains this desktop point (use the captured
    /// monitor's top-left corner), else any adapter with an encoder.
    Point { x: i32, y: i32 },
    /// The first adapter from this PCI vendor (see [`VENDOR_INTEL`] etc.); no other is tried.
    Vendor(u32),
    /// Exactly this adapter ([`GpuAdapter::luid`]); no other is tried.
    Luid(i64),
}

impl HwAdapter {
    /// Parses `auto`, `intel`, `nvidia`, `amd` or a hex vendor id like `0x10de`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Some(Self::Auto),
            "intel" => Some(Self::Vendor(VENDOR_INTEL)),
            "nvidia" => Some(Self::Vendor(VENDOR_NVIDIA)),
            "amd" => Some(Self::Vendor(VENDOR_AMD)),
            other => other
                .strip_prefix("0x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .map(Self::Vendor),
        }
    }
}

/// What a hardware encoder is running on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HwEncoderInfo {
    /// The MFT's friendly name.
    pub name: String,
    /// PCI vendor id of the encoder, when the MFT reports it.
    pub vendor_id: Option<u32>,
    /// The adapter it was requested for, when one was chosen.
    pub adapter: Option<String>,
    /// H.264 profile the MFT accepted (empty in [`list_hw_encoders`]).
    pub profile: String,
    /// Rate control the MFT accepted (empty in [`list_hw_encoders`]).
    pub rate_control: String,
}

/// How long the worker may take to bring the MFT up.
const INIT_TIMEOUT: Duration = Duration::from_secs(10);
/// Per-frame budget inside the worker (the first frame also pays for GPU warm-up).
const FRAME_TIMEOUT: Duration = Duration::from_millis(750);
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_millis(2_500);
/// Extra time the caller waits beyond the worker's own deadline.
const REPLY_GRACE: Duration = Duration::from_millis(500);

enum Cmd {
    Encode {
        yuv: I420Buffer,
        timestamp_us: u64,
        keyframe: bool,
        deadline: Instant,
    },
    SetBitrate(u32),
}

enum Reply {
    Encoded {
        result: Result<Option<(Vec<u8>, i64)>, CodecError>,
        yuv: I420Buffer,
    },
    Bitrate(Result<(), CodecError>),
}

/// Hardware H.264 encoder (Media Foundation MFT). Same contract as the software
/// [`H264Encoder`](crate::H264Encoder): Annex-B, no B-frames, IDR on request only, CBR.
pub struct MfEncoder {
    config: EncoderConfig,
    encoded: (u32, u32),
    half: bool,
    info: HwEncoderInfo,
    tx: Sender<Cmd>,
    rx: Receiver<Reply>,
    spare: Option<I420Buffer>,
    params: ParamSets,
    force_keyframe: bool,
    frames: u64,
    /// Why the encoder stopped working (a timeout, a lost worker, an MFT error); every later
    /// call fails fast with this reason, so the fallback's log says what really happened.
    broken: Option<String>,
}

impl MfEncoder {
    /// Opens a hardware encoder on the adapter `adapter` selects.
    ///
    /// # Errors
    /// Fails when no hardware H.264 MFT is available there or it rejects the settings.
    pub fn new(config: EncoderConfig, adapter: HwAdapter) -> Result<Self, CodecError> {
        Self::with_qp(config, adapter, crate::QpBounds::FLOOR)
    }

    /// Like [`MfEncoder::new`] with explicit quantiser limits (vendors that ignore them keep
    /// their own).
    ///
    /// # Errors
    /// As [`MfEncoder::new`].
    pub fn with_qp(
        config: EncoderConfig,
        adapter: HwAdapter,
        qp: crate::QpBounds,
    ) -> Result<Self, CodecError> {
        if config.width == 0 || config.height == 0 {
            return Err(CodecError::Backend("zero-sized encoder config".into()));
        }
        let encoded = auto_encoded_size(config.width, config.height);
        let half = encoded != (even_up(config.width), even_up(config.height));
        let params = MftParams {
            width: encoded.0,
            height: encoded.1,
            fps: config.fps.max(1),
            bitrate_bps: config.bitrate_kbps.max(1).saturating_mul(1000),
            qp,
        };
        let (tx, cmd_rx) = mpsc::channel();
        let (reply_tx, rx) = mpsc::channel();
        let (init_tx, init_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("tlp-mf-encoder".into())
            .spawn(move || worker(params, adapter, cmd_rx, reply_tx, init_tx))
            .map_err(|e| CodecError::Backend(format!("cannot start encoder thread: {e}")))?;
        let info = match init_rx.recv_timeout(INIT_TIMEOUT) {
            Ok(r) => r?,
            Err(_) => {
                return Err(CodecError::Backend(
                    "hardware encoder did not start in time".into(),
                ))
            }
        };
        tracing::info!(
            encoder = %info.name,
            adapter = info.adapter.as_deref().unwrap_or("any"),
            profile = %info.profile,
            rate_control = %info.rate_control,
            width = encoded.0,
            height = encoded.1,
            "hardware H.264 encoder ready"
        );
        Ok(Self {
            config,
            encoded,
            half,
            info,
            tx,
            rx,
            spare: None,
            params: ParamSets::default(),
            force_keyframe: false, // the first frame is always an IDR
            frames: 0,
            broken: None,
        })
    }

    /// The MFT and adapter in use.
    pub fn info(&self) -> &HwEncoderInfo {
        &self.info
    }

    /// Size of the encoded pictures (same policy as the software encoder).
    pub fn encoded_size(&self) -> (u32, u32) {
        self.encoded
    }

    fn broken(&mut self, why: &str) -> CodecError {
        self.broken = Some(why.to_owned());
        CodecError::Backend(format!("hardware encoder lost: {why}"))
    }

    /// The error for calls after the encoder broke, `None` while it works.
    fn unusable(&self) -> Option<CodecError> {
        self.broken.as_ref().map(|why| {
            CodecError::Backend(format!("hardware encoder is not usable (it failed: {why})"))
        })
    }
}

impl VideoEncoder for MfEncoder {
    fn encode(&mut self, frame: &BgraFrame) -> Result<Option<EncodedFrame>, CodecError> {
        if let Some(e) = self.unusable() {
            return Err(e);
        }
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
        let mut yuv = self.spare.take().unwrap_or_default();
        if self.half {
            bgra_to_i420_half(frame, &mut yuv);
        } else {
            bgra_to_i420(frame, &mut yuv);
        }
        let budget = if self.frames == 0 {
            FIRST_FRAME_TIMEOUT
        } else {
            FRAME_TIMEOUT
        };
        let keyframe = std::mem::take(&mut self.force_keyframe);
        let cmd = Cmd::Encode {
            yuv,
            timestamp_us: frame.timestamp_us,
            keyframe,
            deadline: Instant::now() + budget,
        };
        if self.tx.send(cmd).is_err() {
            return Err(self.broken("worker thread exited"));
        }
        let (result, yuv) = match self.rx.recv_timeout(budget + REPLY_GRACE) {
            Ok(Reply::Encoded { result, yuv }) => (result, yuv),
            Ok(Reply::Bitrate(_)) => return Err(self.broken("unexpected reply")),
            Err(RecvTimeoutError::Timeout) => return Err(self.broken("encode timed out")),
            Err(RecvTimeoutError::Disconnected) => return Err(self.broken("worker thread exited")),
        };
        self.spare = Some(yuv);
        self.frames += 1;
        let Some((mut data, time_hns)) =
            result.inspect_err(|e| self.broken = Some(e.to_string()))?
        else {
            return Ok(None);
        };
        let keyframe = self.params.process(&mut data);
        let timestamp_us = u64::try_from(time_hns / 10).unwrap_or(frame.timestamp_us);
        Ok(Some(EncodedFrame {
            keyframe,
            timestamp_us,
            data,
        }))
    }

    fn request_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError> {
        if let Some(e) = self.unusable() {
            return Err(e);
        }
        let kbps = kbps.max(1);
        if self
            .tx
            .send(Cmd::SetBitrate(kbps.saturating_mul(1000)))
            .is_err()
        {
            return Err(self.broken("worker thread exited"));
        }
        match self.rx.recv_timeout(FRAME_TIMEOUT + REPLY_GRACE) {
            Ok(Reply::Bitrate(r)) => r?,
            Ok(Reply::Encoded { .. }) => return Err(self.broken("unexpected reply")),
            Err(_) => return Err(self.broken("bitrate change timed out")),
        }
        self.config.bitrate_kbps = kbps;
        Ok(())
    }

    fn config(&self) -> EncoderConfig {
        self.config
    }

    fn backend_label(&self) -> String {
        format!("hardware: {}", self.info.name)
    }
}

/// The adapter `choice` asks for, and whether other adapters may be tried after it.
fn resolve_adapter(choice: HwAdapter, adapters: &[GpuAdapter]) -> (Option<&GpuAdapter>, bool) {
    match choice {
        HwAdapter::Auto => (adapters.iter().find(|a| a.drives_point(0, 0)), true),
        HwAdapter::Point { x, y } => (adapters.iter().find(|a| a.drives_point(x, y)), true),
        HwAdapter::Vendor(v) => (adapters.iter().find(|a| a.vendor_id == v), false),
        HwAdapter::Luid(l) => (adapters.iter().find(|a| a.luid == l), false),
    }
}

/// Hardware encoder MFTs to try, best first, with the adapter name they were found for.
fn candidates(choice: HwAdapter) -> Result<Vec<(Candidate, Option<String>)>, CodecError> {
    let adapters = list_adapters().unwrap_or_default();
    let (target, any_ok) = resolve_adapter(choice, &adapters);
    let mut out = Vec::new();
    if let Some(a) = target {
        for c in mft::enumerate(Some(a.luid))? {
            out.push((c, Some(a.name.clone())));
        }
    } else if !any_ok {
        return Err(CodecError::Backend(format!(
            "no display adapter matches {choice:?}"
        )));
    }
    if any_ok {
        // Others after the preferred adapter's (skipping ones already listed by name).
        for c in mft::enumerate(None)? {
            if !out.iter().any(|(o, _)| o.name == c.name) {
                out.push((c, None));
            }
        }
    }
    if out.is_empty() {
        return Err(CodecError::Backend(
            "no hardware H.264 encoder found".into(),
        ));
    }
    Ok(out)
}

fn open_first(params: &MftParams, choice: HwAdapter) -> Result<(Mft, HwEncoderInfo), CodecError> {
    let mut last = CodecError::Backend("no hardware H.264 encoder found".into());
    for (cand, adapter) in candidates(choice)? {
        match Mft::create(&cand, params) {
            Ok(m) => {
                let info = HwEncoderInfo {
                    name: cand.name.clone(),
                    vendor_id: cand.vendor_id,
                    adapter,
                    profile: m.profile.to_string(),
                    rate_control: match m.rate_control {
                        mft::RateControl::PeakConstrainedVbr => "peak-constrained VBR".into(),
                        mft::RateControl::Cbr => "CBR".into(),
                    },
                };
                return Ok((m, info));
            }
            Err(e) => {
                tracing::debug!(encoder = %cand.name, error = %e, "hardware encoder rejected");
                last = e;
            }
        }
    }
    Err(last)
}

fn worker(
    params: MftParams,
    choice: HwAdapter,
    cmds: Receiver<Cmd>,
    replies: Sender<Reply>,
    init: Sender<Result<HwEncoderInfo, CodecError>>,
) {
    let _com = ComApartment::enter();
    let _mf = match MfRuntime::start() {
        Ok(r) => r,
        Err(e) => {
            let _ = init.send(Err(e));
            return;
        }
    };
    let mut mft = match open_first(&params, choice) {
        Ok((m, info)) => {
            if init.send(Ok(info)).is_err() {
                return; // the caller gave up waiting
            }
            m
        }
        Err(e) => {
            let _ = init.send(Err(e));
            return;
        }
    };
    // Ends when the encoder is dropped (sender closed); `mft` then shuts down before MF/COM.
    for cmd in cmds {
        let reply = match cmd {
            Cmd::Encode {
                yuv,
                timestamp_us,
                keyframe,
                deadline,
            } => Reply::Encoded {
                result: mft.encode(&yuv, timestamp_us, keyframe, deadline),
                yuv,
            },
            Cmd::SetBitrate(bps) => Reply::Bitrate(mft.set_bitrate(bps)),
        };
        if replies.send(reply).is_err() {
            break;
        }
    }
}

/// Lists the hardware H.264 encoder MFTs on this machine (any adapter), best first.
///
/// # Errors
/// Fails when Media Foundation is unavailable.
pub fn list_hw_encoders() -> Result<Vec<HwEncoderInfo>, CodecError> {
    let probe = std::thread::Builder::new()
        .name("tlp-mf-probe".into())
        .spawn(|| -> Result<Vec<HwEncoderInfo>, CodecError> {
            let _com = ComApartment::enter();
            let _mf = MfRuntime::start()?;
            let adapters = list_adapters().unwrap_or_default();
            let mut out = Vec::new();
            for a in &adapters {
                for c in mft::enumerate(Some(a.luid))? {
                    out.push(HwEncoderInfo {
                        name: c.name,
                        vendor_id: c.vendor_id,
                        adapter: Some(a.name.clone()),
                        profile: String::new(),
                        rate_control: String::new(),
                    });
                }
            }
            Ok(out)
        })
        .map_err(|e| CodecError::Backend(format!("cannot start probe thread: {e}")))?;
    probe
        .join()
        .map_err(|_| CodecError::Backend("encoder probe panicked".into()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(vendor: u32, luid: i64, outputs: Vec<(i32, i32, i32, i32)>) -> GpuAdapter {
        GpuAdapter {
            luid,
            vendor_id: vendor,
            name: format!("gpu {vendor:x}"),
            outputs,
        }
    }

    #[test]
    fn adapter_choice_parses() {
        assert_eq!(HwAdapter::parse("auto"), Some(HwAdapter::Auto));
        assert_eq!(HwAdapter::parse(""), Some(HwAdapter::Auto));
        assert_eq!(
            HwAdapter::parse(" NVIDIA "),
            Some(HwAdapter::Vendor(VENDOR_NVIDIA))
        );
        assert_eq!(
            HwAdapter::parse("intel"),
            Some(HwAdapter::Vendor(VENDOR_INTEL))
        );
        assert_eq!(
            HwAdapter::parse("0x1002"),
            Some(HwAdapter::Vendor(VENDOR_AMD))
        );
        assert_eq!(HwAdapter::parse("gpu"), None);
    }

    #[test]
    fn adapter_resolution_follows_the_monitor() {
        let intel = adapter(VENDOR_INTEL, 1, vec![(0, 0, 1920, 1200)]);
        let nvidia = adapter(VENDOR_NVIDIA, 2, vec![(1920, 0, 3840, 1080)]);
        let all = vec![intel, nvidia];
        let (a, any) = resolve_adapter(HwAdapter::Auto, &all);
        assert_eq!(a.map(|a| a.luid), Some(1));
        assert!(any);
        let (a, _) = resolve_adapter(HwAdapter::Point { x: 2000, y: 10 }, &all);
        assert_eq!(a.map(|a| a.luid), Some(2));
        let (a, any) = resolve_adapter(HwAdapter::Point { x: -5, y: 0 }, &all);
        assert!(
            a.is_none() && any,
            "unknown point falls back to any adapter"
        );
        let (a, any) = resolve_adapter(HwAdapter::Vendor(VENDOR_NVIDIA), &all);
        assert_eq!(a.map(|a| a.luid), Some(2));
        assert!(!any, "an explicit vendor is not silently replaced");
        let (a, _) = resolve_adapter(HwAdapter::Luid(7), &all);
        assert!(a.is_none());
    }
}
