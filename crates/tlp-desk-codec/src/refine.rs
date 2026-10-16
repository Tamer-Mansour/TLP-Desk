//! Static-refinement support: the quantiser and fallback used for "refresh" pictures.
//!
//! When the screen stops changing, the host re-encodes the last frame once at a very low
//! quantiser (see [`VideoEncoder::encode_refine`]) so text that was smeared by rate control
//! during motion becomes crisp. The trait method lives in the crate root; this module holds the
//! constants and the generic fallback used by backends without direct quantiser control.

use tlp_desk_capture::BgraFrame;

use crate::{CodecError, EncodedFrame, VideoEncoder};

/// H.264 quantiser (0-51) of refresh pictures. 14 is visually near-lossless for text and UI
/// while a 1080p refresh keyframe stays in the low hundreds of kilobytes.
pub const REFINE_QP: i32 = 14;

/// How much the bitrate is multiplied for a refresh picture by [`boosted_refine`].
const BOOST_FACTOR: u32 = 6;

/// Generic refresh for backends without quantiser control: force a keyframe and let rate
/// control spend [`BOOST_FACTOR`] times the bitrate on it, then restore the bitrate.
pub(crate) fn boosted_refine<E: VideoEncoder + ?Sized>(
    enc: &mut E,
    frame: &BgraFrame,
) -> Result<Option<EncodedFrame>, CodecError> {
    let base = enc.config().bitrate_kbps.max(1);
    let boosted = base.saturating_mul(BOOST_FACTOR).min(200_000);
    let raised = enc.set_bitrate(boosted).is_ok();
    enc.request_keyframe();
    let out = enc.encode(frame);
    if raised {
        enc.set_bitrate(base)?;
    }
    out
}
