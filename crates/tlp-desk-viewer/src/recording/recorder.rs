//! The recording state machine, independent of threads and sessions so it can be tested with
//! synthetic frames: waits for a keyframe, splits into a new file when the stream's parameter
//! sets (picture size) change, drops to the next keyframe after a gap, asks for keyframes so
//! the file stays seekable, and watches the free disk space.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::files::{base_name, next_free_path, Stamp};
use super::mp4::TrackConfig;
use super::nal::{parse_access_unit, parse_sps};
use super::segment::{SegmentInfo, SegmentWriter};
use super::{KeyframeFn, TapFrame};
use crate::ViewerError;

/// Free space is re-checked after this many bytes were written...
const SPACE_CHECK_BYTES: u64 = 32 << 20;
/// ... or after this long.
const SPACE_CHECK_EVERY: Duration = Duration::from_secs(10);
/// Keyframe requests are not repeated faster than this.
const KEYFRAME_RETRY: Duration = Duration::from_secs(1);

/// Returns the free bytes on the volume holding a folder.
pub(crate) type SpaceFn = Box<dyn Fn(&Path) -> std::io::Result<u64> + Send>;

pub(crate) struct Core {
    folder: PathBuf,
    base: String,
    min_free_bytes: u64,
    keyframe_interval: Duration,
    request_keyframe: KeyframeFn,
    free_space: SpaceFn,
    segment: Option<SegmentWriter>,
    done: Vec<SegmentInfo>,
    waiting_for_keyframe: bool,
    last_seq: Option<u64>,
    last_keyframe: Option<Instant>,
    last_request: Option<Instant>,
    checked_at: (Instant, u64),
    /// Bytes in finished files (for the total).
    done_bytes: u64,
}

impl Core {
    /// Prepares a recording into `folder` (created if needed) and checks the free space.
    pub fn new(
        folder: &Path,
        peer_name: &str,
        min_free_bytes: u64,
        keyframe_interval: Duration,
        request_keyframe: KeyframeFn,
        free_space: SpaceFn,
        now: Instant,
    ) -> Result<Self, ViewerError> {
        std::fs::create_dir_all(folder).map_err(ViewerError::RecordingIo)?;
        let core = Self {
            folder: folder.to_owned(),
            base: base_name(peer_name, Stamp::now()),
            min_free_bytes,
            keyframe_interval,
            request_keyframe,
            free_space,
            segment: None,
            done: Vec::new(),
            waiting_for_keyframe: true,
            last_seq: None,
            last_keyframe: None,
            last_request: None,
            checked_at: (now, 0),
            done_bytes: 0,
        };
        core.check_space(min_free_bytes)?;
        Ok(core)
    }

    fn check_space(&self, needed: u64) -> Result<(), ViewerError> {
        match (self.free_space)(&self.folder) {
            Ok(available) if available < needed => Err(ViewerError::DiskFull { available, needed }),
            Ok(_) => Ok(()),
            Err(e) => {
                // Unknown free space is not a reason to refuse; a full disk still surfaces
                // as a write error.
                tracing::debug!(%e, "cannot query free disk space");
                Ok(())
            }
        }
    }

    fn ask_keyframe(&mut self, now: Instant) {
        let recent = self
            .last_request
            .is_some_and(|t| now.saturating_duration_since(t) < KEYFRAME_RETRY);
        if !recent {
            self.last_request = Some(now);
            (self.request_keyframe)();
        }
    }

    /// Total bytes written so far.
    pub fn bytes(&self) -> u64 {
        self.done_bytes.saturating_add(
            self.segment
                .as_ref()
                .map_or(0, SegmentWriter::bytes_written),
        )
    }

    /// Media time recorded so far, over all files.
    pub fn duration(&self) -> Duration {
        let done: Duration = self.done.iter().map(|s| s.duration).sum();
        done + self
            .segment
            .as_ref()
            .map_or(Duration::ZERO, SegmentWriter::duration)
    }

    /// Files written so far (finished ones, then the open one).
    pub fn files(&self) -> Vec<PathBuf> {
        self.done
            .iter()
            .map(|s| s.path.clone())
            .chain(self.segment.as_ref().map(|s| s.path().to_owned()))
            .collect()
    }

    /// Handles one received video frame.
    pub fn on_frame(&mut self, frame: TapFrame, now: Instant) -> Result<(), ViewerError> {
        let gap = self
            .last_seq
            .is_some_and(|last| frame.seq != last.wrapping_add(1));
        self.last_seq = Some(frame.seq);
        if gap {
            // A lost frame breaks the reference chain until the next keyframe.
            tracing::debug!(
                seq = frame.seq,
                "gap in the video stream; waiting for a keyframe"
            );
            self.waiting_for_keyframe = true;
        }
        let au = match parse_access_unit(&frame.data) {
            Ok(au) => au,
            Err(e) => {
                tracing::debug!(%e, seq = frame.seq, "unrecordable frame; waiting for a keyframe");
                self.waiting_for_keyframe = true;
                self.ask_keyframe(now);
                return Ok(());
            }
        };
        let keyframe = au.idr || frame.keyframe;
        if self.waiting_for_keyframe && !keyframe {
            self.ask_keyframe(now);
            return Ok(());
        }
        if keyframe {
            self.last_keyframe = Some(now);
            if let (Some(sps), Some(pps)) = (au.sps.first(), au.pps.first()) {
                let same = self
                    .segment
                    .as_ref()
                    .is_some_and(|s| &s.track().sps == sps && &s.track().pps == pps);
                if !same {
                    let info = parse_sps(sps)?;
                    let track = TrackConfig {
                        info,
                        sps: sps.clone(),
                        pps: pps.clone(),
                    };
                    self.start_segment(track, frame.timestamp_us)?;
                }
            }
            if self.segment.is_none() {
                // A keyframe without parameter sets cannot start a file.
                self.ask_keyframe(now);
                return Ok(());
            }
            self.waiting_for_keyframe = false;
        }
        let Some(segment) = self.segment.as_mut() else {
            return Ok(());
        };
        segment.push(frame.timestamp_us, keyframe, au.sample, now)?;
        self.periodic(now)
    }

    /// Housekeeping without a frame: writes out a waiting frame on a static screen, keeps
    /// keyframes coming, re-checks the disk.
    pub fn on_tick(&mut self, now: Instant) -> Result<(), ViewerError> {
        if let Some(s) = self.segment.as_mut() {
            s.tick(now)?;
        }
        if self.waiting_for_keyframe {
            self.ask_keyframe(now);
        }
        self.periodic(now)
    }

    fn periodic(&mut self, now: Instant) -> Result<(), ViewerError> {
        let due = self
            .last_keyframe
            .is_some_and(|t| now.saturating_duration_since(t) >= self.keyframe_interval);
        if due {
            // Regular keyframes make the file seekable and bound what a damaged fragment
            // can break.
            self.last_keyframe = Some(now);
            self.ask_keyframe(now);
        }
        let bytes = self.bytes();
        let (at, at_bytes) = self.checked_at;
        if bytes.saturating_sub(at_bytes) >= SPACE_CHECK_BYTES
            || now.saturating_duration_since(at) >= SPACE_CHECK_EVERY
        {
            self.checked_at = (now, bytes);
            // Stop with half the start-up margin left, so the disk never fills completely.
            self.check_space(self.min_free_bytes / 2)?;
        }
        Ok(())
    }

    fn start_segment(&mut self, track: TrackConfig, origin_us: u64) -> Result<(), ViewerError> {
        if let Some(old) = self.segment.take() {
            tracing::info!(
                width = track.info.width,
                height = track.info.height,
                "remote picture changed; continuing the recording in a new file"
            );
            self.finish_segment(old)?;
        }
        let path = next_free_path(&self.folder, &self.base);
        let writer = SegmentWriter::create(&path, track, origin_us)?;
        tracing::info!(path = %path.display(), "recording to file");
        self.segment = Some(writer);
        Ok(())
    }

    fn finish_segment(&mut self, segment: SegmentWriter) -> Result<(), ViewerError> {
        // Keep the file in the list even if closing it fails: it still plays up to its last
        // complete fragment and can be repaired later.
        let fallback = SegmentInfo {
            path: segment.path().to_owned(),
            bytes: segment.bytes_written(),
            duration: segment.duration(),
        };
        let (info, result) = match segment.finish() {
            Ok(info) => (info, Ok(())),
            Err(e) => (fallback, Err(e)),
        };
        self.done_bytes = self.done_bytes.saturating_add(info.bytes);
        self.done.push(info);
        result
    }

    /// Closes the open file. Returns the finished files.
    pub fn finish(mut self) -> (Vec<SegmentInfo>, Result<(), ViewerError>) {
        let result = match self.segment.take() {
            Some(s) => self.finish_segment(s),
            None => Ok(()),
        };
        (self.done, result)
    }
}
