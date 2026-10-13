//! Video pipeline: a capture thread and an encode thread joined by a one-slot mailbox.
//!
//! * The **capture thread** owns the capturer: it waits for new desktop frames, polls the
//!   cursor, handles live monitor switching, capture-surface loss and size-cap changes (quality
//!   preset, adaptive resolution step), and publishes the newest frame into the mailbox. A
//!   frame that the encoder has not taken yet is *replaced* (and its buffer recycled), never
//!   queued, so a slow encoder drops frames instead of adding latency.
//! * The **encode thread** owns the encoder: paced at the adaptive frame rate, it takes the
//!   newest frame, draws the cursor (scaled to the frame, saved/restored in place: no
//!   full-frame copy), encodes and hands the packet to `send_video`. Used buffers go back to
//!   the capture thread for reuse. About four times per second it ticks the adaptive
//!   controller (`adapt::EncodeAdapter`), which retunes the bitrate and picks the frame rate
//!   and resolution step.
//!
//! Each capturer (re)open bumps a generation number carried with every frame, so the encoder
//! knows to rebuild (new size) or emit a keyframe (same size, new content source).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc as std_mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tlp_desk_capture::{BgraFrame, CaptureError, DirtyTracker, MonitorInfo, ALL_MONITORS};
use tlp_desk_codec::{CodecError, VideoEncoder};
use tlp_desk_input::MonitorRect;
use tlp_desk_net::{Session, VideoSendStatus};
use tlp_desk_proto::{Codec, VideoFrame};
use tokio::sync::{mpsc, watch};

use crate::adapt::{open_source, wanted_cap, AdaptShared, EncodeAdapter, QualityTarget, Source};
use crate::cursor::{CursorOverlay, CursorPatch};
use crate::{CapturerFactory, EncoderFactory, MonitorLister};

/// Consecutive non-fatal capture errors tolerated before the session is ended.
const MAX_CAPTURE_ERRORS: u32 = 100;
/// Attempts to open the capturer when a session starts.
const INITIAL_OPEN_ATTEMPTS: u32 = 10;
/// How often the "all monitors" layout is compared with the monitor list (hot-plug).
const LAYOUT_POLL: Duration = Duration::from_secs(2);

pub(crate) struct PipelineCtx {
    pub capturer_factory: CapturerFactory,
    pub encoder_factory: EncoderFactory,
    pub monitor_lister: MonitorLister,
    /// Initial monitor index.
    pub monitor: u32,
    /// Monitor the viewer asked for (validated by the control loop; `ALL_MONITORS` = all).
    pub select: watch::Receiver<u32>,
    /// Monitor index actually being captured.
    pub active: watch::Sender<u32>,
    /// Quality target (preset) set by the viewer.
    pub quality: watch::Receiver<QualityTarget>,
    /// Viewer feedback and the session clock.
    pub adapt: Arc<AdaptShared>,
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

struct Mailbox {
    slot: Mutex<Slot>,
    cv: Condvar,
    /// Adaptive resolution step (per mille of the quality cap), set by the encode thread.
    scale: Arc<AtomicU32>,
}

impl Default for Mailbox {
    fn default() -> Self {
        Self {
            slot: Mutex::default(),
            cv: Condvar::new(),
            scale: Arc::new(AtomicU32::new(1000)),
        }
    }
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

/// Opens the capture source; `attempts == None` retries until the session stops.
fn open(
    ctx: &PipelineCtx,
    mailbox: &Mailbox,
    index: u32,
    attempts: Option<u32>,
) -> Result<Option<Source>, String> {
    let mut n = 0u32;
    loop {
        if stopped(ctx) || mailbox.is_closed() {
            return Ok(None);
        }
        match open_now(ctx, mailbox, index) {
            Ok(s) => return Ok(Some(s)),
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

/// One attempt to open `index` with the current quality cap and resolution step.
fn open_now(ctx: &PipelineCtx, mailbox: &Mailbox, index: u32) -> Result<Source, CaptureError> {
    let quality_cap = ctx.quality.borrow().max_size;
    let scale = mailbox.scale.load(Ordering::Acquire);
    let s = open_source(
        &ctx.capturer_factory,
        &ctx.monitor_lister,
        index,
        quality_cap,
        scale,
    )?;
    let m = s.cap.monitor();
    tracing::info!(
        monitor = index,
        width = m.width,
        height = m.height,
        cap = ?s.max_size,
        frame = ?s.cap.frame_size(),
        backend = ?s.cap.backend(),
        "capturer opened"
    );
    Ok(s)
}

/// What the capture thread is reading from.
struct CaptureState {
    src: Source,
    index: u32,
    gen: u64,
    cursor: CursorOverlay,
    /// A cap we failed to reopen with; not retried until the wanted cap changes again.
    failed_cap: Option<(u32, u32)>,
    /// Static-screen detection: frames identical to the previous one are not published.
    dirty: DirtyTracker,
    /// Identical frames skipped (for the log).
    skipped: u64,
    /// Last time the "all monitors" layout was compared with the monitor list.
    layout_checked: Instant,
}

impl CaptureState {
    /// Switches to a freshly opened source: new generation, stale frame and cursor dropped.
    fn replace(&mut self, ctx: &PipelineCtx, mailbox: &Mailbox, src: Source, index: u32) {
        self.src = src;
        self.index = index;
        self.gen += 1;
        self.failed_cap = None;
        self.dirty.reset();
        // A frame of the old source must not be encoded any more.
        drop(mailbox.lock().frame.take());
        self.cursor = CursorOverlay::new();
        mailbox.put_cursor(self.cursor.clone(), false);
        ctx.rect.send_replace(Some(rect_of(self.src.cap.monitor())));
        ctx.active.send_replace(index);
    }
}

fn capture_loop(
    ctx: &PipelineCtx,
    mailbox: &Mailbox,
    recycle: &std_mpsc::Receiver<BgraFrame>,
) -> Result<(), String> {
    let mut select = ctx.select.clone();
    select.mark_unchanged();
    let Some(src) = open(ctx, mailbox, ctx.monitor, Some(INITIAL_OPEN_ATTEMPTS))? else {
        return Ok(());
    };
    ctx.rect.send_replace(Some(rect_of(src.cap.monitor())));
    ctx.active.send_replace(ctx.monitor);
    let mut st = CaptureState {
        src,
        index: ctx.monitor,
        gen: 0,
        cursor: CursorOverlay::new(),
        failed_cap: None,
        dirty: DirtyTracker::default(),
        skipped: 0,
        layout_checked: Instant::now(),
    };
    let mut capture_errors = 0u32;

    while !stopped(ctx) && !mailbox.is_closed() {
        if select.has_changed().unwrap_or(false) {
            let want = *select.borrow_and_update();
            if want != st.index {
                // Switch live: one attempt; on failure keep streaming the current monitor.
                match open_now(ctx, mailbox, want) {
                    Ok(s) => {
                        tracing::info!(from = st.index, to = want, "switched captured monitor");
                        st.replace(ctx, mailbox, s, want);
                    }
                    Err(e) => {
                        tracing::warn!(%e, monitor = want, "cannot switch to monitor; keeping the current one");
                        ctx.active.send_replace(st.index);
                    }
                }
            }
        }
        reopen_if_cap_changed(ctx, mailbox, &mut st);
        recompose_if_layout_changed(ctx, mailbox, &mut st);
        while let Ok(buf) = recycle.try_recv() {
            st.src.cap.recycle(buf);
        }

        let interval = frame_interval(ctx.quality.borrow().fps);
        match st.src.cap.next_frame(interval) {
            Ok(Some(frame)) if !st.dirty.update(&frame).is_changed() => {
                // Nothing on screen changed (the OS reports frames for other reasons too):
                // nothing to encode or send. Cursor moves still reach the encoder below.
                capture_errors = 0;
                st.skipped += 1;
                if st.skipped % 600 == 0 {
                    tracing::debug!(skipped = st.skipped, "identical frames not encoded");
                }
                st.src.cap.recycle(frame);
            }
            Ok(Some(frame)) => {
                capture_errors = 0;
                let m = st.src.cap.monitor();
                let replaced = mailbox.put_frame(Captured {
                    frame,
                    gen: st.gen,
                    monitor: (m.width, m.height),
                    at: Instant::now(),
                });
                if let Some(old) = replaced {
                    st.src.cap.recycle(old);
                }
            }
            Ok(None) => {}
            Err(CaptureError::SurfaceLost) => {
                tracing::info!("capture surface lost (desktop switch or mode change); reopening");
                drop(mailbox.lock().frame.take());
                let Some(s) = open(ctx, mailbox, st.index, None)? else {
                    return Ok(());
                };
                let index = st.index;
                st.replace(ctx, mailbox, s, index);
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

        match st.src.cap.cursor() {
            Ok(state) => {
                if st.cursor.update(state) {
                    mailbox.put_cursor(st.cursor.clone(), true);
                }
            }
            Err(e) => tracing::trace!(%e, "cursor poll failed"),
        }
    }
    Ok(())
}

/// Reopens the current source when the quality preset or the adaptive resolution step asks
/// for another size cap. One attempt per wanted cap; on failure the current source stays.
fn reopen_if_cap_changed(ctx: &PipelineCtx, mailbox: &Mailbox, st: &mut CaptureState) {
    let quality_cap = ctx.quality.borrow().max_size;
    let scale = mailbox.scale.load(Ordering::Acquire);
    let want = wanted_cap(st.src.cap.monitor(), quality_cap, scale);
    if want == st.src.max_size || st.failed_cap == Some(want) {
        return;
    }
    tracing::info!(from = ?st.src.max_size, to = ?want, scale, "capture size cap changed; reopening");
    match open_now(ctx, mailbox, st.index) {
        Ok(s) => {
            let index = st.index;
            st.replace(ctx, mailbox, s, index);
        }
        Err(e) => {
            tracing::warn!(%e, cap = ?want, "cannot reopen capture with the new size cap; keeping the current one");
            st.failed_cap = Some(want);
        }
    }
}

/// While all monitors are shown, re-reads the monitor list now and then and recomposes when a
/// monitor was added, removed, moved or resized.
fn recompose_if_layout_changed(ctx: &PipelineCtx, mailbox: &Mailbox, st: &mut CaptureState) {
    let Some(layout) = st.src.monitors.as_ref() else {
        return;
    };
    if st.layout_checked.elapsed() < LAYOUT_POLL {
        return;
    }
    st.layout_checked = Instant::now();
    match (ctx.monitor_lister)() {
        Ok(list) if !list.is_empty() && list != *layout => {
            tracing::info!(
                monitors = list.len(),
                "monitor layout changed; recomposing all monitors"
            );
            match open_now(ctx, mailbox, ALL_MONITORS) {
                Ok(s) => st.replace(ctx, mailbox, s, ALL_MONITORS),
                Err(e) => {
                    tracing::warn!(%e, "cannot recompose all monitors; keeping the old layout")
                }
            }
        }
        Ok(_) => {}
        Err(e) => tracing::debug!(%e, "cannot list monitors"),
    }
}

fn new_encoder(
    ctx: &PipelineCtx,
    adapter: &mut EncodeAdapter,
    w: u32,
    h: u32,
) -> Result<Box<dyn VideoEncoder>, String> {
    let cfg = adapter.encoder_config((w, h));
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

/// Why the encode thread woke up.
enum Wake {
    /// Something to encode and the frame interval elapsed.
    Encode,
    /// The adaptive controller is due.
    Tick,
}

/// Microseconds from the session clock to `t`.
fn clock_us(ctx: &PipelineCtx, t: Instant) -> u64 {
    u64::try_from(t.saturating_duration_since(ctx.adapt.clock).as_micros()).unwrap_or(u64::MAX)
}

fn encode_loop(
    ctx: &PipelineCtx,
    mailbox: &Mailbox,
    recycle: &std_mpsc::Sender<BgraFrame>,
) -> Result<(), String> {
    let mut adapter = EncodeAdapter::new(
        ctx.quality.clone(),
        Arc::clone(&mailbox.scale),
        &ctx.session,
    );
    let mut interval = frame_interval(adapter.fps());
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
        let wake = wait_for_work(
            ctx,
            mailbox,
            recycle,
            &mut st,
            &adapter,
            next_encode,
            interval,
        );
        let wake = match wake {
            Some(w) => w,
            None => return Ok(()),
        };
        if let Wake::Tick = wake {
            adapter.tick(&ctx.session, &ctx.adapt, st.enc.as_deref_mut());
            interval = frame_interval(adapter.fps());
            continue;
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
            st.enc = Some(new_encoder(ctx, &mut adapter, frame_size.0, frame_size.1)?);
            st.size = frame_size;
        }
        let Some(enc) = st.enc.as_mut() else {
            continue;
        };

        // The encoder uses the input timestamp for rate control: keep it monotonic even when
        // the same captured frame is re-encoded for a cursor move.
        frame.timestamp_us = clock_us(ctx, now);
        let drawn = st.cursor.draw_scaled(frame, *monitor, &mut st.patch);
        let encoded = enc.encode(frame);
        if drawn {
            st.patch.restore(frame);
        }
        // On the wire: when the oldest change in this frame happened, on the session clock
        // (also used by `LatencyEcho`), for the viewer's latency estimate.
        let timestamp_us = clock_us(ctx, since);
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
                        adapter.frame_dropped();
                        enc.request_keyframe();
                        st.pending_since.get_or_insert(now);
                    }
                    Err(e) => return Err(format!("cannot send video: {e}")),
                }
            }
            Ok(None) => {}
            Err(CodecError::SizeMismatch { .. }) => {
                st.enc = Some(new_encoder(ctx, &mut adapter, frame_size.0, frame_size.1)?);
                st.size = frame_size;
                st.pending_since.get_or_insert(now);
            }
            Err(e) => return Err(format!("encoder failed: {e}")),
        }
    }
}

/// Waits (without holding the lock while encoding) until there is something to send and the
/// frame interval has elapsed, or the controller tick is due. `None` = stop.
fn wait_for_work(
    ctx: &PipelineCtx,
    mailbox: &Mailbox,
    recycle: &std_mpsc::Sender<BgraFrame>,
    st: &mut EncodeState,
    adapter: &EncodeAdapter,
    next_encode: Instant,
    interval: Duration,
) -> Option<Wake> {
    let mut slot = mailbox.lock();
    loop {
        if slot.closed || stopped(ctx) {
            return None;
        }
        if let Some(c) = slot.frame.take() {
            st.pending_since.get_or_insert(c.at);
            if st.gen != Some(c.gen) {
                // New capturer: the old frame is stale and the stream must restart with a
                // keyframe (the encoder is rebuilt if the size changed).
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
        if now >= adapter.next_tick() {
            return Some(Wake::Tick);
        }
        let wait = match st.pending_since {
            Some(_) if st.last.is_some() && now >= next_encode => return Some(Wake::Encode),
            Some(_) if st.last.is_some() => next_encode - now,
            // Nothing to send: wake up now and then for keyframe requests / stop.
            _ => interval,
        };
        let wait = wait.min(adapter.next_tick().saturating_duration_since(now));
        slot = mailbox
            .cv
            .wait_timeout(slot, wait)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}
