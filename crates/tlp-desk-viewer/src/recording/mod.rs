//! Session recording to a local file (viewer side) and playback.
//!
//! The recorder stores the H.264 pictures exactly as they arrive from the host (no
//! re-encoding, so it costs almost no CPU) in a standard **fragmented MP4** file that Windows
//! Media Player, the Windows "Media Player"/"Films & TV" apps, VLC and browsers play. The
//! container writer is our own small one (`mp4.rs`, documented box by box) rather than a
//! dependency: we only need one video track in fragments, and writing it ourselves keeps the
//! format fully under our control and the licence question trivial.
//!
//! * **Start/stop:** [`Recording::start`] taps a [`ViewerSession`]'s received video and writes
//!   on its own thread; [`Recording::stop`] closes the file. A first keyframe is requested at
//!   once, and another every few seconds so the file stays seekable.
//! * **Crash-safe:** the file is written as one-second fragments, each appended in one write.
//!   After a crash everything but the last second plays; [`recover_file`] finishes such a
//!   file (cuts the damaged tail, writes the index and the duration).
//! * **Picture size changes** (the host switched monitors): the recording continues in a new
//!   file named like the first with ` (2)`, ` (3)`, ...; [`RecordingSummary::files`] lists
//!   them all.
//! * **Disk space:** recording refuses to start below [`RecordingConfig::min_free_bytes`] free
//!   and stops (closing the file properly) when half of that is left.
//! * **Names:** `%USERPROFILE%\Videos\TLP-Desk\TLP-Desk <peer> <date time>.mp4` (see
//!   [`default_folder`]); Arabic peer names are kept exactly.
//! * **Privacy:** recording happens only on the viewer, from video the host already sends;
//!   the host is not notified (the protocol has no message for it). The viewer shows a red
//!   recording indicator the whole time.
//! * **Audio:** not recorded yet. When the audio lane's Opus stream is available, a second
//!   track can be added: feed its packets to the recorder thread like [`TapFrame`]s and write
//!   an `Opus` track next to the video one (see the note in `mp4.rs`).
//!
//! Playback: [`Player`] opens a recording, decodes it with the same OpenH264 decoder as live
//! sessions on its own thread, and supports play/pause, seeking (from the nearest keyframe)
//! and 0.5x-4x speed.

mod demux;
mod files;
mod mp4;
mod nal;
mod player;
mod recorder;
mod segment;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub use files::{
    default_folder, delete_recording, list_recordings, RecordingEntry, FILE_EXTENSION, FILE_PREFIX,
};
pub use player::{Player, PlayerStatus, MAX_SPEED, MIN_SPEED};
pub use segment::{recover_file, Recovery};

use crate::{ViewerError, ViewerSession};
use recorder::Core;

/// One received video frame as handed to a [`VideoTap`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TapFrame {
    /// Host frame sequence number (a jump means frames were lost).
    pub seq: u64,
    /// Host capture time in microseconds since the host session started.
    pub timestamp_us: u64,
    /// Whether the host marked it as a keyframe.
    pub keyframe: bool,
    /// The H.264 Annex-B access unit.
    pub data: Vec<u8>,
}

/// Receives every video frame a session gets from the host, before decoding. Called on the
/// session's network task: it must return quickly and never block.
pub type VideoTap = Arc<dyn Fn(&tlp_desk_proto::VideoFrame) + Send + Sync>;

/// Asks the host for a keyframe.
pub type KeyframeFn = Arc<dyn Fn() + Send + Sync>;

/// Called (from the recorder thread) when a recording stopped by itself because of an error
/// such as a full disk. Not called for [`Recording::stop`] or when the handle is dropped.
pub type StoppedFn = Arc<dyn Fn(&RecordingSummary) + Send + Sync>;

/// Frames buffered between the network task and the writer thread. When the disk cannot keep
/// up, newer frames are dropped (and the recording resumes at the next keyframe) instead of
/// the session slowing down.
const QUEUE: usize = 120;
/// How often the writer thread wakes up without frames (static screen).
const TICK: Duration = Duration::from_millis(500);

/// Recording settings.
#[derive(Clone)]
pub struct RecordingConfig {
    /// Folder for the files (created if missing).
    pub folder: PathBuf,
    /// Remote device name, used in the file name (sanitized; Arabic kept).
    pub peer_name: String,
    /// Refuse to start with less free space than this; stop at half of it.
    pub min_free_bytes: u64,
    /// Ask the host for a keyframe this often (seek granularity of the file).
    pub keyframe_interval: Duration,
    /// See [`StoppedFn`].
    pub on_stopped: Option<StoppedFn>,
}

impl RecordingConfig {
    /// Settings for recording `peer_name` into `folder` with the default limits (256 MiB
    /// free space margin, a keyframe every 4 seconds).
    pub fn new(folder: PathBuf, peer_name: impl Into<String>) -> Self {
        Self {
            folder,
            peer_name: peer_name.into(),
            min_free_bytes: 256 << 20,
            keyframe_interval: Duration::from_secs(4),
            on_stopped: None,
        }
    }
}

impl std::fmt::Debug for RecordingConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordingConfig")
            .field("folder", &self.folder)
            .field("peer_name", &self.peer_name)
            .field("min_free_bytes", &self.min_free_bytes)
            .field("keyframe_interval", &self.keyframe_interval)
            .finish_non_exhaustive()
    }
}

/// Why a recording stopped early.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingFailure {
    /// Less free disk space than the safety margin.
    DiskFull { available: u64 },
    /// Writing the file failed (details for the log).
    Write(String),
}

impl RecordingFailure {
    fn from_error(e: &ViewerError) -> Self {
        match e {
            ViewerError::DiskFull { available, .. } => Self::DiskFull {
                available: *available,
            },
            ViewerError::RecordingIo(io) if io.kind() == std::io::ErrorKind::StorageFull => {
                Self::DiskFull { available: 0 }
            }
            other => Self::Write(other.to_string()),
        }
    }
}

/// Live state of a recording.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecordingStatus {
    /// Media time recorded so far.
    pub duration: Duration,
    /// Bytes written so far.
    pub bytes: u64,
    /// Files so far; the last one is being written.
    pub files: Vec<PathBuf>,
    /// The recorder stopped by itself (see [`RecordingSummary::failure`]).
    pub stopped: bool,
}

/// The result of a finished recording.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecordingSummary {
    /// Every file written, in order (more than one if the picture size changed). Empty if no
    /// keyframe ever arrived.
    pub files: Vec<PathBuf>,
    pub duration: Duration,
    pub bytes: u64,
    /// Set when the recording ended because of an error.
    pub failure: Option<RecordingFailure>,
}

enum Msg {
    Frame(TapFrame),
    Stop,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A running recording. Dropping it stops the recording and closes the file.
pub struct Recording {
    tx: SyncSender<Msg>,
    thread: Option<JoinHandle<RecordingSummary>>,
    status: Arc<Mutex<RecordingStatus>>,
    stop_requested: Arc<AtomicBool>,
    session: Option<Weak<ViewerSession>>,
}

impl std::fmt::Debug for Recording {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recording")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl Recording {
    /// Starts recording what `session` receives. Fails if the folder cannot be created or
    /// the disk is (nearly) full. The file appears with the first keyframe, requested here.
    pub fn start(
        session: &Arc<ViewerSession>,
        cfg: RecordingConfig,
    ) -> Result<Recording, ViewerError> {
        let weak = Arc::downgrade(session);
        let requester = weak.clone();
        let request: KeyframeFn = Arc::new(move || {
            if let Some(s) = requester.upgrade() {
                s.request_keyframe();
            }
        });
        let (mut rec, tap) = Self::start_with(cfg, request)?;
        session.set_video_tap(Some(tap));
        session.request_keyframe();
        rec.session = Some(weak);
        Ok(rec)
    }

    /// Starts a recording fed by the returned [`VideoTap`] (for a session, use
    /// [`Recording::start`]). `request_keyframe` is called when the recorder needs one.
    pub fn start_with(
        cfg: RecordingConfig,
        request_keyframe: KeyframeFn,
    ) -> Result<(Recording, VideoTap), ViewerError> {
        let space: recorder::SpaceFn = Box::new(|p| fs4::available_space(p));
        Self::start_inner(cfg, request_keyframe, space)
    }

    fn start_inner(
        cfg: RecordingConfig,
        request_keyframe: KeyframeFn,
        free_space: recorder::SpaceFn,
    ) -> Result<(Recording, VideoTap), ViewerError> {
        let core = Core::new(
            &cfg.folder,
            &cfg.peer_name,
            cfg.min_free_bytes,
            cfg.keyframe_interval,
            request_keyframe,
            free_space,
            Instant::now(),
        )?;
        let (tx, rx) = mpsc::sync_channel::<Msg>(QUEUE);
        let status = Arc::new(Mutex::new(RecordingStatus::default()));
        let stop_requested = Arc::new(AtomicBool::new(false));
        let thread = {
            let status = status.clone();
            let stop_requested = stop_requested.clone();
            let on_stopped = cfg.on_stopped.clone();
            std::thread::Builder::new()
                .name("tlp-viewer-record".into())
                .spawn(move || {
                    let summary = run(core, &rx, &status);
                    if !stop_requested.load(Ordering::Acquire) {
                        if let Some(f) = &on_stopped {
                            f(&summary);
                        }
                    }
                    summary
                })
                .map_err(ViewerError::RecordingIo)?
        };
        let tap_tx = tx.clone();
        let tap: VideoTap = Arc::new(move |f: &tlp_desk_proto::VideoFrame| {
            let frame = TapFrame {
                seq: f.seq,
                timestamp_us: f.timestamp_us,
                keyframe: f.keyframe,
                data: f.data.clone(),
            };
            match tap_tx.try_send(Msg::Frame(frame)) {
                Ok(()) | Err(TrySendError::Disconnected(_)) => {}
                // The skipped sequence number makes the recorder wait for a keyframe.
                Err(TrySendError::Full(_)) => tracing::debug!("recorder busy; frame dropped"),
            }
        });
        tracing::info!(folder = %cfg.folder.display(), "recording started");
        Ok((
            Recording {
                tx,
                thread: Some(thread),
                status,
                stop_requested,
                session: None,
            },
            tap,
        ))
    }

    /// Current progress.
    pub fn status(&self) -> RecordingStatus {
        lock(&self.status).clone()
    }

    /// Stops recording and closes the file (waits for the writer, normally milliseconds).
    pub fn stop(mut self) -> RecordingSummary {
        self.shutdown()
    }

    fn shutdown(&mut self) -> RecordingSummary {
        if let Some(s) = self.session.take().and_then(|w| w.upgrade()) {
            s.set_video_tap(None);
        }
        self.stop_requested.store(true, Ordering::Release);
        let _ = self.tx.send(Msg::Stop);
        let Some(thread) = self.thread.take() else {
            return RecordingSummary::default();
        };
        let summary = thread.join().unwrap_or_else(|_| RecordingSummary {
            failure: Some(RecordingFailure::Write("recorder thread panicked".into())),
            ..RecordingSummary::default()
        });
        tracing::info!(files = summary.files.len(), duration = ?summary.duration, "recording stopped");
        summary
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.shutdown();
        }
    }
}

/// The writer thread: feeds frames to the core until stopped, disconnected or failed.
fn run(
    mut core: Core,
    rx: &mpsc::Receiver<Msg>,
    status: &Mutex<RecordingStatus>,
) -> RecordingSummary {
    let mut failure = None;
    loop {
        let result = match rx.recv_timeout(TICK) {
            Ok(Msg::Frame(f)) => core.on_frame(f, Instant::now()),
            Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => core.on_tick(Instant::now()),
        };
        if let Err(e) = result {
            tracing::warn!(%e, "recording stopped by an error");
            failure = Some(RecordingFailure::from_error(&e));
            break;
        }
        let mut st = lock(status);
        st.duration = core.duration();
        st.bytes = core.bytes();
        let files = core.files();
        if st.files != files {
            st.files = files;
        }
    }
    let (done, result) = core.finish();
    if let Err(e) = result {
        tracing::warn!(%e, "could not close the recording file properly");
        failure.get_or_insert(RecordingFailure::from_error(&e));
    }
    let summary = RecordingSummary {
        duration: done.iter().map(|s| s.duration).sum(),
        bytes: done.iter().map(|s| s.bytes).sum(),
        files: done.into_iter().map(|s| s.path).collect(),
        failure,
    };
    let mut st = lock(status);
    st.stopped = true;
    st.files = summary.files.clone();
    st.duration = summary.duration;
    st.bytes = summary.bytes;
    summary
}

#[cfg(test)]
mod tests;
