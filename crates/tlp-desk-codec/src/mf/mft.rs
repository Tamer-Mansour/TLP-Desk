//! Media Foundation H.264 encoder MFT driven through the asynchronous event model.
//!
//! Everything here runs on the encoder's worker thread (see `super::worker`), inside a COM
//! multithreaded apartment with Media Foundation started. Hardware encoder MFTs are always
//! asynchronous: they announce `METransformNeedInput` when they accept a frame and
//! `METransformHaveOutput` when a coded frame is ready. Events are polled (never a blocking
//! `GetEvent`), so a wedged driver turns into a timeout error instead of a hung thread.

use std::collections::VecDeque;
use std::mem::ManuallyDrop;
use std::time::{Duration, Instant};

use windows::core::{Interface, GUID, PWSTR};
use windows::Win32::Media::MediaFoundation::{
    eAVEncCommonRateControlMode_CBR, eAVEncCommonRateControlMode_PeakConstrainedVBR,
    eAVEncH264VProfile_Base, eAVEncH264VProfile_ConstrainedBase, eAVEncH264VProfile_Main,
    CODECAPI_AVEncCommonMaxBitRate, CODECAPI_AVEncCommonMeanBitRate,
    CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncH264CABACEnable,
    CODECAPI_AVEncMPVDefaultBPictureCount, CODECAPI_AVEncMPVGOPSize,
    CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVEncVideoMaxQP, CODECAPI_AVEncVideoMinQP,
    CODECAPI_AVLowLatencyMode, ICodecAPI, IMFActivate, IMFAttributes, IMFMediaEvent,
    IMFMediaEventGenerator, IMFMediaType, IMFSample, IMFTransform, MEError, METransformHaveOutput,
    METransformNeedInput, MFCreateAttributes, MFCreateMediaType, MFCreateMemoryBuffer,
    MFCreateSample, MFMediaType_Video, MFNominalRange_16_235, MFShutdown, MFStartup, MFTEnum2,
    MFT_ENUM_HARDWARE_VENDOR_ID_Attribute, MFT_FRIENDLY_NAME_Attribute, MFVideoFormat_H264,
    MFVideoFormat_NV12, MFVideoInterlace_Progressive, MFVideoTransferMatrix_BT709, MFSTARTUP_LITE,
    MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_ADAPTER_LUID, MFT_ENUM_FLAG_HARDWARE,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_END_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFT_REGISTER_TYPE_INFO, MF_EVENT_FLAG_NO_WAIT, MF_E_NO_EVENTS_AVAILABLE,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY,
    MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_MT_VIDEO_NOMINAL_RANGE,
    MF_MT_YUV_MATRIX, MF_TRANSFORM_ASYNC, MF_TRANSFORM_ASYNC_UNLOCK, MF_VERSION,
};
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED,
};
use windows::Win32::System::Variant::VARIANT;

use crate::convert::{i420_to_nv12, nv12_len, I420Buffer};
use crate::CodecError;

fn mf_err(what: &str, e: windows::core::Error) -> CodecError {
    CodecError::Backend(format!("media foundation {what} failed: {e}"))
}

/// COM multithreaded apartment for the current thread, left on drop if we entered it.
pub(crate) struct ComApartment {
    entered: bool,
}

impl ComApartment {
    pub(crate) fn enter() -> Self {
        // SAFETY: initialises COM for this thread only; balanced by CoUninitialize in Drop when
        // it succeeded (S_OK or S_FALSE). RPC_E_CHANGED_MODE leaves the thread as it was.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Self {
            entered: hr.is_ok(),
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.entered {
            // SAFETY: balances the successful CoInitializeEx on this same thread (the guard is
            // created and dropped on the worker thread and is not Send).
            unsafe { CoUninitialize() };
        }
    }
}

/// Media Foundation started for the lifetime of the guard (calls are reference counted).
pub(crate) struct MfRuntime;

impl MfRuntime {
    pub(crate) fn start() -> Result<Self, CodecError> {
        // SAFETY: MFStartup is reference counted and balanced by MFShutdown in Drop.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) }.map_err(|e| mf_err("MFStartup", e))?;
        Ok(Self)
    }
}

impl Drop for MfRuntime {
    fn drop(&mut self) {
        // SAFETY: balances the successful MFStartup in `start`.
        let _ = unsafe { MFShutdown() };
    }
}

/// A hardware encoder MFT that can be activated.
pub(crate) struct Candidate {
    activate: IMFActivate,
    /// Friendly name, e.g. "Intel® Quick Sync Video H.264 Encoder MFT".
    pub name: String,
    /// PCI vendor id from `MFT_ENUM_HARDWARE_VENDOR_ID_Attribute` ("VEN_8086").
    pub vendor_id: Option<u32>,
}

fn attr_string(attrs: &IMFAttributes, key: &GUID) -> Option<String> {
    let mut p = PWSTR::null();
    let mut len = 0u32;
    // SAFETY: on success `p` is a CoTaskMem-allocated, NUL-terminated string of `len` UTF-16
    // units which we copy and then free exactly once.
    unsafe { attrs.GetAllocatedString(key, &mut p, &mut len) }.ok()?;
    if p.is_null() {
        return None;
    }
    // SAFETY: `p` is valid for `len` units (see above).
    let s = unsafe { String::from_utf16_lossy(std::slice::from_raw_parts(p.0, len as usize)) };
    // SAFETY: `p` was allocated by GetAllocatedString with CoTaskMemAlloc.
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    Some(s)
}

/// Parses "VEN_10DE" (optionally followed by more text) into 0x10DE.
pub(crate) fn parse_vendor(s: &str) -> Option<u32> {
    let hex = s.strip_prefix("VEN_")?;
    let hex = hex.get(..4)?;
    u32::from_str_radix(hex, 16).ok()
}

/// Lists hardware H.264 encoder MFTs (NV12 in), best first, optionally only those on the
/// adapter with `luid`.
pub(crate) fn enumerate(luid: Option<i64>) -> Result<Vec<Candidate>, CodecError> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let mut attrs: Option<IMFAttributes> = None;
    // SAFETY: creates an empty attribute store into a live out-pointer.
    unsafe { MFCreateAttributes(&mut attrs, 1) }.map_err(|e| mf_err("MFCreateAttributes", e))?;
    let attrs = attrs.ok_or_else(|| CodecError::Backend("no attribute store".into()))?;
    if let Some(luid) = luid {
        // SAFETY: MFT_ENUM_ADAPTER_LUID takes the 8-byte LUID as a blob; the slice outlives
        // the call.
        unsafe { attrs.SetBlob(&MFT_ENUM_ADAPTER_LUID, &luid.to_le_bytes()) }
            .map_err(|e| mf_err("SetBlob(adapter luid)", e))?;
    }
    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: the type infos and attributes outlive the call; on success `list` points at
    // `count` activation objects allocated with CoTaskMemAlloc, which we take and free below.
    unsafe {
        MFTEnum2(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &attrs,
            &mut list,
            &mut count,
        )
    }
    .map_err(|e| mf_err("MFTEnum2", e))?;
    let mut out = Vec::new();
    if list.is_null() {
        return Ok(out);
    }
    // SAFETY: MFTEnum2 returned `count` initialised entries at `list`; taking each Option moves
    // its reference out so the array can be freed without releasing them twice.
    let slots = unsafe { std::slice::from_raw_parts_mut(list, count as usize) };
    for slot in slots.iter_mut() {
        if let Some(activate) = slot.take() {
            let name = attr_string(&activate, &MFT_FRIENDLY_NAME_Attribute)
                .unwrap_or_else(|| "hardware H.264 encoder".into());
            let vendor_id = attr_string(&activate, &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute)
                .and_then(|v| parse_vendor(&v));
            out.push(Candidate {
                activate,
                name,
                vendor_id,
            });
        }
    }
    // SAFETY: the array came from MFTEnum2 (CoTaskMemAlloc) and every entry was moved out.
    unsafe { CoTaskMemFree(Some(list as *const _)) };
    Ok(out)
}

/// Encoder geometry and rate settings.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MftParams {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_bps: u32,
    pub qp: crate::QpBounds,
}

/// An activated, configured, streaming encoder MFT.
pub(crate) struct Mft {
    activate: IMFActivate,
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    codec: ICodecAPI,
    in_id: u32,
    out_id: u32,
    provides_samples: bool,
    out_size: u32,
    width: u32,
    height: u32,
    frame_hns: i64,
    need_input: u32,
    /// Coded frames (Annex-B, sample time in 100 ns) not yet handed out.
    ready: VecDeque<(Vec<u8>, i64)>,
    /// H.264 profile the MFT accepted.
    pub profile: &'static str,
    /// Rate control the MFT accepted.
    pub rate_control: RateControl,
    /// Peak bitrate currently configured (VBR only).
    max_bps: u32,
}

fn set_codec(codec: &ICodecAPI, api: &GUID, value: VARIANT) -> windows::core::Result<()> {
    // SAFETY: `api` and `value` outlive the call; the MFT copies the value.
    unsafe { codec.SetValue(api, &value) }
}

/// Rate control mode in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RateControl {
    /// Mean bitrate with a peak (1.5x the initial mean, the OpenH264 path's headroom).
    PeakConstrainedVbr,
    /// Constant bitrate.
    Cbr,
}

impl RateControl {
    /// The mode that behaves best on each vendor's MFT, measured on an Intel UHD (Tiger Lake)
    /// and an RTX A3000 with 1280x720@60 synthetic desktop frames:
    ///
    /// * Intel Quick Sync in CBR pads every frame up to the target (40 KB/frame at 20 Mbps for
    ///   a mostly static screen) and ignores mid-stream changes; peak-constrained VBR spends
    ///   3-4 KB/frame and follows a new mean within a frame.
    /// * NVIDIA ignores a new mean in VBR; in CBR it does not pad and follows the change at the
    ///   next frame.
    /// * Others: CBR, the mode every H.264 encoder MFT supports.
    fn preferred(vendor_id: Option<u32>) -> Self {
        if vendor_id == Some(super::VENDOR_INTEL) {
            Self::PeakConstrainedVbr
        } else {
            Self::Cbr
        }
    }

    fn mode(self) -> u32 {
        match self {
            Self::PeakConstrainedVbr => eAVEncCommonRateControlMode_PeakConstrainedVBR.0 as u32,
            Self::Cbr => eAVEncCommonRateControlMode_CBR.0 as u32,
        }
    }
}

/// Peak bitrate allowed for a mean target.
fn peak(bps: u32) -> u32 {
    bps.saturating_add(bps / 2)
}

/// Sets the mean bitrate (valid before and during streaming), and for VBR raises the peak when
/// the mean would exceed it. The peak is otherwise left alone: changing it mid-stream makes
/// Intel's MFT restart with an IDR.
fn apply_bitrate(
    codec: &ICodecAPI,
    rc: RateControl,
    bps: u32,
    max_bps: &mut u32,
) -> Result<(), CodecError> {
    if rc == RateControl::PeakConstrainedVbr && bps > *max_bps {
        // Optional: without it the MFT picks its own peak.
        if set_codec(codec, &CODECAPI_AVEncCommonMaxBitRate, peak(bps).into()).is_ok() {
            *max_bps = peak(bps);
        }
    }
    set_codec(codec, &CODECAPI_AVEncCommonMeanBitRate, bps.into())
        .map_err(|e| mf_err("mean bitrate", e))
}

fn pack(hi: u32, lo: u32) -> u64 {
    (u64::from(hi) << 32) | u64::from(lo)
}

fn video_type(subtype: &GUID, p: &MftParams) -> windows::core::Result<IMFMediaType> {
    // SAFETY: creates an empty media type and sets plain values on it.
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(p.width, p.height))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(p.fps.max(1), 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        // BT.709 limited range, matching crate::convert (the decoder side assumes the same).
        t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        Ok(t)
    }
}

/// Profiles in order of preference. The viewer decodes with OpenH264, whose decoder targets
/// (constrained) baseline; Main without B-frames is the last resort for MFTs that refuse
/// baseline, and CABAC is switched off where the MFT allows it.
const PROFILES: [(i32, &str); 3] = [
    (eAVEncH264VProfile_ConstrainedBase.0, "constrained baseline"),
    (eAVEncH264VProfile_Base.0, "baseline"),
    (eAVEncH264VProfile_Main.0, "main"),
];

impl Mft {
    /// Activates and configures `cand` for `p`, ready for the first frame.
    pub(crate) fn create(cand: &Candidate, p: &MftParams) -> Result<Self, CodecError> {
        // SAFETY: ActivateObject creates the MFT; the returned interface is owned.
        let transform: IMFTransform =
            unsafe { cand.activate.ActivateObject() }.map_err(|e| mf_err("ActivateObject", e))?;
        let built = Self::configure(cand, transform.clone(), p);
        if built.is_err() {
            // SAFETY: releases the MFT created above; it is not used afterwards.
            let _ = unsafe { cand.activate.ShutdownObject() };
        }
        built
    }

    fn configure(
        cand: &Candidate,
        transform: IMFTransform,
        p: &MftParams,
    ) -> Result<Self, CodecError> {
        // SAFETY: plain attribute reads/writes on the live MFT.
        let attrs = unsafe { transform.GetAttributes() }.map_err(|e| mf_err("GetAttributes", e))?;
        // SAFETY: as above.
        let is_async = unsafe { attrs.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or(0) != 0;
        if !is_async {
            return Err(CodecError::Backend(format!(
                "{} is not an asynchronous MFT",
                cand.name
            )));
        }
        // SAFETY: as above; unlocking is required before using an async MFT.
        unsafe { attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) }
            .map_err(|e| mf_err("ASYNC_UNLOCK", e))?;
        // SAFETY: as above; optional hint.
        let _ = unsafe { attrs.SetUINT32(&MF_LOW_LATENCY, 1) };
        let events: IMFMediaEventGenerator = transform
            .cast()
            .map_err(|e| mf_err("IMFMediaEventGenerator", e))?;
        let codec: ICodecAPI = transform.cast().map_err(|e| mf_err("ICodecAPI", e))?;

        let (mut ins, mut outs) = ([0u32], [0u32]);
        // SAFETY: both arrays have room for the one stream an encoder has. E_NOTIMPL means the
        // ids are the defaults (0, 0), which the arrays already hold.
        let _ = unsafe { transform.GetStreamIDs(&mut ins, &mut outs) };
        let (in_id, out_id) = (ins[0], outs[0]);

        // Rate control and latency first: some MFTs only honour them before the types are set.
        let mut rate_control = RateControl::preferred(cand.vendor_id);
        if set_codec(
            &codec,
            &CODECAPI_AVEncCommonRateControlMode,
            rate_control.mode().into(),
        )
        .is_err()
        {
            rate_control = RateControl::Cbr;
            set_codec(
                &codec,
                &CODECAPI_AVEncCommonRateControlMode,
                rate_control.mode().into(),
            )
            .map_err(|e| mf_err("rate control mode", e))?;
        }
        let mut max_bps = 0;
        apply_bitrate(&codec, rate_control, p.bitrate_bps, &mut max_bps)?;
        set_codec(&codec, &CODECAPI_AVLowLatencyMode, true.into())
            .map_err(|e| mf_err("low latency mode", e))?;
        // Optional knobs: an MFT that rejects them still produces a usable stream.
        let _ = set_codec(&codec, &CODECAPI_AVEncMPVDefaultBPictureCount, 0u32.into());
        let _ = set_codec(&codec, &CODECAPI_AVEncH264CABACEnable, false.into());
        // IDR only on request: the longest GOP the MFT accepts.
        let _ = set_codec(&codec, &CODECAPI_AVEncMPVGOPSize, 0u32.into());
        // Quality floor: no coarser quantiser than the OpenH264 path uses. Vendors differ in
        // which of these they honour; a refusal only means the MFT keeps its own limits.
        let max_qp = set_codec(
            &codec,
            &CODECAPI_AVEncVideoMaxQP,
            u32::from(p.qp.max).into(),
        );
        let min_qp = set_codec(
            &codec,
            &CODECAPI_AVEncVideoMinQP,
            u32::from(p.qp.min).into(),
        );
        tracing::debug!(
            vendor = ?cand.vendor_id,
            max_qp = max_qp.is_ok(),
            min_qp = min_qp.is_ok(),
            "hardware encoder QP bounds"
        );

        let mut profile = None;
        for (value, name) in PROFILES {
            let t = video_type(&MFVideoFormat_H264, p).map_err(|e| mf_err("output type", e))?;
            // SAFETY: plain values on a media type we own.
            let set = unsafe {
                t.SetUINT32(&MF_MT_AVG_BITRATE, p.bitrate_bps)
                    .and_then(|()| t.SetUINT32(&MF_MT_MPEG2_PROFILE, value as u32))
            };
            set.map_err(|e| mf_err("output type", e))?;
            // SAFETY: `t` is a complete H.264 media type; the MFT copies what it needs.
            if unsafe { transform.SetOutputType(out_id, &t, 0) }.is_ok() {
                profile = Some(name);
                break;
            }
        }
        let profile = profile.ok_or_else(|| {
            CodecError::Backend(format!(
                "{} accepts no baseline/main output type",
                cand.name
            ))
        })?;
        let input = video_type(&MFVideoFormat_NV12, p).map_err(|e| mf_err("input type", e))?;
        // SAFETY: `input` is a complete NV12 media type of the configured size.
        unsafe { transform.SetInputType(in_id, &input, 0) }
            .map_err(|e| mf_err("SetInputType(NV12)", e))?;

        let mut mft = Self {
            activate: cand.activate.clone(),
            transform,
            events,
            codec,
            in_id,
            out_id,
            provides_samples: true,
            out_size: 0,
            width: p.width,
            height: p.height,
            frame_hns: 10_000_000 / i64::from(p.fps.max(1)),
            need_input: 0,
            ready: VecDeque::new(),
            profile,
            rate_control,
            max_bps,
        };
        mft.read_output_info()?;
        for msg in [
            MFT_MESSAGE_COMMAND_FLUSH,
            MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
            MFT_MESSAGE_NOTIFY_START_OF_STREAM,
        ] {
            // SAFETY: streaming notifications with no parameter.
            unsafe { mft.transform.ProcessMessage(msg, 0) }
                .map_err(|e| mf_err("ProcessMessage(start)", e))?;
        }
        Ok(mft)
    }

    fn read_output_info(&mut self) -> Result<(), CodecError> {
        // SAFETY: plain query on the live MFT.
        let info = unsafe { self.transform.GetOutputStreamInfo(self.out_id) }
            .map_err(|e| mf_err("GetOutputStreamInfo", e))?;
        let provides =
            (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
        self.provides_samples = info.dwFlags & provides != 0;
        self.out_size = info.cbSize;
        Ok(())
    }

    /// Changes the target bitrate mid-stream.
    pub(crate) fn set_bitrate(&mut self, bps: u32) -> Result<(), CodecError> {
        apply_bitrate(&self.codec, self.rate_control, bps, &mut self.max_bps)
    }

    /// Encodes one picture. Returns the coded access unit (Annex-B, sample time in 100 ns) or
    /// `None` when the MFT holds the frame back. Fails when `deadline` passes first.
    pub(crate) fn encode(
        &mut self,
        yuv: &I420Buffer,
        timestamp_us: u64,
        keyframe: bool,
        deadline: Instant,
    ) -> Result<Option<(Vec<u8>, i64)>, CodecError> {
        self.pump_until(deadline, |m| m.need_input > 0)?;
        if keyframe {
            set_codec(&self.codec, &CODECAPI_AVEncVideoForceKeyFrame, 1u32.into())
                .map_err(|e| mf_err("force keyframe", e))?;
        }
        let sample = self.input_sample(yuv, timestamp_us)?;
        // SAFETY: `sample` is a complete NV12 sample; the MFT takes its own reference.
        unsafe { self.transform.ProcessInput(self.in_id, &sample, 0) }
            .map_err(|e| mf_err("ProcessInput", e))?;
        self.need_input = self.need_input.saturating_sub(1);
        self.pump_until(deadline, |m| !m.ready.is_empty())?;
        Ok(self.ready.pop_front())
    }

    fn input_sample(&self, yuv: &I420Buffer, timestamp_us: u64) -> Result<IMFSample, CodecError> {
        if yuv.width != self.width as usize || yuv.height != self.height as usize {
            return Err(CodecError::Backend(format!(
                "picture {}x{} does not match the {}x{} encoder",
                yuv.width, yuv.height, self.width, self.height
            )));
        }
        let len = nv12_len(yuv);
        let len32 = u32::try_from(len)
            .map_err(|_| CodecError::Backend("picture too large for one buffer".into()))?;
        // SAFETY: allocates a system-memory buffer of `len32` bytes.
        let buf = unsafe { MFCreateMemoryBuffer(len32) }.map_err(|e| mf_err("buffer", e))?;
        let mut ptr = std::ptr::null_mut();
        let mut max = 0u32;
        // SAFETY: Lock gives exclusive access to the buffer's `max` bytes until Unlock.
        unsafe { buf.Lock(&mut ptr, Some(&mut max), None) }.map_err(|e| mf_err("Lock", e))?;
        let written = if ptr.is_null() || (max as usize) < len {
            false
        } else {
            // SAFETY: `ptr` is valid for `max >= len` writable bytes while locked, and nothing
            // else aliases the freshly created buffer.
            let dst = unsafe { std::slice::from_raw_parts_mut(ptr, len) };
            i420_to_nv12(yuv, dst)
        };
        // SAFETY: balances the Lock above; `dst` is no longer used.
        unsafe { buf.Unlock() }.map_err(|e| mf_err("Unlock", e))?;
        if !written {
            return Err(CodecError::Backend("could not fill the NV12 buffer".into()));
        }
        let hns = i64::try_from(timestamp_us)
            .unwrap_or(i64::MAX / 10)
            .saturating_mul(10);
        let build = || -> windows::core::Result<IMFSample> {
            // SAFETY: plain calls on objects we own; AddBuffer takes its own reference.
            unsafe {
                buf.SetCurrentLength(len32)?;
                let sample = MFCreateSample()?;
                sample.AddBuffer(&buf)?;
                sample.SetSampleTime(hns)?;
                sample.SetSampleDuration(self.frame_hns)?;
                Ok(sample)
            }
        };
        build().map_err(|e| mf_err("input sample", e))
    }

    /// Handles queued events until `done` holds; fails at `deadline`.
    fn pump_until(
        &mut self,
        deadline: Instant,
        done: impl Fn(&Self) -> bool,
    ) -> Result<(), CodecError> {
        let mut idle = 0u32;
        loop {
            if done(self) {
                return Ok(());
            }
            // SAFETY: non-blocking event query on the live MFT.
            match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(ev) => {
                    idle = 0;
                    self.handle_event(&ev)?;
                }
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => {
                    if Instant::now() >= deadline {
                        return Err(CodecError::Backend(
                            "hardware encoder did not respond in time".into(),
                        ));
                    }
                    // Spin briefly (a hardware encode takes a few ms), then back off.
                    if idle < 64 {
                        std::thread::yield_now();
                    } else {
                        std::thread::sleep(Duration::from_micros(200));
                    }
                    idle = idle.saturating_add(1);
                }
                Err(e) => return Err(mf_err("GetEvent", e)),
            }
        }
    }

    fn handle_event(&mut self, ev: &IMFMediaEvent) -> Result<(), CodecError> {
        // SAFETY: plain queries on a live event object.
        let (kind, status) = unsafe { (ev.GetType(), ev.GetStatus()) };
        let kind = kind.map_err(|e| mf_err("event type", e))?;
        let status = status.map_err(|e| mf_err("event status", e))?;
        if status.is_err() || kind == MEError.0 as u32 {
            return Err(CodecError::Backend(format!(
                "hardware encoder reported an error ({status:?})"
            )));
        }
        if kind == METransformNeedInput.0 as u32 {
            self.need_input = self.need_input.saturating_add(1);
        } else if kind == METransformHaveOutput.0 as u32 {
            self.process_output()?;
        }
        Ok(())
    }

    fn process_output(&mut self) -> Result<(), CodecError> {
        let own = if self.provides_samples {
            None
        } else {
            Some(self.output_sample()?)
        };
        let mut db = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: self.out_id,
            pSample: ManuallyDrop::new(own),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        }];
        let mut status = 0u32;
        // SAFETY: `db` describes one output stream; the MFT fills in (or uses) the sample and
        // may attach an event collection, both of which we take back below.
        let r = unsafe { self.transform.ProcessOutput(0, &mut db, &mut status) };
        // SAFETY: each ManuallyDrop field is taken exactly once, so the references the call
        // returned are released exactly once.
        let (sample, events) = unsafe {
            (
                ManuallyDrop::take(&mut db[0].pSample),
                ManuallyDrop::take(&mut db[0].pEvents),
            )
        };
        drop(events);
        match r {
            Ok(()) => {}
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                return self.renegotiate_output();
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
            Err(e) => return Err(mf_err("ProcessOutput", e)),
        }
        let Some(sample) = sample else {
            return Ok(());
        };
        let bytes = sample_bytes(&sample)?;
        // SAFETY: plain query; a missing time falls back to 0.
        let time = unsafe { sample.GetSampleTime() }.unwrap_or(0);
        if !bytes.is_empty() {
            self.ready.push_back((bytes, time));
        }
        Ok(())
    }

    fn output_sample(&self) -> Result<IMFSample, CodecError> {
        let size = self
            .out_size
            .max(self.width.saturating_mul(self.height).saturating_mul(3) / 2)
            .max(64 * 1024);
        let build = || -> windows::core::Result<IMFSample> {
            // SAFETY: allocates an empty sample and buffer that we own.
            unsafe {
                let buf = MFCreateMemoryBuffer(size)?;
                let sample = MFCreateSample()?;
                sample.AddBuffer(&buf)?;
                Ok(sample)
            }
        };
        build().map_err(|e| mf_err("output sample", e))
    }

    fn renegotiate_output(&mut self) -> Result<(), CodecError> {
        // SAFETY: plain query for the MFT's own preferred output type.
        let t = unsafe { self.transform.GetOutputAvailableType(self.out_id, 0) }
            .map_err(|e| mf_err("output renegotiation", e))?;
        // SAFETY: sets that type back, as the stream-change protocol requires.
        unsafe { self.transform.SetOutputType(self.out_id, &t, 0) }
            .map_err(|e| mf_err("output renegotiation", e))?;
        self.read_output_info()
    }
}

fn sample_bytes(sample: &IMFSample) -> Result<Vec<u8>, CodecError> {
    // SAFETY: returns a buffer holding the whole sample.
    let buf = unsafe { sample.ConvertToContiguousBuffer() }
        .map_err(|e| mf_err("ConvertToContiguousBuffer", e))?;
    let mut ptr = std::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: Lock gives read access to `len` valid bytes until Unlock.
    unsafe { buf.Lock(&mut ptr, None, Some(&mut len)) }.map_err(|e| mf_err("Lock", e))?;
    let bytes = if ptr.is_null() {
        Vec::new()
    } else {
        // SAFETY: `ptr` is valid for `len` bytes while locked; we copy before unlocking.
        unsafe { std::slice::from_raw_parts(ptr, len as usize) }.to_vec()
    };
    // SAFETY: balances the Lock above.
    unsafe { buf.Unlock() }.map_err(|e| mf_err("Unlock", e))?;
    Ok(bytes)
}

impl Drop for Mft {
    fn drop(&mut self) {
        for msg in [
            MFT_MESSAGE_NOTIFY_END_OF_STREAM,
            MFT_MESSAGE_NOTIFY_END_STREAMING,
            MFT_MESSAGE_COMMAND_FLUSH,
        ] {
            // SAFETY: streaming notifications with no parameter on the live MFT.
            let _ = unsafe { self.transform.ProcessMessage(msg, 0) };
        }
        // SAFETY: shuts down the MFT this activation object created; it is not used afterwards.
        let _ = unsafe { self.activate.ShutdownObject() };
    }
}

#[cfg(test)]
mod tests {
    use super::parse_vendor;

    #[test]
    fn vendor_attribute_parses() {
        assert_eq!(parse_vendor("VEN_10DE"), Some(0x10DE));
        assert_eq!(parse_vendor("VEN_8086&DEV_4688"), Some(0x8086));
        assert_eq!(parse_vendor("VEN_"), None);
        assert_eq!(parse_vendor("8086"), None);
        assert_eq!(parse_vendor("VEN_ZZZZ"), None);
    }
}
