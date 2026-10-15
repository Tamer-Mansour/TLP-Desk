//! Just enough H.264 bitstream handling for the recorder: split Annex-B access units into NAL
//! units, convert between Annex-B and the length-prefixed form MP4 stores, and read the picture
//! size from a sequence parameter set (SPS).
//!
//! Everything here parses bytes that came from the network peer, so every read is bounds
//! checked and every loop is bounded. Nothing panics on malformed input.

use crate::ViewerError;

/// NAL unit type: coded slice of an IDR picture.
pub(crate) const NAL_IDR: u8 = 5;
/// NAL unit type: sequence parameter set.
pub(crate) const NAL_SPS: u8 = 7;
/// NAL unit type: picture parameter set.
pub(crate) const NAL_PPS: u8 = 8;
/// NAL unit type: access unit delimiter.
pub(crate) const NAL_AUD: u8 = 9;

/// Largest picture side we accept from an SPS. H.264 level 6.2 tops out at 8192 x 4320; this
/// leaves headroom while keeping later size arithmetic far from overflow.
const MAX_SIDE: u32 = 16_384;

fn bad(reason: &'static str) -> ViewerError {
    ViewerError::BadVideo(reason)
}

/// The NAL unit type (low five bits of the header byte).
pub(crate) fn nal_type(nal: &[u8]) -> Option<u8> {
    nal.first().map(|b| b & 0x1F)
}

/// Splits an Annex-B byte stream (`00 00 01` / `00 00 00 01` start codes) into NAL units.
/// Bytes before the first start code are ignored; trailing zero bytes (the leading zero of a
/// 4-byte start code, or `trailing_zero_8bits`) are trimmed from each unit; empty units are
/// skipped.
pub(crate) fn split_annex_b(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut i = 0usize;
    while i + 3 <= data.len() {
        if data.get(i..i + 3) == Some(&[0, 0, 1]) {
            if let Some(s) = start {
                push_trimmed(&mut out, data.get(s..i).unwrap_or_default());
            }
            i += 3;
            start = Some(i);
        } else {
            i += 1;
        }
    }
    if let Some(s) = start {
        push_trimmed(&mut out, data.get(s..).unwrap_or_default());
    }
    out
}

fn push_trimmed<'a>(out: &mut Vec<&'a [u8]>, nal: &'a [u8]) {
    let end = nal.iter().rposition(|b| *b != 0).map_or(0, |p| p + 1);
    if let Some(nal) = nal.get(..end).filter(|n| !n.is_empty()) {
        out.push(nal);
    }
}

/// What the recorder needs from one received access unit.
#[derive(Debug, Default)]
pub(crate) struct AccessUnit {
    /// Sequence parameter sets carried in-band (the encoder sends them with every IDR).
    pub sps: Vec<Vec<u8>>,
    /// Picture parameter sets carried in-band.
    pub pps: Vec<Vec<u8>>,
    /// The picture data in MP4 form: each remaining NAL unit prefixed by its 4-byte big-endian
    /// length. Parameter sets and access unit delimiters are left out (they live in the
    /// sample description).
    pub sample: Vec<u8>,
    /// Whether the access unit contains an IDR slice.
    pub idr: bool,
}

/// Converts one Annex-B access unit to its MP4 sample form and pulls out its parameter sets.
pub(crate) fn parse_access_unit(data: &[u8]) -> Result<AccessUnit, ViewerError> {
    let mut au = AccessUnit {
        sample: Vec::with_capacity(data.len().saturating_add(16)),
        ..AccessUnit::default()
    };
    for nal in split_annex_b(data) {
        match nal_type(nal) {
            Some(NAL_SPS) => au.sps.push(nal.to_vec()),
            Some(NAL_PPS) => au.pps.push(nal.to_vec()),
            Some(NAL_AUD) => {}
            Some(t) => {
                if t == NAL_IDR {
                    au.idr = true;
                }
                let len = u32::try_from(nal.len()).map_err(|_| bad("NAL unit too large"))?;
                au.sample.extend_from_slice(&len.to_be_bytes());
                au.sample.extend_from_slice(nal);
            }
            None => {}
        }
    }
    if au.sample.is_empty() {
        return Err(bad("access unit without picture data"));
    }
    Ok(au)
}

/// Converts an MP4 sample (4-byte length-prefixed NAL units) back to Annex-B, optionally
/// preceded by parameter sets (needed before a keyframe so a fresh decoder can start there).
pub(crate) fn sample_to_annex_b(
    sample: &[u8],
    params: &[&[u8]],
    out: &mut Vec<u8>,
) -> Result<(), ViewerError> {
    out.clear();
    for p in params {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(p);
    }
    let mut rest = sample;
    // Each iteration consumes at least 4 bytes, so the loop is bounded by the sample size.
    while !rest.is_empty() {
        let (len, tail) = rest
            .split_first_chunk::<4>()
            .ok_or(bad("truncated NAL length"))?;
        let len = usize::try_from(u32::from_be_bytes(*len)).map_err(|_| bad("NAL length"))?;
        let nal = tail
            .get(..len)
            .ok_or(bad("NAL unit runs past the sample"))?;
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
        rest = tail.get(len..).unwrap_or_default();
    }
    Ok(())
}

/// Fields of a sequence parameter set the container needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SpsInfo {
    pub profile_idc: u8,
    pub constraint_flags: u8,
    pub level_idc: u8,
    pub chroma_format_idc: u32,
    pub bit_depth_luma_minus8: u32,
    pub bit_depth_chroma_minus8: u32,
    /// Displayed picture size (after frame cropping).
    pub width: u32,
    pub height: u32,
}

/// Removes emulation prevention bytes (`00 00 03` -> `00 00`) to get the raw RBSP.
fn unescape(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0u32;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// MSB-first bit reader with Exp-Golomb support.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Result<u32, ViewerError> {
        let byte = self.data.get(self.pos / 8).ok_or(bad("SPS is truncated"))?;
        let bit = (byte >> (7 - (self.pos % 8))) & 1;
        self.pos += 1;
        Ok(u32::from(bit))
    }

    fn bits(&mut self, n: u32) -> Result<u32, ViewerError> {
        let mut v = 0u32;
        for _ in 0..n.min(32) {
            v = (v << 1) | self.bit()?;
        }
        Ok(v)
    }

    /// Unsigned Exp-Golomb (`ue(v)`), limited to 31 leading zeros (fits in `u32`).
    fn ue(&mut self) -> Result<u32, ViewerError> {
        let mut zeros = 0u32;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return Err(bad("SPS Exp-Golomb value too long"));
            }
        }
        if zeros == 0 {
            return Ok(0);
        }
        let rest = self.bits(zeros)?;
        let base = (1u64 << zeros) - 1;
        u32::try_from(base + u64::from(rest)).map_err(|_| bad("SPS value out of range"))
    }

    /// Signed Exp-Golomb (`se(v)`).
    fn se(&mut self) -> Result<i64, ViewerError> {
        let k = i64::from(self.ue()?);
        Ok(if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) })
    }
}

fn skip_scaling_list(r: &mut Bits<'_>, size: u32) -> Result<(), ViewerError> {
    let (mut last, mut next) = (8i64, 8i64);
    for _ in 0..size {
        if next != 0 {
            let delta = r.se()?;
            next = (last + delta + 256).rem_euclid(256);
        }
        if next != 0 {
            last = next;
        }
    }
    Ok(())
}

/// Parses an SPS NAL unit (header byte included). See ITU-T H.264 section 7.3.2.1.1.
pub(crate) fn parse_sps(nal: &[u8]) -> Result<SpsInfo, ViewerError> {
    if nal_type(nal) != Some(NAL_SPS) {
        return Err(bad("not an SPS"));
    }
    let rbsp = unescape(nal.get(1..).unwrap_or_default());
    let mut r = Bits {
        data: &rbsp,
        pos: 0,
    };
    let profile_idc = u8::try_from(r.bits(8)?).map_err(|_| bad("profile"))?;
    let constraint_flags = u8::try_from(r.bits(8)?).map_err(|_| bad("constraints"))?;
    let level_idc = u8::try_from(r.bits(8)?).map_err(|_| bad("level"))?;
    let _sps_id = r.ue()?;
    let mut chroma_format_idc = 1;
    let mut separate_colour_plane = false;
    let (mut bit_depth_luma_minus8, mut bit_depth_chroma_minus8) = (0, 0);
    if matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        chroma_format_idc = r.ue()?;
        if chroma_format_idc > 3 {
            return Err(bad("SPS chroma format"));
        }
        if chroma_format_idc == 3 {
            separate_colour_plane = r.bit()? == 1;
        }
        bit_depth_luma_minus8 = r.ue()?;
        bit_depth_chroma_minus8 = r.ue()?;
        let _qpprime_y_zero_transform_bypass = r.bit()?;
        if r.bit()? == 1 {
            let lists = if chroma_format_idc == 3 { 12 } else { 8 };
            for i in 0..lists {
                if r.bit()? == 1 {
                    skip_scaling_list(&mut r, if i < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    let _log2_max_frame_num_minus4 = r.ue()?;
    match r.ue()? {
        0 => {
            let _log2_max_poc_lsb_minus4 = r.ue()?;
        }
        1 => {
            let _delta_pic_order_always_zero = r.bit()?;
            let _offset_for_non_ref_pic = r.se()?;
            let _offset_for_top_to_bottom_field = r.se()?;
            let cycle = r.ue()?;
            if cycle > 255 {
                return Err(bad("SPS POC cycle too long"));
            }
            for _ in 0..cycle {
                let _offset_for_ref_frame = r.se()?;
            }
        }
        2 => {}
        _ => return Err(bad("SPS picture order count type")),
    }
    let _max_num_ref_frames = r.ue()?;
    let _gaps_in_frame_num_allowed = r.bit()?;
    let width_mbs = r.ue()?.checked_add(1).ok_or(bad("SPS width"))?;
    let height_map_units = r.ue()?.checked_add(1).ok_or(bad("SPS height"))?;
    let frame_mbs_only = r.bit()?;
    if frame_mbs_only == 0 {
        let _mb_adaptive_frame_field = r.bit()?;
    }
    let _direct_8x8_inference = r.bit()?;
    let (mut crop_l, mut crop_r, mut crop_t, mut crop_b) = (0u32, 0u32, 0u32, 0u32);
    if r.bit()? == 1 {
        crop_l = r.ue()?;
        crop_r = r.ue()?;
        crop_t = r.ue()?;
        crop_b = r.ue()?;
    }

    let field_factor = 2 - frame_mbs_only;
    let (crop_x, crop_y) = if separate_colour_plane || chroma_format_idc == 0 {
        (1, field_factor)
    } else {
        let sub_w = if chroma_format_idc == 3 { 1 } else { 2 };
        let sub_h = if chroma_format_idc == 1 { 2 } else { 1 };
        (sub_w, sub_h * field_factor)
    };
    let full_w = width_mbs.checked_mul(16).ok_or(bad("SPS width"))?;
    let full_h = height_map_units
        .checked_mul(16)
        .and_then(|h| h.checked_mul(field_factor))
        .ok_or(bad("SPS height"))?;
    let cut_w = crop_l
        .checked_add(crop_r)
        .and_then(|c| c.checked_mul(crop_x))
        .ok_or(bad("SPS crop"))?;
    let cut_h = crop_t
        .checked_add(crop_b)
        .and_then(|c| c.checked_mul(crop_y))
        .ok_or(bad("SPS crop"))?;
    let width = full_w.checked_sub(cut_w).ok_or(bad("SPS crop"))?;
    let height = full_h.checked_sub(cut_h).ok_or(bad("SPS crop"))?;
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(bad("SPS picture size out of range"));
    }
    Ok(SpsInfo {
        profile_idc,
        constraint_flags,
        level_idc,
        chroma_format_idc,
        bit_depth_luma_minus8,
        bit_depth_chroma_minus8,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_three_and_four_byte_start_codes() {
        let data = [
            0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 9, 9, 0,
        ];
        let nals = split_annex_b(&data);
        assert_eq!(nals, vec![&[0x67, 1, 2][..], &[0x68, 3], &[0x65, 9, 9]]);
        assert!(split_annex_b(&[1, 2, 3]).is_empty());
        assert!(split_annex_b(&[]).is_empty());
        assert!(split_annex_b(&[0, 0, 1]).is_empty());
    }

    #[test]
    fn access_unit_round_trips_through_mp4_form() {
        let data = [
            0, 0, 0, 1, 0x09, 0xF0, // AUD (dropped)
            0, 0, 0, 1, 0x67, 1, 2, // SPS
            0, 0, 0, 1, 0x68, 3, // PPS
            0, 0, 1, 0x65, 7, 7, 7, // IDR slice
        ];
        let au = parse_access_unit(&data).unwrap();
        assert!(au.idr);
        assert_eq!(au.sps, vec![vec![0x67, 1, 2]]);
        assert_eq!(au.pps, vec![vec![0x68, 3]]);
        assert_eq!(au.sample, vec![0, 0, 0, 4, 0x65, 7, 7, 7]);
        let mut out = Vec::new();
        sample_to_annex_b(&au.sample, &[&au.sps[0], &au.pps[0]], &mut out).unwrap();
        assert_eq!(
            out,
            vec![0, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 7, 7, 7]
        );
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        assert!(parse_access_unit(&[]).is_err());
        assert!(parse_access_unit(&[0, 0, 1, 0x67, 1]).is_err(), "SPS only");
        let mut out = Vec::new();
        assert!(sample_to_annex_b(&[0, 0, 0, 9, 1], &[], &mut out).is_err());
        assert!(sample_to_annex_b(&[0, 0], &[], &mut out).is_err());
        assert!(parse_sps(&[0x67]).is_err());
        assert!(parse_sps(&[0x68, 1, 2]).is_err());
        // All-zero payload: an Exp-Golomb value that never terminates.
        assert!(parse_sps(&[0x67, 66, 0, 30, 0, 0, 0, 0, 0, 0]).is_err());
        for seed in 0u32..2000 {
            // Cheap deterministic noise.
            let bytes: Vec<u8> = (0..24u32)
                .map(|i| (seed.wrapping_mul(2_654_435_761).wrapping_add(i * 97) >> 7) as u8)
                .collect();
            let _ = parse_sps(&[&[0x67][..], &bytes].concat());
            let _ = parse_access_unit(&bytes);
            let _ = sample_to_annex_b(&bytes, &[], &mut out);
        }
    }

    #[test]
    fn unescape_drops_emulation_prevention() {
        assert_eq!(unescape(&[0, 0, 3, 1, 0, 0, 3]), vec![0, 0, 1, 0, 0]);
        assert_eq!(unescape(&[1, 0, 3, 0]), vec![1, 0, 3, 0]);
    }
}
