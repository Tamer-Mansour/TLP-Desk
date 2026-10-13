//! One recording file: an init segment, fragments appended as samples arrive, and an `mfra`
//! index plus patched durations when it is closed. Also repairs files whose recording was
//! interrupted (crash, power loss, full disk).

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::demux::{demux, Demuxed};
use super::mp4::{
    fragment, init_segment, mfra, movie_ticks, us_to_ticks, FragmentSample, RandomAccess,
    TrackConfig, VIDEO_TIMESCALE,
};
use crate::ViewerError;

/// A fragment is written at the latest after this much media time, so a crash loses at most
/// about this much video.
pub(crate) const MAX_FRAGMENT: Duration = Duration::from_secs(1);
/// ... or once it holds this many bytes (bounds memory and the size of one write).
pub(crate) const MAX_FRAGMENT_BYTES: usize = 8 << 20;
/// When no new frame arrives for this long (static screen), the pending one is written anyway
/// with the time waited so far as its duration.
pub(crate) const IDLE_FLUSH: Duration = Duration::from_secs(2);
/// Duration given to the very last sample of a file: one frame at 30 fps.
const LAST_SAMPLE_TICKS: u32 = VIDEO_TIMESCALE / 30;

fn io(e: std::io::Error) -> ViewerError {
    ViewerError::RecordingIo(e)
}

/// Opens a new file for writing. On Windows other programs may read it while it is being
/// recorded (e.g. to copy it), but not write or delete it.
fn create_exclusive(path: &Path) -> Result<File, ViewerError> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;
        opts.share_mode(FILE_SHARE_READ);
    }
    opts.open(path).map_err(io)
}

struct Pending {
    dts: u64,
    sync: bool,
    data: Vec<u8>,
    arrived: Instant,
}

/// Writes one fragmented MP4 file.
pub(crate) struct SegmentWriter {
    file: File,
    path: PathBuf,
    track: TrackConfig,
    mvhd_duration_at: u64,
    mehd_duration_at: u64,
    /// File length so far.
    pos: u64,
    sequence: u32,
    /// Host timestamp (us) that maps to decode time 0.
    origin_us: u64,
    pending: Vec<Pending>,
    pending_bytes: usize,
    /// Decode time after the last written sample: the next sample starts no earlier.
    written_end: u64,
    last_duration: u32,
    index: Vec<RandomAccess>,
}

impl SegmentWriter {
    /// Creates `path` (it must not exist) and writes the init segment.
    pub fn create(path: &Path, track: TrackConfig, origin_us: u64) -> Result<Self, ViewerError> {
        let mut file = create_exclusive(path)?;
        let init = init_segment(&track);
        file.write_all(&init.bytes).map_err(io)?;
        Ok(Self {
            file,
            path: path.to_owned(),
            track,
            mvhd_duration_at: init.mvhd_duration_at,
            mehd_duration_at: init.mehd_duration_at,
            pos: init.bytes.len() as u64,
            sequence: 0,
            origin_us,
            pending: Vec::new(),
            pending_bytes: 0,
            written_end: 0,
            last_duration: LAST_SAMPLE_TICKS,
            index: Vec::new(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn track(&self) -> &TrackConfig {
        &self.track
    }

    /// Bytes in the file so far (written fragments only).
    pub fn bytes_written(&self) -> u64 {
        self.pos
    }

    /// Media time recorded so far (written and pending samples).
    pub fn duration(&self) -> Duration {
        let end = self
            .pending
            .last()
            .map_or(self.written_end, |p| p.dts.max(self.written_end));
        Duration::from_micros(super::mp4::ticks_to_us(end, VIDEO_TIMESCALE))
    }

    /// Adds one sample (MP4 form) captured at host time `timestamp_us`.
    pub fn push(
        &mut self,
        timestamp_us: u64,
        sync: bool,
        data: Vec<u8>,
        now: Instant,
    ) -> Result<(), ViewerError> {
        let floor = self
            .pending
            .last()
            .map_or(self.written_end, |p| p.dts.saturating_add(1));
        // Host timestamps are monotonic, but never trust them to be: clamp so decode times
        // strictly increase.
        let dts = us_to_ticks(timestamp_us.saturating_sub(self.origin_us)).max(floor);
        if let Some(first) = self.pending.first() {
            let span = dts.saturating_sub(first.dts);
            let full = span >= us_to_ticks(MAX_FRAGMENT.as_micros() as u64)
                || self.pending_bytes.saturating_add(data.len()) > MAX_FRAGMENT_BYTES;
            if sync || full {
                self.flush(Some(dts))?;
            }
        }
        self.pending_bytes = self.pending_bytes.saturating_add(data.len());
        self.pending.push(Pending {
            dts,
            sync,
            data,
            arrived: now,
        });
        Ok(())
    }

    /// Writes the pending samples if nothing new arrived for [`IDLE_FLUSH`].
    pub fn tick(&mut self, now: Instant) -> Result<(), ViewerError> {
        let idle = self
            .pending
            .last()
            .is_some_and(|p| now.saturating_duration_since(p.arrived) >= IDLE_FLUSH);
        if idle {
            self.flush(None)?;
        }
        Ok(())
    }

    /// Writes pending samples as one fragment. `next_dts` is the decode time of the sample
    /// that follows (gives the last pending sample its duration); without it the last sample
    /// lasts as long as we have waited for a successor.
    fn flush(&mut self, next_dts: Option<u64>) -> Result<(), ViewerError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let pending = std::mem::take(&mut self.pending);
        self.pending_bytes = 0;
        let base_dts = pending.first().map_or(self.written_end, |p| p.dts);
        let mut durations = Vec::with_capacity(pending.len());
        let mut end = base_dts;
        for (i, p) in pending.iter().enumerate() {
            let next = match (pending.get(i + 1), next_dts) {
                (Some(n), _) => n.dts,
                (None, Some(n)) => n,
                (None, None) => {
                    let waited = us_to_ticks(p.arrived.elapsed().as_micros() as u64);
                    p.dts
                        .saturating_add(waited.max(u64::from(LAST_SAMPLE_TICKS)))
                }
            };
            let duration = u32::try_from(next.saturating_sub(p.dts).max(1)).unwrap_or(u32::MAX);
            end = p.dts.saturating_add(u64::from(duration));
            durations.push(duration);
        }
        self.sequence = self.sequence.wrapping_add(1);
        let moof_offset = self.pos;
        let mut index = Vec::new();
        let mut samples = Vec::with_capacity(pending.len());
        for (n, (p, duration)) in pending.into_iter().zip(durations).enumerate() {
            if p.sync {
                index.push(RandomAccess {
                    time: p.dts,
                    moof_offset,
                    sample_number: u32::try_from(n + 1).unwrap_or(u32::MAX),
                });
            }
            self.last_duration = duration;
            samples.push(FragmentSample {
                duration,
                sync: p.sync,
                data: p.data,
            });
        }
        let bytes = fragment(self.sequence, base_dts, &samples);
        self.file.write_all(&bytes).map_err(io)?;
        self.pos = self.pos.saturating_add(bytes.len() as u64);
        self.index.extend(index);
        self.written_end = end;
        Ok(())
    }

    /// Writes what is pending, the keyframe index and the final duration, and closes the file.
    pub fn finish(mut self) -> Result<SegmentInfo, ViewerError> {
        if let Some(last) = self.pending.last() {
            // The last frame lasts as long as the one before it.
            let n = self.pending.len();
            let step = match self.pending.get(n.saturating_sub(2)).filter(|_| n >= 2) {
                Some(prev) => last.dts.saturating_sub(prev.dts),
                None => u64::from(self.last_duration),
            };
            let next = last.dts.saturating_add(step.max(1));
            self.flush(Some(next))?;
        }
        let tail = mfra(&self.index);
        self.file.write_all(&tail).map_err(io)?;
        self.pos = self.pos.saturating_add(tail.len() as u64);
        patch_durations(
            &mut self.file,
            Some((self.mvhd_duration_at, 0)),
            Some((self.mehd_duration_at, 1)),
            self.written_end,
        )?;
        self.file.sync_all().map_err(io)?;
        Ok(SegmentInfo {
            path: self.path,
            bytes: self.pos,
            duration: Duration::from_micros(super::mp4::ticks_to_us(
                self.written_end,
                VIDEO_TIMESCALE,
            )),
        })
    }
}

/// A finished recording file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentInfo {
    pub path: PathBuf,
    pub bytes: u64,
    pub duration: Duration,
}

/// Writes the total duration into `mvhd` and `mehd` (both in movie milliseconds).
fn patch_durations(
    file: &mut File,
    mvhd: Option<(u64, u8)>,
    mehd: Option<(u64, u8)>,
    end_ticks: u64,
) -> Result<(), ViewerError> {
    let ms = movie_ticks(end_ticks);
    for field in [mvhd, mehd].into_iter().flatten() {
        let (at, version) = field;
        file.seek(SeekFrom::Start(at)).map_err(io)?;
        if version == 1 {
            file.write_all(&ms.to_be_bytes()).map_err(io)?;
        } else {
            let v = u32::try_from(ms).unwrap_or(u32::MAX);
            file.write_all(&v.to_be_bytes()).map_err(io)?;
        }
    }
    file.seek(SeekFrom::End(0)).map_err(io)?;
    Ok(())
}

/// What [`recover_file`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// The file was closed properly; nothing to do.
    AlreadyComplete,
    /// The damaged tail was cut and the index and duration were written. `kept` is how much
    /// video the file holds now.
    Repaired { kept: Duration, removed_bytes: u64 },
}

/// Repairs a recording that was interrupted: keeps every complete fragment, cuts whatever
/// follows the last one, then writes the keyframe index and the duration like a normal close.
///
/// Fails if the file is not a recording we can read (no movie header) or is being recorded
/// right now (Windows refuses to open it for writing).
pub fn recover_file(path: &Path) -> Result<Recovery, ViewerError> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(io)?;
    let d: Demuxed = demux(&mut file)?;
    if d.complete {
        return Ok(Recovery::AlreadyComplete);
    }
    file.set_len(d.valid_end).map_err(io)?;
    file.seek(SeekFrom::End(0)).map_err(io)?;
    let index: Vec<RandomAccess> = d
        .fragments
        .iter()
        .enumerate()
        .flat_map(|(f, &(moof_offset, first))| {
            let last = d.fragments.get(f + 1).map_or(d.samples.len(), |n| n.1);
            let samples = d.samples.get(first..last).unwrap_or_default();
            samples
                .iter()
                .enumerate()
                .filter(|(_, s)| s.sync)
                .map(move |(n, s)| RandomAccess {
                    time: s.dts,
                    moof_offset,
                    sample_number: u32::try_from(n + 1).unwrap_or(u32::MAX),
                })
                .collect::<Vec<_>>()
        })
        .collect();
    file.write_all(&mfra(&index)).map_err(io)?;
    let end = d.end_dts();
    let end_track_in_video_ticks = if d.timescale == VIDEO_TIMESCALE {
        end
    } else {
        end.saturating_mul(u64::from(VIDEO_TIMESCALE)) / u64::from(d.timescale.max(1))
    };
    patch_durations(
        &mut file,
        d.mvhd_duration_at,
        d.mehd_duration_at,
        end_track_in_video_ticks,
    )?;
    file.sync_all().map_err(io)?;
    Ok(Recovery::Repaired {
        kept: Duration::from_micros(super::mp4::ticks_to_us(end, d.timescale)),
        removed_bytes: d.file_len.saturating_sub(d.valid_end),
    })
}
