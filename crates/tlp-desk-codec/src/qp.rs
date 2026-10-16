//! Reads the quantiser (QP) an H.264 encoder used from its bitstream.
//!
//! The viewer shows "Q~24" next to the resolution, and the benchmark reports it, without any
//! encoder API: [`QpProbe`] parses just enough of the SPS, the PPS and the first slice header
//! of each access unit to compute `26 + pic_init_qp_minus26 + slice_qp_delta`. Anything it
//! does not understand (tools it cannot skip, field coding, ...) yields `None`, never an
//! error: the number is informational.
//!
//! Every byte comes from a peer, so reads are bounds-checked (a short or hostile NAL ends
//! the parse) and nothing here allocates from a value in the stream.

/// Sequence parameters that the slice header layout depends on.
#[derive(Debug, Clone, Copy)]
struct Sps {
    log2_max_frame_num: u32,
    /// `Some(bits)` for POC type 0, `None` for type 2 (no POC syntax in the slice header).
    poc_lsb_bits: Option<u32>,
}

/// Picture parameters that the slice header layout depends on.
#[derive(Debug, Clone, Copy)]
struct Pps {
    sps_id: usize,
    cabac: bool,
    bottom_field_pic_order: bool,
    weighted_pred: bool,
    redundant_pic_cnt: bool,
    init_qp: i32,
}

/// Stateful parser: feed it every access unit in order (parameter sets arrive with keyframes).
#[derive(Debug, Default)]
pub struct QpProbe {
    sps: [Option<Sps>; 32],
    pps: Vec<Option<Pps>>,
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn bit(&mut self) -> Option<u32> {
        let byte = self.data.get(self.pos / 8)?;
        let b = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Some(u32::from(b))
    }

    fn u(&mut self, n: u32) -> Option<u32> {
        if n > 32 {
            return None;
        }
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Some(v)
    }

    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0u32;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        let rest = self.u(zeros)?;
        let v = (1u64 << zeros) - 1 + u64::from(rest);
        u32::try_from(v).ok()
    }

    /// `ref_pic_list_modification()` for one list: a flag, then commands until idc 3.
    fn skip_list_modification(&mut self) -> Option<()> {
        if self.bit()? == 0 {
            return Some(());
        }
        for _ in 0..66 {
            match self.ue()? {
                0..=2 => {
                    self.ue()?;
                }
                3 => return Some(()),
                _ => return None,
            }
        }
        None
    }

    fn se(&mut self) -> Option<i32> {
        let k = i64::from(self.ue()?);
        let v = if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) };
        i32::try_from(v).ok()
    }
}

/// NAL payload without the emulation-prevention bytes (00 00 03 -> 00 00).
fn unescape(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
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

/// Splits Annex-B data into NAL units (start codes removed).
fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data.get(i..i + 3) == Some(&[0, 0, 1]) {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (n, &s) in starts.iter().enumerate() {
        let mut end = starts
            .get(n + 1)
            .map_or(data.len(), |&next| next.saturating_sub(3));
        // Trailing zero bytes belong to the next start code (4-byte form).
        while end > s && data.get(end - 1) == Some(&0) {
            end -= 1;
        }
        if let Some(nal) = data.get(s..end) {
            out.push(nal);
        }
    }
    out
}

impl QpProbe {
    /// A probe that has seen nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one access unit (Annex-B) and returns the QP of its first slice, if it could be
    /// worked out.
    pub fn feed(&mut self, access_unit: &[u8]) -> Option<u8> {
        let mut qp = None;
        for nal in nal_units(access_unit) {
            let Some((&head, body)) = nal.split_first() else {
                continue;
            };
            match head & 0x1f {
                7 => self.parse_sps(&unescape(body)),
                8 => self.parse_pps(&unescape(body)),
                t @ (1 | 5) if qp.is_none() => {
                    qp = self.slice_qp(&unescape(body), t == 5, head >> 5 != 0);
                }
                _ => {}
            }
        }
        qp
    }

    fn parse_sps(&mut self, rbsp: &[u8]) {
        let parsed = (|| {
            let mut r = Bits::new(rbsp);
            let profile = r.u(8)?;
            r.u(16)?; // constraint flags + level
            let id = usize::try_from(r.ue()?).ok().filter(|&i| i < 32)?;
            if matches!(
                profile,
                100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
            ) {
                if r.ue()? == 3 {
                    r.bit()?;
                }
                r.ue()?;
                r.ue()?;
                r.bit()?;
                if r.bit()? == 1 {
                    return None; // scaling lists: not worth parsing
                }
            }
            let log2_max_frame_num = r.ue()?.checked_add(4).filter(|&b| b <= 16)?;
            let poc_lsb_bits = match r.ue()? {
                0 => Some(r.ue()?.checked_add(4).filter(|&b| b <= 16)?),
                2 => None,
                _ => return None,
            };
            r.ue()?; // max_num_ref_frames
            r.bit()?; // gaps_in_frame_num_value_allowed
            r.ue()?;
            r.ue()?;
            if r.bit()? == 0 {
                return None; // interlaced coding adds field_pic_flag to slice headers
            }
            Some((
                id,
                Sps {
                    log2_max_frame_num,
                    poc_lsb_bits,
                },
            ))
        })();
        if let Some((id, sps)) = parsed {
            if let Some(slot) = self.sps.get_mut(id) {
                *slot = Some(sps);
            }
        }
    }

    fn parse_pps(&mut self, rbsp: &[u8]) {
        let parsed = (|| {
            let mut r = Bits::new(rbsp);
            let id = usize::try_from(r.ue()?).ok().filter(|&i| i < 256)?;
            let sps_id = usize::try_from(r.ue()?).ok().filter(|&i| i < 32)?;
            let cabac = r.bit()? == 1;
            let bottom_field_pic_order = r.bit()? == 1;
            if r.ue()? != 0 {
                return None; // slice groups
            }
            r.ue()?;
            r.ue()?;
            let weighted_pred = r.bit()? == 1;
            r.u(2)?;
            let init_qp = 26 + r.se()?;
            r.se()?; // pic_init_qs
            r.se()?; // chroma_qp_index_offset
            r.bit()?; // deblocking_filter_control_present
            r.bit()?; // constrained_intra_pred
            let redundant_pic_cnt = r.bit()? == 1;
            Some((
                id,
                Pps {
                    sps_id,
                    cabac,
                    bottom_field_pic_order,
                    weighted_pred,
                    redundant_pic_cnt,
                    init_qp,
                },
            ))
        })();
        if let Some((id, pps)) = parsed {
            if self.pps.len() <= id {
                self.pps.resize(id + 1, None);
            }
            if let Some(slot) = self.pps.get_mut(id) {
                *slot = Some(pps);
            }
        }
    }

    fn slice_qp(&self, rbsp: &[u8], idr: bool, reference: bool) -> Option<u8> {
        let mut r = Bits::new(rbsp);
        r.ue()?; // first_mb_in_slice
        let slice_type = r.ue()? % 5; // 0 P, 1 B, 2 I, 3 SP, 4 SI
        let pps_id = usize::try_from(r.ue()?).ok()?;
        let pps = self.pps.get(pps_id).copied().flatten()?;
        let sps = self.sps.get(pps.sps_id).copied().flatten()?;
        r.u(sps.log2_max_frame_num)?;
        if idr {
            r.ue()?;
        }
        if let Some(bits) = sps.poc_lsb_bits {
            r.u(bits)?;
            if pps.bottom_field_pic_order {
                r.se()?;
            }
        }
        if pps.redundant_pic_cnt {
            r.ue()?;
        }
        let inter = matches!(slice_type, 0 | 1 | 3);
        if slice_type == 1 {
            r.bit()?; // direct_spatial_mv_pred
        }
        if inter {
            if r.bit()? == 1 {
                r.ue()?;
                if slice_type == 1 {
                    r.ue()?;
                }
            }
            r.skip_list_modification()?;
            if slice_type == 1 {
                r.skip_list_modification()?;
            }
            if pps.weighted_pred {
                return None;
            }
        }
        if reference {
            if idr {
                r.u(2)?;
            } else if r.bit()? == 1 {
                return None; // adaptive ref pic marking
            }
        }
        if pps.cabac && inter {
            r.ue()?;
        }
        let qp = pps.init_qp.checked_add(r.se()?)?;
        u8::try_from(qp).ok().filter(|&q| q <= 51)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exp_golomb_codes() {
        // 1 | 010 | 011 | 00100 -> ue 0, 1, 2, 3
        let mut r = Bits::new(&[0b1010_0110, 0b0100_0000]);
        assert_eq!(
            [r.ue(), r.ue(), r.ue(), r.ue()],
            [Some(0), Some(1), Some(2), Some(3)]
        );
        // se: ue 1 -> +1, ue 2 -> -1
        let mut r = Bits::new(&[0b0100_1100]);
        assert_eq!([r.se(), r.se()], [Some(1), Some(-1)]);
        assert_eq!(Bits::new(&[0, 0, 0, 0, 0]).ue(), None);
    }

    #[test]
    fn hostile_input_is_none() {
        let mut p = QpProbe::new();
        assert_eq!(p.feed(&[]), None);
        assert_eq!(p.feed(&[0, 0, 1]), None);
        assert_eq!(p.feed(&[0, 0, 1, 0x65]), None);
        assert_eq!(p.feed(&[0, 0, 0, 1, 0x67, 0xff, 0xff]), None);
        assert_eq!(p.feed(&[0xff; 64]), None);
    }
}
