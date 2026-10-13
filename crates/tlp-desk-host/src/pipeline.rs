//! Video pipeline: a capture thread and an encode thread joined by a one-slot mailbox.
//!
//! * The **capture thread** owns the capturer: it waits for new desktop frames, polls the
//!   cursor, handles live monitor switching and capture-surface loss, and publishes the newest
//!   frame into the mailbox. A frame that the encoder has not taken yet is *replaced* (and its
//!   buffer recycled), never queued, so a slow encoder drops frames instead of adding latency.
//! * The **encode thread** owns the encoder: paced at the target fps, it takes the newest
//!   frame, draws the cursor (scaled to the frame, saved/restored in place: no full-frame copy),
//!   encodes and hands the packet to `send_video`. Used buffers go back to the capture thread
//!   for reuse.
//!
//! Each capturer (re)open bumps a generation number carried with every frame, so the encoder
//! knows to rebuild (new size) or emit a keyframe (same size, new content source).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc as std_mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tlp_desk_capture::{BgraFrame, CaptureError, MonitorInfo, ScreenCapturer};
use tlp_desk_codec::{CodecError, EncoderConfig, VideoEncoder};
use tlp_desk_input::MonitorRect;
use tlp_desk_net::{Session, VideoSendStatus};
use tlp_desk_proto::{Codec, VideoFrame};
use tokio::sync::{mpsc, watch};

use crate::cursor::{CursorOverlay, CursorPatch};
use crate::{CapturerFactory, EncoderFactory};

/// Consecutive non-fatal capture errors tolerated before the session is ended.
const MAX_CAPTURE_ERRORS: u32 = 100;
/// Attempts to open the capturer when a session starts.
const INITIAL_OPEN_ATTEMPTS: u32 = 10;

pub(crate) struct PipelineCtx {
    pub capturer_factory: CapturerFactory,
    pub encoder_factory: EncoderFactory,
    /// Initial monitor index.
    pub monitor: u32,
    /// Monitor the viewer asked for (validated by the control loop).
    pub select: watch::Receiver<u32>,
    /// Monitor index actually being captured.
    pub active: watch::Sender<u32>,
    pub fps: u32,
    pub session: Arc<Session>,
    /// Set by the control loop on `Message::RequestKeyframe`.
    pub keyframe: Arc<AtomicBool>,
    /// Set when the session ends.
    pub stop: Arc<AtomicBool>,
    /// The captured monitor's rectangle, for the input injector.
    pub rect: watch::Sender<Option<MonitorRect>>,
    /// Fatal pipeline errors end the session.
    pub fatal: mpsc::UnboundedSender<String>,
}

/// A captured frame waiting for the encoder.
struct Captured {
    frame: BgraFrame,
    /// Capturer generation (bumped on every open / monitor switch / surface-loss reopen).
    gen: u64,
    /// Real size of the captured monitor (the frame may be smaller).
    monitor: (u32, u32),
    /// When the capture thread received it.
    at: Instant,
}

#[derive(Default)]
struct Slot {
    /// Newest frame the encoder has not taken yet.
    frame: Option<Captured>,
    /// New cursor state (position/shape/visibility changed) not yet seen by the encoder.
    /// The instant is `None` for a reset (new capturer) that alone must not trigger an encode.
    cursor: Option<(CursorOverlay, Option<Instant>)>,
    /// Set by either side to make the other one exit.
    closed: bool,
}

#[derive(Default)]
struct Mailbox {
    slot: Mutex<Slot>,
    cv: Condvar,
}

impl Mailbox {
    fn lock(&self) -> std::sync::MutexGuard<'_, Slot> {
        // A poisoned lock only means the other thread panicked; the data is still consistent
        // enough to shut down.
        self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Publishes a frame; returns the replaced (never encoded) frame for recycling.
    fn put_frame(&self, c: Captured) -> Option<BgraFrame> {
        let old = self.lock().frame.replace(c).map(|o| o.frame);
        self.cv.notify_one();
        old
    }

    fn put_cursor(&self, overlay: CursorOverlay, changed: bool) {
        self.lock().cursor = Some((overlay, changed.then(Instant::now)));
        self.cv.notify_one();
    }

    fn close(&self) {
        self.lock().closed = true;
        self.cv.notify_all();
    }

    fn is_closed(&self) -> bool {
        self.lock().closed
    }
}

pub(crate) fn run(ctx: PipelineCtx) {
    if let Err(e) = tlp_desk_input::ensure_dpi_aware() {
        tracing::warn!(%e, "could not make the host process per-monitor DPI aware");
    }
    let ctx = Arc::new(ctx);
    let mailbox = Arc::new(Mailbox::default());
    let (recycle_tx, recycle_rx) = std_mpsc::channel::<BgraFrame>();

    let encoder = {
        let (ctx, mailbox) = (Arc::clone(&ctx), Arc::clone(&mailbox));
        std::thread::Builder::new()
            .name(
                std::thread::current()
                    .name()
                    .map_or_else(|| "tlp-host-encode".into(), |n| format!("{n}-encode")),
            )
            .spawn(move || {
                let result = encode_loop(&ctx, &mailbox, &recycle_tx);
                mailbox.close();
                result
            })
    };
    let result = match encoder {
        Ok(handle) => {
            let r = capture_loop(&ctx, &mailbox, &recycle_rx);
            mailbox.close();
            let enc_result = handle
                .join()
                .unwrap_or_else(|_| Err("encode thread panicked".into()));
            r.and(enc_result)
        }
        Err(e) => Err(format!("cannot spawn encode thread: {e}")),
    };
    if let Err(msg) = result {
        if !ctx.stop.load(Ordering::Acquire) {
            tracing::error!(%msg, "video pipeline failed");
            let _ = ctx.fatal.send(msg);
        }
    }
    tracing::debug!("video pipeline stopped");
}

fn rect_of(m: &MonitorInfo) -> MonitorRect {
    MonitorRect::new(m.x, m.y, m.width, m.height)
}

fn stopped(ctx: &PipelineCtx) -> bool {
    ctx.stop.load(Ordering::Acquire)
}

fn frame_interval(fps: u32) -> Duration {
    Duration::from_secs_f64(1.0 / f64::from(fps.max(1)))
}

/// Opens the capturer; `attempts == None` retries until the session stops.
fn open(
    ctx: &PipelineCtx,
    mailbox: &Mailbox,
    index: u32,
    attempts: Option<u32>,
) -> Result<Option<Box<dyn ScreenCapturer>>, String> {
    let mut n = 0u32;
    loop {
        if stopped(ctx) || mailbox.is_closed() {
            return Ok(None);
        }
        match (ctx.capturer_factory)(index) {
            Ok(c) => {
                let m = c.monitor();
                tracing::info!(
                    monitor = index,
                    width = m.width,
                    height = m.height,
                    frame = ?c.frame_size(),
                    backend = ?c.backend(),
                    "capturer opened"
                );
                return Ok(Some(c));
            }
            Err(e) => {
                n += 1;
                if attempts.is_some_and(|max| n >= max) {
                    return Err(format!("cannot open screen capture: {e}"));
                }
                if n == 1 || n % 20 == 0 {
                    tracing::warn!(%e, attempt = n, "opening screen capture failed; retrying");
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

fn capture_loop(
    ctx: &PipelineCtx,
    mailbox: &Mailbox,
    recycle: &std_mpsc::Receiver<BgraFrame>,
) -> Result<(), String> {
    let mut index = ctx.monitor;
    let mut select = ctx.select.clone();
    select.mark_unchanged();
    let Some(mut cap) = open(ctx, mailbox, index, Some(INITIAL_OPEN_ATTEMPTS))? else {
        return Ok(());
    };
    let mut gen = 0u64;
    ctx.rect.send_replace(Some(rect_of(cap.monitor())));
    ctx.active.send_replace(index);
    let mut cursor = CursorOverlay::new();
    let interval = frame_interval(ctx.fps);
    let mut capture_errors = 0u32;

    while !stopped(ctx) && !mailbox.is_closed() {
        if select.has_changed().unwrap_or(false) {
            let want = *select.borrow_and_update();
            if want != index {
                // Switch live: one attempt; on failure keep streaming the current monitor.
                match (ctx.capturer_factory)(want) {
                    Ok(c) => {
                        tracing::info!(
                            from = index,
                            to = want,
                            width = c.monitor().width,
                            height = c.monitor().height,
                            frame = ?c.frame_size(),
                            "switched captured monitor"
                        );
                        drop(cap);
                        cap = c;
                        index = want;
                        gen += 1;
                        // A frame of the old monitor must not be encoded any more.
                        drop(mailbox.lock().frame.take());
                        cursor = CursorOverlay::new();
                        mailbox.put_cursor(cursor.clone(), false);
                        ctx.rect.send_replace(Some(rect_of(cap.monitor())));
                        ctx.active.send_replace(index);
                    }
                    Err(e) => {
                        tracing::warn!(%e, monitor = want, "cannot switch to monitor; keeping the current one");
                        ctx.active.send_replace(index);
                    }
                }
            }
        }
        while let Ok(buf) = recycle.try_recv() {
            cap.recycle(buf);
        }

        match cap.next_frame(interval) {
            Ok(Some(frame)) => {
                capture_errors = 0;
                let m = cap.monitor();
                let replaced = mailbox.put_frame(Captured {
                    frame,
                    gen,
                    monitor: (m.width, m.height),
                    at: Instant::now(),
                });
                if let Some(old) = replaced {
                    cap.recycle(old);
                }
            }
            Ok(None) => {}
            Err(CaptureError::SurfaceLost) => {
                tracing::info!("capture surface lost (desktop switch or mode change); reopening");
                drop(cap);
                drop(mailbox.lock().frame.take());
                let Some(c) = open(ctx, mailbox, index, None)? else {
                    return Ok(());
                };
                cap = c;
                gen += 1;
                cursor = CursorOverlay::new();
                mailbox.put_cursor(cursor.clone(), false);
                ctx.rect.send_replace(Some(rect_of(cap.monitor())));
                continue;
            }
            Err(e) => {
                capture_errors += 1;
                if capture_errors >= MAX_CAPTURE_ERRORS {
                    return Err(format!("screen capture keeps failing: {e}"));
                }
                tracing::debug!(%e, "capture error");
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
        }

        match cap.cursor() {
            Ok(state) => {
                if cursor.update(state) {
                    mailbox.put_cursor(cursor.clone(), true);
                }
            }
            Err(e) => tracing::trace!(%e, "cursor poll failed"),
        }
    }
    Ok(())
}

fn new_encoder(ctx: &PipelineCtx, w: u32, h: u32) -> Result<Box<dyn VideoEncoder>, String> {
    let cfg = EncoderConfig::for_screen(w, h, ctx.fps);
    tracing::info!(
        width = w,
        height = h,
        fps = cfg.fps,
        kbps = cfg.bitrate_kbps,
        "encoder created"
    );
    let mut enc = (ctx.encoder_factory)(cfg).map_err(|e| format!("cannot create encoder: {e}"))?;
    enc.request_keyframe();
    Ok(enc)
}

/// What the encode thread works on.
struct EncodeState {
    enc: Option<Box<dyn VideoEncoder>>,
    /// Size the encoder was built for.
    size: (u32, u32),
    gen: Option<u64>,
    /// Clean (cursor-free) newest frame and its monitor size.
    last: Option<(BgraFrame, (u32, u32))>,
    cursor: CursorOverlay,
    patch: CursorPatch,
    /// When the oldest not-yet-encoded change happened (None = nothing to send).
    pending_since: Option<Instant>,
}

fn encode_loop(
    ctx: &PipelineCtx,
    mailbox: &Mailbox,
    recycle: &std_mpsc::Sender<BgraFrame>,
) -> Result<(), String> {
    let interval = frame_interval(ctx.fps);
    let start = Instant::now();
    let mut seq = 0u64;
    let mut next_encode = Instant::now();
    let mut st = EncodeState {
        enc: None,
        size: (0, 0),
        gen: None,
        last: None,
        cursor: CursorOverlay::new(),
        patch: CursorPatch::default(),
        pending_since: None,
    };

    loop {
        // Wait (without holding the lock while encoding) until there is something to send and
        // the frame interval has elapsed.
        {
            let mut slot = mailbox.lock();
            loop {
                if slot.closed || stopped(ctx) {
                    return Ok(());
                }
                if let Some(c) = slot.frame.take() {
                    st.pending_since.get_or_insert(c.at);
                    if st.gen != Some(c.gen) {
                        // New capturer: the old frame is stale and the stream must restart
                        // with a keyframe (the encoder is rebuilt below if the size changed).
                        st.gen = Some(c.gen);
                        if let Some(enc) = st.enc.as_mut() {
                            enc.request_keyframe();
                        }
                    }
                    if let Some((old, _)) = st.last.replace((c.frame, c.monitor)) {
                        let _ = recycle.send(old);
                    }
                }
                if let Some((overlay, at)) = slot.cursor.take() {
                    st.cursor = overlay;
                    if let Some(at) = at {
                        st.pending_since.get_or_insert(at);
                    }
                }
                if ctx.keyframe.swap(false, Ordering::AcqRel) {
                    tracing::debug!("viewer requested a keyframe");
                    if let Some(enc) = st.enc.as_mut() {
                        enc.request_keyframe();
                    }
                    st.pending_since.get_or_insert_with(Instant::now);
                }
                let now = Instant::now();
                let wait = match st.pending_since {
                    Some(_) if st.last.is_some() && now >= next_encode => break,
                    Some(_) if st.last.is_some() => next_encode - now,
                    // Nothing to send: wake up now and then for keyframe requests / stop.
                    _ => interval,
                };
                slot = mailbox
                    .cv
                    .wait_timeout(slot, wait)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        }

        let now = Instant::now();
        next_encode += interval;
        if next_encode < now {
            next_encode = now;
        }
        let Some(since) = st.pending_since.take() else {
            continue;
        };
        let Some((frame, monitor)) = st.last.as_mut() else {
            continue;
        };
        let frame_size = (frame.width, frame.height);
        if st.enc.is_none() || st.size != frame_size {
            if st.enc.is_some() {
                tracing::info!(old = ?st.size, new = ?frame_size, "frame size changed; recreating encoder");
            }
            st.enc = Some(new_encoder(ctx, frame_size.0, frame_size.1)?);
            st.size = frame_size;
        }
        let Some(enc) = st.enc.as_mut() else {
            continue;
        };

        // The encoder uses the input timestamp for rate control: keep it monotonic even when
        // the same captured frame is re-encoded for a cursor move.
        frame.timestamp_us = now.saturating_duration_since(start).as_micros() as u64;
        let drawn = st.cursor.draw_scaled(frame, *monitor, &mut st.patch);
        let encoded = enc.encode(frame);
        if drawn {
            st.patch.restore(frame);
        }
        // On the wire: when the oldest change in this frame happened (for latency estimates).
        let timestamp_us = since.saturating_duration_since(start).as_micros() as u64;
        match encoded {
            Ok(Some(ef)) => {
                let vf = VideoFrame {
                    seq,
                    timestamp_us,
                    width: frame_size.0,
                    height: frame_size.1,
                    codec: Codec::H264,
                    keyframe: ef.keyframe,
                    data: ef.data,
                };
                seq += 1;
                match ctx.session.send_video(vf) {
                    Ok(VideoSendStatus::Sent) => {}
                    Ok(VideoSendStatus::Dropped) => {
                        // The viewer is behind: the next frame must be a keyframe, and send it
                        // even if the screen stays static.
                        enc.request_keyframe();
                        st.pending_since.get_or_insert(now);
                    }
                    Err(e) => return Err(format!("cannot send video: {e}")),
                }
            }
            Ok(None) => {}
            Err(CodecError::SizeMismatch { .. }) => {
                st.enc = Some(new_encoder(ctx, frame_size.0, frame_size.1)?);
                st.size = frame_size;
                st.pending_since.get_or_insert(now);
            }
            Err(e) => return Err(format!("encoder failed: {e}")),
        }
    }
}
