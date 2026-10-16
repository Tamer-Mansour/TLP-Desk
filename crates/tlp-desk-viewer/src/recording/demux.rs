//! Reader for the fragmented MP4 files the recorder writes: builds a sample index (file offset,
//! size, decode time, keyframe flag) for playback, seeking and crash recovery.
//!
//! Files on disk are untrusted (they may be truncated by a crash, or be any `.mp4` the user
//! put in the folder), so every size and offset is checked against the file length, box
//! nesting is bounded, and the in-memory index is capped. A damaged tail is not an error: the
//! index stops at the last complete fragment and [`Demuxed::valid_end`] says where that is.

use std::io::{Read, Seek, SeekFrom};

use super::mp4::{FLAGS_NON_SYNC, MOVIE_TIMESCALE};
use crate::ViewerError;

/// Largest `moov` / `moof` we load into memory.
const MAX_HEADER_BOX: u64 = 8 << 20;
/// Largest number of samples we index (about 46 hours at 60 fps).
const MAX_SAMPLES: usize = 10_000_000;
/// Largest single sample (an encoded picture) we accept.
const MAX_SAMPLE_SIZE: u32 = 64 << 20;

fn bad(reason: &'static str) -> ViewerError {
    ViewerError::BadRecording(reason)
}

/// One indexed sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SampleRef {
    pub offset: u64,
    pub size: u32,
    /// Decode time in track ticks.
    pub dts: u64,
    pub duration: u32,
    pub sync: bool,
}

/// What we learned from a file.
#[derive(Debug, Clone, Default)]
pub(crate) struct Demuxed {
    /// Track timescale (ticks per second).
    pub timescale: u32,
    pub width: u32,
    pub height: u32,
    /// Parameter sets from `avcC`.
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
    pub samples: Vec<SampleRef>,
    /// `(moof offset, index of its first sample)` per fragment.
    pub fragments: Vec<(u64, usize)>,
    /// End of the last complete box we could use (`moov` or a full `moof`+`mdat`).
    pub valid_end: u64,
    pub file_len: u64,
    /// The file ends with an `mfra` right after the last fragment (closed cleanly).
    pub complete: bool,
    /// Offset and version of `mvhd.duration`.
    pub mvhd_duration_at: Option<(u64, u8)>,
    /// Offset and version of `mehd.fragment_duration`.
    pub mehd_duration_at: Option<(u64, u8)>,
    /// `mehd` duration in movie ticks (0 while recording).
    pub mehd_duration: u64,
    pub movie_timescale: u32,
}

impl Demuxed {
    /// Track ticks after the last sample.
    pub fn end_dts(&self) -> u64 {
        self.samples
            .last()
            .map_or(0, |s| s.dts.saturating_add(u64::from(s.duration)))
    }
}

/// A box header inside an in-memory buffer.
struct Child<'a> {
    kind: [u8; 4],
    /// Offset of the body within the parent buffer.
    body_at: usize,
    body: &'a [u8],
}

/// Iterates the boxes in `data`; stops (without error) at the first malformed header.
fn children(data: &[u8]) -> impl Iterator<Item = Child<'_>> {
    let mut pos = 0usize;
    std::iter::from_fn(move || {
        let head = data.get(pos..pos.checked_add(8)?)?;
        let size32 = u32::from_be_bytes(head.get(..4)?.try_into().ok()?);
        let kind: [u8; 4] = head.get(4..8)?.try_into().ok()?;
        let (header, size) = match size32 {
            0 => (8usize, data.len().checked_sub(pos)?),
            1 => {
                let large = data.get(pos + 8..pos + 16)?;
                let size = u64::from_be_bytes(large.try_into().ok()?);
                (16, usize::try_from(size).ok()?)
            }
            n => (8, usize::try_from(n).ok()?),
        };
        if size < header {
            return None;
        }
        let end = pos.checked_add(size)?;
        let body = data.get(pos + header..end)?;
        let child = Child {
            kind,
            body_at: pos + header,
            body,
        };
        pos = end;
        Some(child)
    })
}

fn find<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<Child<'a>> {
    children(data).find(|c| &c.kind == kind)
}

/// Byte reader over a box body.
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], ViewerError> {
        let end = self.pos.checked_add(n).ok_or(bad("box too short"))?;
        let s = self.data.get(self.pos..end).ok_or(bad("box too short"))?;
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, ViewerError> {
        Ok(*self.take(1)?.first().ok_or(bad("box too short"))?)
    }
    fn u16(&mut self) -> Result<u16, ViewerError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes(b.try_into().map_err(|_| bad("u16"))?))
    }
    fn u32(&mut self) -> Result<u32, ViewerError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes(b.try_into().map_err(|_| bad("u32"))?))
    }
    fn u64(&mut self) -> Result<u64, ViewerError> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes(b.try_into().map_err(|_| bad("u64"))?))
    }
    fn skip(&mut self, n: usize) -> Result<(), ViewerError> {
        self.take(n).map(|_| ())
    }
    /// Version and flags of a full box.
    fn full(&mut self) -> Result<(u8, u32), ViewerError> {
        let v = self.u32()?;
        Ok(((v >> 24) as u8, v & 0x00FF_FFFF))
    }
}

/// Reads the whole file index. Fails only when the file has no usable `moov`.
pub(crate) fn demux<R: Read + Seek>(r: &mut R) -> Result<Demuxed, ViewerError> {
    let file_len = r.seek(SeekFrom::End(0)).map_err(ViewerError::RecordingIo)?;
    let mut d = Demuxed {
        file_len,
        movie_timescale: MOVIE_TIMESCALE,
        ..Demuxed::default()
    };
    let mut have_moov = false;
    let mut trex_duration = 0u32;
    let mut pending: Option<PendingFragment> = None;
    let mut pos = 0u64;
    let mut buf = Vec::new();

    while let Some(end_of_header) = pos.checked_add(8).filter(|e| *e <= file_len) {
        r.seek(SeekFrom::Start(pos))
            .map_err(ViewerError::RecordingIo)?;
        let mut head = [0u8; 16];
        r.read_exact(&mut head[..8])
            .map_err(ViewerError::RecordingIo)?;
        let size32 = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
        let kind = [head[4], head[5], head[6], head[7]];
        let (header, size) = match size32 {
            0 => (8u64, file_len - pos),
            1 => {
                if end_of_header + 8 > file_len {
                    break;
                }
                r.read_exact(&mut head[8..16])
                    .map_err(ViewerError::RecordingIo)?;
                let mut large = [0u8; 8];
                large.copy_from_slice(&head[8..16]);
                (16, u64::from_be_bytes(large))
            }
            n => (8, u64::from(n)),
        };
        if size < header {
            break;
        }
        let Some(end) = pos.checked_add(size).filter(|e| *e <= file_len) else {
            break; // truncated box: the tail of an interrupted recording
        };
        let body_at = pos + header;
        let body_len = size - header;
        match &kind {
            b"moov" | b"moof" => {
                if body_len > MAX_HEADER_BOX {
                    break;
                }
                buf.clear();
                buf.resize(body_len as usize, 0);
                r.read_exact(&mut buf).map_err(ViewerError::RecordingIo)?;
                if &kind == b"moov" {
                    if have_moov {
                        break;
                    }
                    parse_moov(&buf, body_at, &mut d, &mut trex_duration)?;
                    have_moov = true;
                    d.valid_end = end;
                } else {
                    if !have_moov {
                        break;
                    }
                    match parse_moof(&buf, pos, trex_duration) {
                        Ok(f) => pending = Some(f),
                        Err(_) => break,
                    }
                }
            }
            b"mdat" => {
                if let Some(f) = pending.take() {
                    let data_start = body_at;
                    let fits = f.samples.iter().all(|s| {
                        s.offset >= data_start
                            && s.offset
                                .checked_add(u64::from(s.size))
                                .is_some_and(|e| e <= end)
                    });
                    if !fits || d.samples.len() + f.samples.len() > MAX_SAMPLES {
                        break;
                    }
                    d.fragments.push((f.moof_offset, d.samples.len()));
                    d.samples.extend(f.samples);
                    d.valid_end = end;
                }
            }
            b"mfra" => {
                if have_moov && pos == d.valid_end && end == file_len {
                    d.complete = true;
                }
                break;
            }
            _ => {
                if pending.is_some() {
                    break; // a moof not followed by its mdat
                }
            }
        }
        pos = end;
    }
    if !have_moov {
        return Err(bad("no movie header (moov)"));
    }
    if d.sps.is_empty() || d.pps.is_empty() {
        return Err(bad("no H.264 parameter sets (avcC)"));
    }
    Ok(d)
}

fn parse_moov(
    moov: &[u8],
    moov_at: u64,
    d: &mut Demuxed,
    trex_duration: &mut u32,
) -> Result<(), ViewerError> {
    if let Some(mvhd) = find(moov, b"mvhd") {
        let mut c = Cursor::new(mvhd.body);
        let (version, _) = c.full()?;
        let at = moov_at + mvhd.body_at as u64;
        if version == 1 {
            c.skip(16)?;
            d.movie_timescale = c.u32()?;
            d.mvhd_duration_at = Some((at + 24, 1));
        } else {
            c.skip(8)?;
            d.movie_timescale = c.u32()?;
            d.mvhd_duration_at = Some((at + 16, 0));
        }
    }
    if let Some(mvex) = find(moov, b"mvex") {
        if let Some(mehd) = find(mvex.body, b"mehd") {
            let mut c = Cursor::new(mehd.body);
            let (version, _) = c.full()?;
            let at = moov_at + (mvex.body_at + mehd.body_at) as u64 + 4;
            d.mehd_duration = if version == 1 {
                c.u64()?
            } else {
                u64::from(c.u32()?)
            };
            d.mehd_duration_at = Some((at, version));
        }
        if let Some(trex) = find(mvex.body, b"trex") {
            let mut c = Cursor::new(trex.body);
            c.full()?;
            c.skip(8)?; // track_ID, default_sample_description_index
            *trex_duration = c.u32()?;
        }
    }
    let trak = find(moov, b"trak").ok_or(bad("no track"))?;
    if let Some(tkhd) = find(trak.body, b"tkhd") {
        let mut c = Cursor::new(tkhd.body);
        let (version, _) = c.full()?;
        c.skip(if version == 1 { 32 } else { 20 })?;
        c.skip(52)?;
        d.width = c.u32()? >> 16;
        d.height = c.u32()? >> 16;
    }
    let mdia = find(trak.body, b"mdia").ok_or(bad("no media box"))?;
    let mdhd = find(mdia.body, b"mdhd").ok_or(bad("no media header"))?;
    let mut c = Cursor::new(mdhd.body);
    let (version, _) = c.full()?;
    c.skip(if version == 1 { 16 } else { 8 })?;
    d.timescale = c.u32()?;
    if d.timescale == 0 {
        return Err(bad("zero timescale"));
    }
    let stsd = find(mdia.body, b"minf")
        .and_then(|minf| find(minf.body, b"stbl"))
        .and_then(|stbl| find(stbl.body, b"stsd"))
        .ok_or(bad("no sample description"))?;
    let entries = stsd.body.get(8..).ok_or(bad("short stsd"))?;
    let entry = children(entries)
        .next()
        .ok_or(bad("empty sample description"))?;
    if &entry.kind != b"avc1" && &entry.kind != b"avc3" {
        return Err(bad("not an H.264 track"));
    }
    let mut c = Cursor::new(entry.body);
    c.skip(24)?;
    let (w, h) = (c.u16()?, c.u16()?);
    if d.width == 0 || d.height == 0 {
        d.width = u32::from(w);
        d.height = u32::from(h);
    }
    let avcc = entry
        .body
        .get(78..)
        .and_then(|rest| find(rest, b"avcC"))
        .ok_or(bad("no avcC"))?;
    parse_avcc(avcc.body, d)
}

fn parse_avcc(body: &[u8], d: &mut Demuxed) -> Result<(), ViewerError> {
    let mut c = Cursor::new(body);
    c.skip(5)?;
    let n_sps = c.u8()? & 0x1F;
    for _ in 0..n_sps {
        let len = usize::from(c.u16()?);
        d.sps.push(c.take(len)?.to_vec());
    }
    let n_pps = c.u8()?;
    for _ in 0..n_pps {
        let len = usize::from(c.u16()?);
        d.pps.push(c.take(len)?.to_vec());
    }
    Ok(())
}

struct PendingFragment {
    moof_offset: u64,
    samples: Vec<SampleRef>,
}

fn parse_moof(
    moof: &[u8],
    moof_offset: u64,
    trex_duration: u32,
) -> Result<PendingFragment, ViewerError> {
    let traf = find(moof, b"traf").ok_or(bad("fragment without traf"))?;
    let tfhd = find(traf.body, b"tfhd").ok_or(bad("fragment without tfhd"))?;
    let mut c = Cursor::new(tfhd.body);
    let (_, flags) = c.full()?;
    let _track = c.u32()?;
    let mut base = moof_offset;
    if flags & 0x1 != 0 {
        base = c.u64()?;
    }
    if flags & 0x2 != 0 {
        c.skip(4)?;
    }
    let mut default_duration = trex_duration;
    if flags & 0x8 != 0 {
        default_duration = c.u32()?;
    }
    let mut default_size = 0;
    if flags & 0x10 != 0 {
        default_size = c.u32()?;
    }
    let mut default_flags = FLAGS_NON_SYNC;
    if flags & 0x20 != 0 {
        default_flags = c.u32()?;
    }
    let tfdt = find(traf.body, b"tfdt").ok_or(bad("fragment without tfdt"))?;
    let mut c = Cursor::new(tfdt.body);
    let (version, _) = c.full()?;
    let mut dts = if version == 1 {
        c.u64()?
    } else {
        u64::from(c.u32()?)
    };

    let mut samples = Vec::new();
    for trun in children(traf.body).filter(|b| &b.kind == b"trun") {
        let mut c = Cursor::new(trun.body);
        let (_, tflags) = c.full()?;
        let count = c.u32()?;
        let per_sample = [0x100u32, 0x200, 0x400, 0x800]
            .iter()
            .filter(|f| tflags & **f != 0)
            .count();
        // Every per-sample field is 4 bytes: reject counts the box cannot hold before
        // allocating anything for them.
        let needed = (count as usize).checked_mul(per_sample * 4);
        if count as usize > MAX_SAMPLES || needed.is_none_or(|n| n > trun.body.len()) {
            return Err(bad("sample count exceeds the fragment"));
        }
        let mut offset = base;
        if tflags & 0x1 != 0 {
            let rel = c.u32()? as i32;
            offset = base
                .checked_add_signed(i64::from(rel))
                .ok_or(bad("data offset"))?;
        }
        let mut first_flags = None;
        if tflags & 0x4 != 0 {
            first_flags = Some(c.u32()?);
        }
        samples.reserve(count as usize);
        for i in 0..count {
            let duration = if tflags & 0x100 != 0 {
                c.u32()?
            } else {
                default_duration
            };
            let size = if tflags & 0x200 != 0 {
                c.u32()?
            } else {
                default_size
            };
            let mut sflags = if tflags & 0x400 != 0 {
                c.u32()?
            } else {
                default_flags
            };
            if i == 0 {
                if let Some(f) = first_flags {
                    sflags = f;
                }
            }
            if tflags & 0x800 != 0 {
                c.skip(4)?;
            }
            if size == 0 || size > MAX_SAMPLE_SIZE {
                return Err(bad("sample size out of range"));
            }
            samples.push(SampleRef {
                offset,
                size,
                dts,
                duration,
                sync: sflags & 0x0001_0000 == 0,
            });
            offset = offset
                .checked_add(u64::from(size))
                .ok_or(bad("sample offset"))?;
            dts = dts.saturating_add(u64::from(duration));
        }
    }
    Ok(PendingFragment {
        moof_offset,
        samples,
    })
}

/// Cheap facts about a file for the recordings list (reads the header and the last bytes only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Probe {
    /// From `mehd` (set when the file was closed or repaired).
    pub duration: Option<std::time::Duration>,
    pub complete: bool,
}

/// Reads the movie header from the first megabyte and checks for the closing index.
pub(crate) fn probe<R: Read + Seek>(r: &mut R) -> Result<Probe, ViewerError> {
    const HEAD: u64 = 1 << 20;
    let len = r.seek(SeekFrom::End(0)).map_err(ViewerError::RecordingIo)?;
    r.seek(SeekFrom::Start(0))
        .map_err(ViewerError::RecordingIo)?;
    let mut head = Vec::new();
    r.by_ref()
        .take(HEAD.min(len))
        .read_to_end(&mut head)
        .map_err(ViewerError::RecordingIo)?;
    let moov = find(&head, b"moov").ok_or(bad("no movie header (moov)"))?;
    let mut d = Demuxed::default();
    let mut trex = 0;
    parse_moov(moov.body, 0, &mut d, &mut trex)?;
    let duration = (d.mehd_duration > 0 && d.movie_timescale > 0).then(|| {
        std::time::Duration::from_micros(
            d.mehd_duration.saturating_mul(1_000_000) / u64::from(d.movie_timescale),
        )
    });
    Ok(Probe {
        duration,
        complete: super::files::ends_with_index(r, len),
    })
}
