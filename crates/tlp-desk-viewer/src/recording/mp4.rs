//! Fragmented MP4 (ISO/IEC 14496-12 + 14496-15) box writer for one H.264 video track.
//!
//! File layout written by the recorder:
//!
//! ```text
//! ftyp                      brands isom, iso6, avc1, mp41
//! moov                      mvhd, trak(tkhd, mdia(mdhd, hdlr, minf(vmhd, dinf, stbl(stsd(avc1(avcC)),
//!                           empty stts/stsc/stsz/stco)))), mvex(mehd, trex) -- no samples
//! moof + mdat               one fragment: mfhd, traf(tfhd, tfdt, trun) + the sample bytes
//! moof + mdat               ... appended about once per second (or at each keyframe)
//! mfra                      tfra (keyframe index) + mfro; written when the recording is closed
//! ```
//!
//! Because every fragment is self-contained and appended in a single write, a crash loses at
//! most the fragment being written: everything before it plays. `mvhd`/`mehd` durations are
//! zero while recording and patched in place when the file is closed (or recovered).
//!
//! Audio: a second track (Opus `Opus`/`dOps` sample entry) would be one more `trak` + `trex`
//! and a second `traf` per fragment; the box helpers here are track-agnostic for that reason.

use super::nal::SpsInfo;

/// Timescale of the movie header (`mvhd`, `mehd`): milliseconds.
pub(crate) const MOVIE_TIMESCALE: u32 = 1_000;
/// Timescale of the video track: the usual 90 kHz video clock.
pub(crate) const VIDEO_TIMESCALE: u32 = 90_000;
/// The only track in the file.
pub(crate) const TRACK_ID: u32 = 1;

/// `sample_flags` for a sync sample (keyframe): depends on no other picture.
pub(crate) const FLAGS_SYNC: u32 = 0x0200_0000;
/// `sample_flags` for a non-sync sample: depends on others, `sample_is_non_sync_sample`.
pub(crate) const FLAGS_NON_SYNC: u32 = 0x0101_0000;

/// Unity transformation matrix used by `mvhd` and `tkhd`.
const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

/// Builds nested boxes into one buffer; sizes are filled in when a box ends.
#[derive(Default)]
pub(crate) struct BoxWriter {
    pub buf: Vec<u8>,
    open: Vec<usize>,
}

impl BoxWriter {
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(v);
        self
    }
    pub fn zeros(&mut self, n: usize) -> &mut Self {
        self.buf.resize(self.buf.len() + n, 0);
        self
    }

    /// Opens a plain box.
    pub fn begin(&mut self, kind: &[u8; 4]) -> &mut Self {
        self.open.push(self.buf.len());
        self.u32(0).bytes(kind)
    }

    /// Opens a full box (version + 24-bit flags).
    pub fn begin_full(&mut self, kind: &[u8; 4], version: u8, flags: u32) -> &mut Self {
        self.begin(kind)
            .u32((u32::from(version) << 24) | (flags & 0x00FF_FFFF))
    }

    /// Closes the innermost open box and writes its size. Boxes we build are far below 4 GiB
    /// (fragments are capped by the recorder), so a 32-bit size always fits.
    pub fn end(&mut self) -> &mut Self {
        if let Some(start) = self.open.pop() {
            let size = u32::try_from(self.buf.len() - start).unwrap_or(u32::MAX);
            if let Some(slot) = self.buf.get_mut(start..start + 4) {
                slot.copy_from_slice(&size.to_be_bytes());
            }
        }
        self
    }

    /// Current write position (absolute offset within `buf`).
    pub fn pos(&self) -> usize {
        self.buf.len()
    }
}

/// The video track as described in the init segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrackConfig {
    pub info: SpsInfo,
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
}

/// An init segment and where its duration fields are, for patching when the file is closed.
pub(crate) struct InitSegment {
    pub bytes: Vec<u8>,
    /// Offset of `mvhd.duration` (32-bit, movie timescale).
    pub mvhd_duration_at: u64,
    /// Offset of `mehd.fragment_duration` (64-bit, movie timescale).
    pub mehd_duration_at: u64,
}

/// Writes `ftyp` + `moov` for one H.264 track.
pub(crate) fn init_segment(track: &TrackConfig) -> InitSegment {
    let mut w = BoxWriter::default();
    w.begin(b"ftyp")
        .bytes(b"isom")
        .u32(0x200)
        .bytes(b"isom")
        .bytes(b"iso6")
        .bytes(b"avc1")
        .bytes(b"mp41")
        .end();

    w.begin(b"moov");
    w.begin_full(b"mvhd", 0, 0)
        .u32(0)
        .u32(0)
        .u32(MOVIE_TIMESCALE);
    let mvhd_duration_at = w.pos() as u64;
    w.u32(0) // duration, patched on close
        .u32(0x0001_0000) // rate 1.0
        .u16(0x0100) // volume 1.0
        .zeros(10);
    for m in MATRIX {
        w.u32(m);
    }
    w.zeros(24).u32(TRACK_ID + 1).end();

    write_trak(&mut w, track);

    w.begin(b"mvex");
    w.begin_full(b"mehd", 1, 0);
    let mehd_duration_at = w.pos() as u64;
    w.u64(0).end(); // fragment_duration, patched on close
    w.begin_full(b"trex", 0, 0)
        .u32(TRACK_ID)
        .u32(1) // default_sample_description_index
        .u32(0)
        .u32(0)
        .u32(0)
        .end();
    w.end(); // mvex
    w.end(); // moov
    InitSegment {
        bytes: w.buf,
        mvhd_duration_at,
        mehd_duration_at,
    }
}

fn write_trak(w: &mut BoxWriter, track: &TrackConfig) {
    let (width, height) = (track.info.width, track.info.height);
    w.begin(b"trak");
    // flags: track_enabled | track_in_movie
    w.begin_full(b"tkhd", 0, 0x3)
        .u32(0)
        .u32(0)
        .u32(TRACK_ID)
        .u32(0)
        .u32(0) // duration (fragments carry the real one)
        .zeros(8)
        .u16(0) // layer
        .u16(0) // alternate group
        .u16(0) // volume: video
        .u16(0);
    for m in MATRIX {
        w.u32(m);
    }
    w.u32(width << 16).u32(height << 16).end();

    w.begin(b"mdia");
    w.begin_full(b"mdhd", 0, 0)
        .u32(0)
        .u32(0)
        .u32(VIDEO_TIMESCALE)
        .u32(0)
        .u16(0x55C4) // language "und"
        .u16(0)
        .end();
    w.begin_full(b"hdlr", 0, 0)
        .u32(0)
        .bytes(b"vide")
        .zeros(12)
        .bytes(b"TLP-Desk video\0")
        .end();
    w.begin(b"minf");
    w.begin_full(b"vmhd", 0, 1).zeros(8).end();
    w.begin(b"dinf");
    w.begin_full(b"dref", 0, 0).u32(1);
    w.begin_full(b"url ", 0, 1).end(); // self-contained
    w.end().end(); // dref, dinf
    w.begin(b"stbl");
    w.begin_full(b"stsd", 0, 0).u32(1);
    write_avc1(w, track);
    w.end(); // stsd
    w.begin_full(b"stts", 0, 0).u32(0).end();
    w.begin_full(b"stsc", 0, 0).u32(0).end();
    w.begin_full(b"stsz", 0, 0).u32(0).u32(0).end();
    w.begin_full(b"stco", 0, 0).u32(0).end();
    w.end().end().end().end(); // stbl, minf, mdia, trak
}

fn write_avc1(w: &mut BoxWriter, track: &TrackConfig) {
    let info = &track.info;
    // Sides are capped at 16384 by the SPS parser, so they fit in 16 bits.
    let side = |v: u32| u16::try_from(v).unwrap_or(u16::MAX);
    w.begin(b"avc1")
        .zeros(6)
        .u16(1) // data_reference_index
        .zeros(16)
        .u16(side(info.width))
        .u16(side(info.height))
        .u32(0x0048_0000) // 72 dpi
        .u32(0x0048_0000)
        .u32(0)
        .u16(1); // frame_count
    let mut name = [0u8; 32];
    let label = b"TLP-Desk H.264";
    name[0] = label.len() as u8;
    name[1..=label.len()].copy_from_slice(label);
    w.bytes(&name).u16(0x0018).u16(0xFFFF);

    let len16 = |v: &[u8]| u16::try_from(v.len()).unwrap_or(u16::MAX);
    w.begin(b"avcC")
        .u8(1)
        .u8(info.profile_idc)
        .u8(info.constraint_flags)
        .u8(info.level_idc)
        .u8(0xFF) // 4-byte NAL lengths
        .u8(0xE1) // one SPS
        .u16(len16(&track.sps))
        .bytes(&track.sps)
        .u8(1) // one PPS
        .u16(len16(&track.pps))
        .bytes(&track.pps);
    if matches!(info.profile_idc, 100 | 110 | 122 | 144) {
        w.u8(0xFC | (info.chroma_format_idc & 0x3) as u8)
            .u8(0xF8 | (info.bit_depth_luma_minus8 & 0x7) as u8)
            .u8(0xF8 | (info.bit_depth_chroma_minus8 & 0x7) as u8)
            .u8(0);
    }
    w.end(); // avcC
    w.end(); // avc1
}

/// One sample ready to be written in a fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FragmentSample {
    pub duration: u32,
    pub sync: bool,
    pub data: Vec<u8>,
}

/// Builds `moof` + `mdat` for samples starting at decode time `base_dts`.
pub(crate) fn fragment(sequence: u32, base_dts: u64, samples: &[FragmentSample]) -> Vec<u8> {
    let payload: usize = samples.iter().map(|s| s.data.len()).sum();
    let mut w = BoxWriter {
        buf: Vec::with_capacity(payload + 128 + samples.len() * 12),
        open: Vec::new(),
    };
    w.begin(b"moof");
    w.begin_full(b"mfhd", 0, 0).u32(sequence).end();
    w.begin(b"traf");
    // default-base-is-moof: sample offsets are relative to the start of this moof.
    w.begin_full(b"tfhd", 0, 0x02_0000).u32(TRACK_ID).end();
    w.begin_full(b"tfdt", 1, 0).u64(base_dts).end();
    // data-offset | sample-duration | sample-size | sample-flags
    w.begin_full(b"trun", 0, 0x0701)
        .u32(u32::try_from(samples.len()).unwrap_or(u32::MAX));
    let data_offset_at = w.pos();
    w.u32(0);
    for s in samples {
        w.u32(s.duration)
            .u32(u32::try_from(s.data.len()).unwrap_or(u32::MAX))
            .u32(if s.sync { FLAGS_SYNC } else { FLAGS_NON_SYNC });
    }
    w.end().end().end(); // trun, traf, moof
    let data_offset = u32::try_from(w.pos() + 8).unwrap_or(u32::MAX);
    if let Some(slot) = w.buf.get_mut(data_offset_at..data_offset_at + 4) {
        slot.copy_from_slice(&data_offset.to_be_bytes());
    }
    w.begin(b"mdat");
    for s in samples {
        w.bytes(&s.data);
    }
    w.end();
    w.buf
}

/// One entry of the keyframe index in `tfra`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RandomAccess {
    /// Decode time of the keyframe (track timescale).
    pub time: u64,
    /// File offset of the `moof` containing it.
    pub moof_offset: u64,
    /// 1-based sample number within the fragment's `trun`.
    pub sample_number: u32,
}

/// Builds the `mfra` box (keyframe index) that closes a recording.
pub(crate) fn mfra(entries: &[RandomAccess]) -> Vec<u8> {
    let mut w = BoxWriter::default();
    w.begin(b"mfra");
    w.begin_full(b"tfra", 1, 0)
        .u32(TRACK_ID)
        // length_size_of_traf_num = 0, _trun_num = 0 (1 byte each), _sample_num = 3 (4 bytes)
        .u32(0x3)
        .u32(u32::try_from(entries.len()).unwrap_or(u32::MAX));
    for e in entries {
        w.u64(e.time)
            .u64(e.moof_offset)
            .u8(1)
            .u8(1)
            .u32(e.sample_number);
    }
    w.end();
    let size_so_far = u32::try_from(w.pos() + 16).unwrap_or(u32::MAX);
    w.begin_full(b"mfro", 0, 0).u32(size_so_far).end();
    w.end();
    w.buf
}

/// Track-timescale ticks to movie-timescale ticks (for `mvhd` / `mehd`).
pub(crate) fn movie_ticks(track_ticks: u64) -> u64 {
    track_ticks.saturating_mul(u64::from(MOVIE_TIMESCALE)) / u64::from(VIDEO_TIMESCALE)
}

/// Microseconds to track ticks.
pub(crate) fn us_to_ticks(us: u64) -> u64 {
    us.saturating_mul(u64::from(VIDEO_TIMESCALE)) / 1_000_000
}

/// Track ticks to microseconds.
pub(crate) fn ticks_to_us(ticks: u64, timescale: u32) -> u64 {
    if timescale == 0 {
        return 0;
    }
    ticks.saturating_mul(1_000_000) / u64::from(timescale)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_u32(b: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn nested_boxes_get_their_sizes() {
        let mut w = BoxWriter::default();
        w.begin(b"outr").begin(b"innr").u32(7).end().end();
        assert_eq!(read_u32(&w.buf, 0), 20);
        assert_eq!(&w.buf[4..8], b"outr");
        assert_eq!(read_u32(&w.buf, 8), 12);
        assert_eq!(&w.buf[12..16], b"innr");
    }

    #[test]
    fn fragment_data_offset_points_at_the_samples() {
        let samples = [
            FragmentSample {
                duration: 3000,
                sync: true,
                data: vec![1, 2, 3],
            },
            FragmentSample {
                duration: 3000,
                sync: false,
                data: vec![4, 5],
            },
        ];
        let f = fragment(1, 0, &samples);
        let moof_size = read_u32(&f, 0) as usize;
        assert_eq!(&f[moof_size + 4..moof_size + 8], b"mdat");
        assert_eq!(&f[moof_size + 8..], &[1, 2, 3, 4, 5]);
        // trun data_offset == moof size + mdat header
        let trun = f.windows(4).position(|w| w == b"trun").unwrap();
        assert_eq!(read_u32(&f, trun + 12) as usize, moof_size + 8);
    }

    #[test]
    fn mfro_holds_the_mfra_size() {
        let m = mfra(&[RandomAccess {
            time: 0,
            moof_offset: 100,
            sample_number: 1,
        }]);
        let n = m.len();
        assert_eq!(read_u32(&m, 0) as usize, n);
        assert_eq!(read_u32(&m, n - 4) as usize, n);
    }

    #[test]
    fn time_conversions() {
        assert_eq!(us_to_ticks(1_000_000), 90_000);
        assert_eq!(movie_ticks(90_000), 1_000);
        assert_eq!(ticks_to_us(45_000, VIDEO_TIMESCALE), 500_000);
        assert_eq!(ticks_to_us(1, 0), 0);
        assert_eq!(us_to_ticks(u64::MAX), u64::MAX / 1_000_000);
    }
}
