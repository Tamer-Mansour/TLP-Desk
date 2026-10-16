//! Annex-B helpers for hardware encoder output: NAL unit scanning and SPS/PPS repetition.
//!
//! Hardware MFTs emit SPS/PPS with the first IDR only (some also on later ones). A viewer that
//! joins at a requested keyframe needs them, so [`ParamSets`] remembers the latest ones and
//! puts them in front of any IDR access unit that lacks them.

/// NAL unit type of an IDR slice.
pub(crate) const NAL_IDR: u8 = 5;
/// NAL unit type of a sequence parameter set.
pub(crate) const NAL_SPS: u8 = 7;
/// NAL unit type of a picture parameter set.
pub(crate) const NAL_PPS: u8 = 8;

/// One NAL unit inside an Annex-B buffer: its type and its byte range *including* the start
/// code, so ranges can be copied verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Nal {
    pub kind: u8,
    pub start: usize,
    pub end: usize,
}

/// Position of the next start code (`00 00 01`) at or after `from`, and its length (3, or 4
/// when preceded by an extra zero byte).
fn next_start_code(data: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while let Some(w) = data.get(i..i.checked_add(3)?) {
        if w == [0, 0, 1] {
            if i > from && data.get(i - 1) == Some(&0) {
                return Some((i - 1, 4));
            }
            return Some((i, 3));
        }
        i += 1;
    }
    None
}

/// Splits an Annex-B buffer into NAL units. Bytes before the first start code are ignored;
/// a start code with no header byte after it ends the scan.
pub(crate) fn split_nals(data: &[u8]) -> Vec<Nal> {
    let mut out = Vec::new();
    let Some((mut start, mut sc_len)) = next_start_code(data, 0) else {
        return out;
    };
    loop {
        let header = start + sc_len;
        let Some(&h) = data.get(header) else {
            return out;
        };
        let next = next_start_code(data, header);
        let end = next.map_or(data.len(), |(s, _)| s);
        out.push(Nal {
            kind: h & 0x1F,
            start,
            end,
        });
        match next {
            Some((s, l)) => {
                start = s;
                sc_len = l;
            }
            None => return out,
        }
    }
}

/// The most recent SPS and PPS seen in the stream (with start codes).
#[derive(Debug, Default, Clone)]
pub(crate) struct ParamSets {
    sps: Vec<u8>,
    pps: Vec<u8>,
}

impl ParamSets {
    /// Records the SPS/PPS found in `au` and, if `au` holds an IDR without them, returns it
    /// with the remembered ones in front. Returns whether `au` is an IDR access unit.
    pub(crate) fn process(&mut self, au: &mut Vec<u8>) -> bool {
        let nals = split_nals(au);
        let mut idr = false;
        let (mut saw_sps, mut saw_pps) = (false, false);
        for n in &nals {
            let bytes = au.get(n.start..n.end).unwrap_or_default();
            match n.kind {
                NAL_SPS => {
                    saw_sps = true;
                    self.sps = bytes.to_vec();
                }
                NAL_PPS => {
                    saw_pps = true;
                    self.pps = bytes.to_vec();
                }
                NAL_IDR => idr = true,
                _ => {}
            }
        }
        if idr && !(saw_sps && saw_pps) && !self.sps.is_empty() && !self.pps.is_empty() {
            let mut fixed = Vec::with_capacity(self.sps.len() + self.pps.len() + au.len());
            fixed.extend_from_slice(&self.sps);
            fixed.extend_from_slice(&self.pps);
            fixed.extend_from_slice(au);
            *au = fixed;
        }
        idr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(data: &[u8]) -> Vec<u8> {
        split_nals(data).iter().map(|n| n.kind).collect()
    }

    #[test]
    fn splits_three_and_four_byte_start_codes() {
        let data = [
            0, 0, 0, 1, 0x67, 1, 2, // SPS, 4-byte start code
            0, 0, 1, 0x68, 3, // PPS, 3-byte start code
            0, 0, 0, 1, 0x65, 9, 9, 9, // IDR
        ];
        let nals = split_nals(&data);
        assert_eq!(kinds(&data), vec![7, 8, 5]);
        assert_eq!((nals[0].start, nals[0].end), (0, 7));
        assert_eq!((nals[1].start, nals[1].end), (7, 12));
        assert_eq!((nals[2].start, nals[2].end), (12, data.len()));
    }

    #[test]
    fn garbage_and_truncated_input_never_panics() {
        assert!(split_nals(&[]).is_empty());
        assert!(split_nals(&[1, 2, 3]).is_empty());
        assert!(split_nals(&[0, 0, 1]).is_empty());
        assert_eq!(kinds(&[0, 0, 1, 0x41, 0, 0, 1]), vec![1]);
        assert_eq!(kinds(&[9, 9, 0, 0, 1, 0x09, 0xF0]), vec![9]);
        assert!(split_nals(&[0, 0, 0, 0, 0]).is_empty());
    }

    #[test]
    fn idr_without_parameter_sets_gets_them_prepended() {
        let mut ps = ParamSets::default();
        let mut first = vec![
            0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2, 0, 0, 0, 1, 0x65, 3,
        ];
        let original = first.clone();
        assert!(ps.process(&mut first));
        assert_eq!(first, original, "complete IDR is left alone");

        let mut p = vec![0, 0, 0, 1, 0x41, 4];
        assert!(!ps.process(&mut p));
        assert_eq!(p, vec![0, 0, 0, 1, 0x41, 4]);

        let mut later_idr = vec![0, 0, 0, 1, 0x09, 0xF0, 0, 0, 0, 1, 0x65, 5];
        assert!(ps.process(&mut later_idr));
        assert_eq!(kinds(&later_idr), vec![7, 8, 9, 5]);
    }

    #[test]
    fn idr_before_any_parameter_sets_is_unchanged() {
        let mut ps = ParamSets::default();
        let mut idr = vec![0, 0, 1, 0x65, 1];
        assert!(ps.process(&mut idr));
        assert_eq!(idr, vec![0, 0, 1, 0x65, 1]);
    }
}
