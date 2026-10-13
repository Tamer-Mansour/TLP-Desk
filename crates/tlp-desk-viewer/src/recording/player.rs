//! Plays a recording back: demuxes it, decodes on a dedicated thread with the same OpenH264
//! decoder as live sessions, and publishes frames into a [`FrameSlot`] at the right time.
//!
//! Seeking jumps to the nearest keyframe at or before the target and decodes forward to the
//! target (recordings get a keyframe every few seconds, so that is a short burst).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tlp_desk_codec::VideoDecoder;

use super::demux::{demux, Demuxed};
use super::mp4::ticks_to_us;
use super::nal::sample_to_annex_b;
use crate::frame::{convert, DecodedFrame, FrameSlot, PixelFormat};
use crate::{RepaintFn, ViewerError};

/// Slowest playback speed.
pub const MIN_SPEED: f32 = 0.5;
/// Fastest playback speed.
pub const MAX_SPEED: f32 = 4.0;

/// What the player is doing.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlayerStatus {
    /// Time of the frame on screen.
    pub position: Duration,
    /// Length of the recording.
    pub duration: Duration,
    pub playing: bool,
    pub speed: f32,
    /// Picture size.
    pub width: u32,
    pub height: u32,
    /// Playback reached the end (Play starts over).
    pub ended: bool,
    /// The last error (unreadable file part, decoder failure), for the log / a notice.
    pub error: Option<String>,
}

enum Cmd {
    Play,
    Pause,
    Seek(Duration),
    Speed(f32),
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A recording opened for playback. Starts paused on the first frame. Dropping it stops the
/// decode thread.
pub struct Player {
    cmd: Option<mpsc::Sender<Cmd>>,
    status: Arc<Mutex<PlayerStatus>>,
    frames: Arc<FrameSlot>,
    keyframes: Vec<Duration>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl Player {
    /// Opens `path`. Frames come out in `format`; `repaint` runs after each new frame.
    /// Fails if the file is not a readable recording (an interrupted one is fine: it plays up
    /// to its last complete fragment).
    pub fn open(
        path: &Path,
        format: PixelFormat,
        repaint: Option<RepaintFn>,
    ) -> Result<Player, ViewerError> {
        let mut file = File::open(path).map_err(ViewerError::RecordingIo)?;
        let index = demux(&mut file)?;
        if index.samples.is_empty() {
            return Err(ViewerError::BadRecording("the recording holds no video"));
        }
        let decoder =
            tlp_desk_codec::new_decoder().map_err(|e| ViewerError::Decoder(e.to_string()))?;
        let to_time = |ticks: u64| Duration::from_micros(ticks_to_us(ticks, index.timescale));
        let keyframes = index
            .samples
            .iter()
            .filter(|s| s.sync)
            .map(|s| to_time(s.dts))
            .collect();
        let status = Arc::new(Mutex::new(PlayerStatus {
            duration: to_time(index.end_dts()),
            speed: 1.0,
            width: index.width,
            height: index.height,
            ..PlayerStatus::default()
        }));
        let frames = FrameSlot::new();
        let (tx, rx) = mpsc::channel();
        let mut engine = Engine {
            file,
            index,
            decoder,
            format,
            frames: frames.clone(),
            status: status.clone(),
            repaint,
            buf: Vec::new(),
            annex_b: Vec::new(),
            next: 0,
            playing: false,
            speed: 1.0,
            anchor: (Instant::now(), 0),
            broken: false,
        };
        let thread = std::thread::Builder::new()
            .name("tlp-playback".into())
            .spawn(move || engine.run(&rx))
            .map_err(ViewerError::RecordingIo)?;
        Ok(Player {
            cmd: Some(tx),
            status,
            frames,
            keyframes,
            thread: Some(thread),
        })
    }

    fn send(&self, cmd: Cmd) {
        if let Some(tx) = &self.cmd {
            let _ = tx.send(cmd);
        }
    }

    /// Plays from the current position (from the start once it ended).
    pub fn play(&self) {
        self.send(Cmd::Play);
    }

    pub fn pause(&self) {
        self.send(Cmd::Pause);
    }

    /// Shows the frame at `position` (clamped to the recording).
    pub fn seek(&self, position: Duration) {
        self.send(Cmd::Seek(position));
    }

    /// Playback speed, clamped to [`MIN_SPEED`]..=[`MAX_SPEED`].
    pub fn set_speed(&self, speed: f32) {
        self.send(Cmd::Speed(speed));
    }

    pub fn status(&self) -> PlayerStatus {
        lock(&self.status).clone()
    }

    /// The newest decoded frame, if one arrived since the last call.
    pub fn take_frame(&self) -> Option<DecodedFrame> {
        self.frames.take()
    }

    /// The frame mailbox (to recycle buffers after upload).
    pub fn frames(&self) -> Arc<FrameSlot> {
        self.frames.clone()
    }

    /// Times of the keyframes (exact seek points), in order.
    pub fn keyframes(&self) -> &[Duration] {
        &self.keyframes
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.cmd = None; // the thread sees the channel close and exits
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Engine {
    file: File,
    index: Demuxed,
    decoder: Box<dyn VideoDecoder>,
    format: PixelFormat,
    frames: Arc<FrameSlot>,
    status: Arc<Mutex<PlayerStatus>>,
    repaint: Option<RepaintFn>,
    buf: Vec<u8>,
    annex_b: Vec<u8>,
    /// Index of the next sample to decode.
    next: usize,
    playing: bool,
    speed: f32,
    /// Wall time and media time (ticks) that correspond to each other while playing.
    anchor: (Instant, u64),
    /// The decoder lost the reference chain; skip to the next keyframe.
    broken: bool,
}

impl Engine {
    fn run(&mut self, rx: &mpsc::Receiver<Cmd>) {
        self.seek_to(0);
        loop {
            let wait = self.time_to_next();
            let cmd = match wait {
                Some(w) => rx.recv_timeout(w),
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match cmd {
                Ok(first) => {
                    // While the user drags the timeline many seeks queue up: only the newest
                    // one matters, the others would just delay it.
                    let mut cmds = vec![first];
                    while let Ok(c) = rx.try_recv() {
                        cmds.push(c);
                    }
                    let last_seek = cmds.iter().rposition(|c| matches!(c, Cmd::Seek(_)));
                    for (i, c) in cmds.into_iter().enumerate() {
                        if matches!(c, Cmd::Seek(_)) && Some(i) != last_seek {
                            continue;
                        }
                        self.apply(c);
                    }
                }
                Err(RecvTimeoutError::Timeout) => self.step(),
                Err(RecvTimeoutError::Disconnected) => break,
            }
            let mut st = lock(&self.status);
            st.playing = self.playing;
            st.speed = self.speed;
            if self.playing {
                st.ended = false;
            }
        }
    }

    fn apply(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Play => {
                if self.next >= self.index.samples.len() {
                    self.seek_to(0);
                }
                self.playing = true;
                self.reanchor();
            }
            Cmd::Pause => self.playing = false,
            Cmd::Seek(t) => {
                let ticks = t
                    .as_micros()
                    .saturating_mul(u128::from(self.index.timescale))
                    / 1_000_000;
                self.seek_to(u64::try_from(ticks).unwrap_or(u64::MAX));
                self.reanchor();
            }
            Cmd::Speed(s) => {
                self.speed = if s.is_finite() {
                    s.clamp(MIN_SPEED, MAX_SPEED)
                } else {
                    1.0
                };
                self.reanchor();
            }
        }
    }

    /// Shows the next sample (it is due).
    fn step(&mut self) {
        let i = self.next;
        self.decode(i, true);
        self.next = i + 1;
        if self.next >= self.index.samples.len() {
            self.playing = false;
            lock(&self.status).ended = true;
        }
    }

    /// How long until the next sample is due; `None` when paused or at the end.
    fn time_to_next(&self) -> Option<Duration> {
        if !self.playing {
            return None;
        }
        let s = self.index.samples.get(self.next)?;
        let (wall, media) = self.anchor;
        let ahead_us = ticks_to_us(s.dts.saturating_sub(media), self.index.timescale);
        let due = wall + Duration::from_micros((ahead_us as f64 / f64::from(self.speed)) as u64);
        Some(due.saturating_duration_since(Instant::now()))
    }

    /// Makes "now" correspond to the next sample's time (after play, seek, speed change).
    fn reanchor(&mut self) {
        let media = self
            .index
            .samples
            .get(self.next)
            .map_or(self.index.end_dts(), |s| s.dts);
        self.anchor = (Instant::now(), media);
    }

    /// Decodes from the keyframe at or before `ticks` up to the sample shown at `ticks`.
    fn seek_to(&mut self, ticks: u64) {
        let samples = &self.index.samples;
        let target = samples
            .partition_point(|s| s.dts <= ticks)
            .saturating_sub(1);
        let start = samples
            .get(..=target)
            .and_then(|s| s.iter().rposition(|s| s.sync))
            .unwrap_or(0);
        match tlp_desk_codec::new_decoder() {
            Ok(d) => self.decoder = d,
            Err(e) => self.fail(format!("decoder restart failed: {e}")),
        }
        self.broken = false;
        for i in start..=target {
            self.decode(i, i == target);
        }
        self.next = target + 1;
        lock(&self.status).ended = false;
    }

    fn fail(&self, msg: String) {
        tracing::debug!(%msg, "playback problem");
        lock(&self.status).error = Some(msg);
    }

    /// Decodes sample `i`; publishes the picture if `show`.
    fn decode(&mut self, i: usize, show: bool) {
        let Some(s) = self.index.samples.get(i).copied() else {
            return;
        };
        if self.broken && !s.sync {
            return;
        }
        self.buf.resize(s.size as usize, 0);
        let read = self
            .file
            .seek(SeekFrom::Start(s.offset))
            .and_then(|_| self.file.read_exact(&mut self.buf));
        if let Err(e) = read {
            self.broken = true;
            return self.fail(format!("cannot read the recording: {e}"));
        }
        let params: Vec<&[u8]> = if s.sync {
            self.index
                .sps
                .iter()
                .chain(self.index.pps.iter())
                .map(Vec::as_slice)
                .collect()
        } else {
            Vec::new()
        };
        if let Err(e) = sample_to_annex_b(&self.buf, &params, &mut self.annex_b) {
            self.broken = true;
            return self.fail(e.to_string());
        }
        match self.decoder.decode(&self.annex_b) {
            Ok(Some(bgra)) => {
                self.broken = false;
                if show {
                    let (width, height) = (bgra.width, bgra.height);
                    let pixels = convert(&bgra, self.format, self.frames.take_spare());
                    let pts = ticks_to_us(s.dts, self.index.timescale);
                    self.frames.publish(DecodedFrame {
                        width,
                        height,
                        format: self.format,
                        pixels,
                        seq: i as u64,
                        host_timestamp_us: pts,
                    });
                    {
                        let mut st = lock(&self.status);
                        st.position = Duration::from_micros(pts);
                        st.width = width;
                        st.height = height;
                    }
                    if let Some(r) = &self.repaint {
                        r();
                    }
                }
                self.decoder.recycle(bgra);
            }
            Ok(None) => {}
            Err(e) => {
                self.broken = true;
                self.fail(format!("decode failed at sample {i}: {e}"));
            }
        }
    }
}
