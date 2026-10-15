//! A running viewer session: network tasks on tokio, decoding on a dedicated std thread.

use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tlp_desk_net::{
    FeatureDispatch, FeatureEvent, FeatureEventSink, FeatureSet, NetError, PeerInfo, Session,
};
use tlp_desk_proto::{ByeReason, InputEvent, Message, MonitorDesc, Permissions, VideoFrame};
use tokio::sync::{mpsc, watch, Notify};

use crate::frame::{convert, DecodedFrame, FrameSlot, PixelFormat};
use crate::RepaintFn;

/// Why a viewer session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndReason {
    /// [`ViewerSession::disconnect`] (or the session was dropped).
    Local,
    /// The host said goodbye (`UserClosed` = the host user disconnected us,
    /// `PermissionRevoked` = viewing was revoked).
    Remote(ByeReason),
    /// The connection dropped without a goodbye (network loss, host crashed, idle timeout).
    ConnectionLost(String),
    /// Local failure (e.g. the decoder could not start).
    Error(String),
}

/// Live statistics (updated about once per second).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ViewerStats {
    /// Decoded frames per second.
    pub fps: f32,
    /// Received video bitrate.
    pub bitrate_kbps: f32,
    /// QUIC smoothed round-trip time.
    pub rtt_ms: f32,
    /// Approximate glass-to-glass latency without display time: host change -> decoded.
    /// Estimated as RTT/2 + queueing delay above the best frame seen + decode time (the two
    /// clocks are not synchronized, so this is an estimate).
    pub latency_ms: f32,
    /// Average decode time per frame.
    pub decode_ms: f32,
    pub frames_received: u64,
    pub frames_decoded: u64,
    /// Size of the latest decoded frame.
    pub width: u32,
    pub height: u32,
}

/// Events for the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewerEvent {
    /// The host changed what this viewer may do.
    PermissionsChanged(Permissions),
    /// About once per second.
    Stats(ViewerStats),
    /// The host user is using their own mouse/keyboard (`true`): our input is ignored until
    /// it changes back to `false`.
    HostInputPriority(bool),
    /// The host's monitors or the streamed one changed (see [`ViewerSession::monitors`]).
    MonitorsChanged {
        monitors: Vec<MonitorDesc>,
        active: u32,
    },
    /// Something a session feature reports (see `FeatureEvent::payload`).
    Feature(FeatureEvent),
    /// The session is over; no more events follow.
    Ended(EndReason),
}

/// Receiving end of the session event stream.
pub type ViewerEvents = mpsc::UnboundedReceiver<ViewerEvent>;

#[derive(Default)]
struct Counters {
    received: AtomicU64,
    bytes: AtomicU64,
    decoded: AtomicU64,
    decode_us: AtomicU64,
    latency_us: AtomicU64,
    latency_samples: AtomicU64,
    size: AtomicU64,
}

struct Shared {
    session: Arc<Session>,
    frames: Arc<FrameSlot>,
    perms: AtomicU16,
    host_priority: AtomicBool,
    monitors: Mutex<(Vec<MonitorDesc>, u32)>,
    stats: Mutex<ViewerStats>,
    counters: Counters,
    end: watch::Sender<Option<EndReason>>,
    events: mpsc::UnboundedSender<ViewerEvent>,
    disconnect: Notify,
    on_frame: Option<RepaintFn>,
    format: PixelFormat,
    started: Instant,
    /// Sees every received frame before decoding (session recording).
    tap: Mutex<Option<crate::recording::VideoTap>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Shared {
    fn emit(&self, ev: ViewerEvent) {
        let _ = self.events.send(ev);
        self.repaint();
    }

    fn repaint(&self) {
        if let Some(f) = &self.on_frame {
            f();
        }
    }

    /// Records the end reason; only the first call counts.
    fn end(&self, reason: EndReason) {
        let mut first = false;
        self.end.send_if_modified(|cur| {
            if cur.is_none() {
                *cur = Some(reason.clone());
                first = true;
                true
            } else {
                false
            }
        });
        if first {
            tracing::info!(host = %self.session.peer().device_id, ?reason, "viewer session ended");
            self.emit(ViewerEvent::Ended(reason));
        }
    }
}

/// Source of [`ViewerSession::feature_key`]s, unique per process.
static NEXT_FEATURE_KEY: AtomicU64 = AtomicU64::new(1);

/// An established viewer session. Dropping it disconnects.
pub struct ViewerSession {
    shared: Arc<Shared>,
    feature_key: u64,
    /// Viewer -> host control messages (input, monitor selection), in order.
    input: mpsc::UnboundedSender<Message>,
    ended: watch::Receiver<Option<EndReason>>,
}

impl std::fmt::Debug for ViewerSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewerSession")
            .field("peer", self.peer())
            .finish_non_exhaustive()
    }
}

struct Job {
    frame: VideoFrame,
    /// Delay of this frame above the fastest frame seen so far.
    queue_delay_us: u64,
    received: Instant,
}

impl ViewerSession {
    pub(crate) fn start(
        session: Session,
        format: PixelFormat,
        on_frame: Option<RepaintFn>,
        features: &FeatureSet,
    ) -> (ViewerSession, ViewerEvents) {
        let session = Arc::new(session);
        let feature_key = NEXT_FEATURE_KEY.fetch_add(1, Ordering::Relaxed);
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (end_tx, end_rx) = watch::channel(None);
        let shared = Arc::new(Shared {
            perms: AtomicU16::new(session.initial_permissions().0),
            host_priority: AtomicBool::new(false),
            monitors: Mutex::new((Vec::new(), 0)),
            session,
            frames: FrameSlot::new(),
            stats: Mutex::default(),
            counters: Counters::default(),
            end: end_tx,
            events: events_tx,
            disconnect: Notify::new(),
            on_frame,
            format,
            started: Instant::now(),
            tap: Mutex::new(None),
        });
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        // Capacity 1: if decoding falls behind, the network layer's small queue overflows and
        // it drops frames and requests a keyframe instead of letting latency grow.
        let (job_tx, job_rx) = mpsc::channel::<Job>(1);

        let rt = tokio::runtime::Handle::current();
        {
            let thread_shared = shared.clone();
            if let Err(e) = std::thread::Builder::new()
                .name("tlp-viewer-decode".into())
                .spawn(move || decode_thread(thread_shared, job_rx, rt))
            {
                shared.end(EndReason::Error(format!("cannot spawn decode thread: {e}")));
                shared.disconnect.notify_one();
            }
        }
        tokio::spawn(video_task(shared.clone(), job_tx));
        // FEATURE HOOK: every registered feature starts here; nothing per feature in this file.
        let dispatch = {
            let events = shared.events.clone();
            let repaint = shared.on_frame.clone();
            let sink: FeatureEventSink = Arc::new(move |event| {
                let _ = events.send(ViewerEvent::Feature(event));
                if let Some(f) = &repaint {
                    f();
                }
            });
            FeatureDispatch::start(
                features,
                shared.session.clone(),
                feature_key,
                shared.session.initial_permissions(),
                sink,
            )
        };
        tokio::spawn(control_task(shared.clone(), dispatch));
        tokio::spawn(input_task(shared.clone(), input_rx, end_rx.clone()));
        tokio::spawn(stats_task(shared.clone(), end_rx.clone()));

        (
            ViewerSession {
                shared,
                feature_key,
                input: input_tx,
                ended: end_rx,
            },
            events_rx,
        )
    }

    /// The host (device ID and fingerprint are authenticated; the name is self-reported).
    pub fn peer(&self) -> &PeerInfo {
        self.shared.session.peer()
    }

    /// Current permissions (initial grant plus live changes from the host).
    pub fn permissions(&self) -> Permissions {
        Permissions(self.shared.perms.load(Ordering::Acquire))
    }

    /// The latest-frame mailbox (share it with the renderer).
    pub fn frames(&self) -> Arc<FrameSlot> {
        self.shared.frames.clone()
    }

    /// Shortcut for `frames().take()`.
    pub fn take_frame(&self) -> Option<DecodedFrame> {
        self.shared.frames.take()
    }

    /// Statistics of the last full second.
    pub fn stats(&self) -> ViewerStats {
        *lock(&self.shared.stats)
    }

    /// Current QUIC round-trip time.
    pub fn rtt(&self) -> Duration {
        self.shared.session.rtt()
    }

    /// Queues one input event for the host (non-blocking). Returns `false` once the session
    /// has ended. The host enforces permissions; the UI should not send what is not allowed.
    pub fn send_input(&self, event: InputEvent) -> bool {
        self.ended.borrow().is_none() && self.input.send(Message::InputEvent(event)).is_ok()
    }

    /// The host's monitors (empty until the host sent its list) and the streamed one.
    pub fn monitors(&self) -> (Vec<MonitorDesc>, u32) {
        lock(&self.shared.monitors).clone()
    }

    /// Asks the host to stream (and map input to) another monitor (non-blocking). The switch
    /// is confirmed by [`ViewerEvent::MonitorsChanged`]; unknown indices are ignored by the
    /// host. Returns `false` once the session has ended.
    pub fn select_monitor(&self, index: u32) -> bool {
        self.ended.borrow().is_none() && self.input.send(Message::SelectMonitor { index }).is_ok()
    }

    /// Key identifying this session to features (`FeatureCtx::session_key`,
    /// `FeatureEvent::session_key`); unique within the process.
    pub fn feature_key(&self) -> u64 {
        self.feature_key
    }

    /// Queues any control message for the host, in order with input (non-blocking). Features
    /// normally send through their `FeatureCtx`; this is for the UI and tests. Returns `false`
    /// once the session has ended.
    pub fn send_message(&self, msg: Message) -> bool {
        self.ended.borrow().is_none() && self.input.send(msg).is_ok()
    }

    /// Installs (or with `None` removes) a tap that sees every received video frame before
    /// it is decoded; used by [`crate::Recording`]. One tap at a time.
    pub fn set_video_tap(&self, tap: Option<crate::recording::VideoTap>) {
        *lock(&self.shared.tap) = tap;
    }

    /// Asks the host for a keyframe (non-blocking). Returns `false` once the session has
    /// ended.
    pub fn request_keyframe(&self) -> bool {
        self.send_message(Message::RequestKeyframe)
    }

    /// Whether the host user currently has input priority (our input is being ignored).
    pub fn host_input_priority(&self) -> bool {
        self.shared.host_priority.load(Ordering::Acquire)
    }

    /// Ends the session (non-blocking). [`ViewerEvent::Ended`] with [`EndReason::Local`]
    /// follows unless the session had already ended.
    pub fn disconnect(&self) {
        self.shared.disconnect.notify_one();
    }

    /// `Some(reason)` once the session is over.
    pub fn end_reason(&self) -> Option<EndReason> {
        self.ended.borrow().clone()
    }

    /// Waits until the session is over.
    pub async fn ended(&self) -> EndReason {
        let mut rx = self.ended.clone();
        loop {
            if let Some(r) = rx.borrow_and_update().clone() {
                return r;
            }
            if rx.changed().await.is_err() {
                return EndReason::Error("session state dropped".into());
            }
        }
    }
}

impl Drop for ViewerSession {
    fn drop(&mut self) {
        self.disconnect();
    }
}

async fn video_task(shared: Arc<Shared>, jobs: mpsc::Sender<Job>) {
    let mut min_offset: Option<i64> = None;
    loop {
        let frame = match shared.session.recv_video().await {
            Ok(f) => f,
            Err(e) => {
                tracing::debug!(%e, "video stream ended");
                break;
            }
        };
        let arrival = shared.started.elapsed().as_micros() as i64;
        let offset = arrival - frame.timestamp_us as i64;
        let min = min_offset.map_or(offset, |m| m.min(offset));
        min_offset = Some(min);
        shared.counters.received.fetch_add(1, Ordering::Relaxed);
        shared
            .counters
            .bytes
            .fetch_add(frame.data.len() as u64, Ordering::Relaxed);
        let tap = lock(&shared.tap).clone();
        if let Some(tap) = tap {
            tap(&frame);
        }
        let job = Job {
            frame,
            queue_delay_us: (offset - min) as u64,
            received: Instant::now(),
        };
        if jobs.send(job).await.is_err() {
            shared.end(EndReason::Error("video decoder stopped".into()));
            shared.disconnect.notify_one();
            break;
        }
    }
}

fn decode_thread(shared: Arc<Shared>, mut jobs: mpsc::Receiver<Job>, rt: tokio::runtime::Handle) {
    let mut decoder = match tlp_desk_codec::new_decoder() {
        Ok(d) => d,
        Err(e) => {
            shared.end(EndReason::Error(format!("cannot start video decoder: {e}")));
            shared.disconnect.notify_one();
            return;
        }
    };
    let mut need_keyframe = false;
    while let Some(job) = jobs.blocking_recv() {
        if need_keyframe && !job.frame.keyframe {
            continue;
        }
        let t0 = Instant::now();
        match decoder.decode(&job.frame.data) {
            Ok(Some(bgra)) => {
                need_keyframe = false;
                let (width, height) = (bgra.width, bgra.height);
                let pixels = convert(&bgra, shared.format, shared.frames.take_spare());
                decoder.recycle(bgra);
                let done = Instant::now();
                let c = &shared.counters;
                c.decoded.fetch_add(1, Ordering::Relaxed);
                c.decode_us
                    .fetch_add((done - t0).as_micros() as u64, Ordering::Relaxed);
                let rtt_half = shared.session.rtt().as_micros() as u64 / 2;
                let latency =
                    rtt_half + job.queue_delay_us + (done - job.received).as_micros() as u64;
                c.latency_us.fetch_add(latency, Ordering::Relaxed);
                c.latency_samples.fetch_add(1, Ordering::Relaxed);
                let frame = DecodedFrame {
                    width,
                    height,
                    format: shared.format,
                    pixels,
                    seq: job.frame.seq,
                    host_timestamp_us: job.frame.timestamp_us,
                };
                c.size.store(
                    (u64::from(frame.width) << 32) | u64::from(frame.height),
                    Ordering::Relaxed,
                );
                shared.frames.publish(frame);
                shared.repaint();
            }
            Ok(None) => {}
            Err(e) => {
                tracing::debug!(%e, seq = job.frame.seq, "decode failed; waiting for a keyframe");
                need_keyframe = true;
                let session = shared.session.clone();
                rt.spawn(async move {
                    let _ = session.send(&Message::RequestKeyframe).await;
                });
            }
        }
    }
    tracing::debug!("decode thread stopped");
}

async fn control_task(shared: Arc<Shared>, features: FeatureDispatch) {
    let reason = loop {
        tokio::select! {
            _ = shared.disconnect.notified() => {
                shared.session.close(ByeReason::UserClosed).await;
                break EndReason::Local;
            }
            msg = shared.session.recv() => match msg {
                Ok(Message::Permissions(p)) => {
                    tracing::info!(?p, "host changed permissions");
                    shared.perms.store(p.0, Ordering::Release);
                    features.set_permissions(p);
                    shared.emit(ViewerEvent::PermissionsChanged(p));
                }
                Ok(Message::HostInputPriority { local_active }) => {
                    tracing::info!(local_active, "host input priority changed");
                    shared.host_priority.store(local_active, Ordering::Release);
                    shared.emit(ViewerEvent::HostInputPriority(local_active));
                }
                Ok(Message::MonitorList { monitors, active }) => {
                    tracing::debug!(count = monitors.len(), active, "host monitor list");
                    *lock(&shared.monitors) = (monitors.clone(), active);
                    shared.emit(ViewerEvent::MonitorsChanged { monitors, active });
                }
                Ok(Message::Bye(b)) => break EndReason::Remote(b.reason),
                Ok(other) => {
                    if let Err(msg) = features.dispatch(other) {
                        tracing::debug!(msg = ?std::mem::discriminant(&*msg), "no feature handles this control message; ignoring");
                    }
                }
                Err(NetError::Closed) => break EndReason::ConnectionLost("control stream closed".into()),
                Err(e) => break EndReason::ConnectionLost(e.to_string()),
            }
        }
    };
    // Ends every feature's inbox: features stop with the session.
    drop(features);
    shared.end(reason);
}

/// Resolves once the session has ended (without holding the watch guard across awaits).
async fn wait_ended(ended: &mut watch::Receiver<Option<EndReason>>) {
    let _ = ended.wait_for(Option::is_some).await.map(|_| ());
}

async fn input_task(
    shared: Arc<Shared>,
    mut rx: mpsc::UnboundedReceiver<Message>,
    mut ended: watch::Receiver<Option<EndReason>>,
) {
    loop {
        tokio::select! {
            msg = rx.recv() => {
                let Some(msg) = msg else { break };
                if let Err(e) = shared.session.send(&msg).await {
                    tracing::debug!(%e, "input send failed");
                    break;
                }
            }
            _ = wait_ended(&mut ended) => break,
        }
    }
}

async fn stats_task(shared: Arc<Shared>, mut ended: watch::Receiver<Option<EndReason>>) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.tick().await;
    let mut last = Instant::now();
    let (mut last_dec, mut last_bytes, mut last_dec_us, mut last_lat, mut last_lat_n) =
        (0u64, 0u64, 0u64, 0u64, 0u64);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = wait_ended(&mut ended) => break,
        }
        let now = Instant::now();
        let dt = (now - last).as_secs_f32().max(1e-3);
        last = now;
        let c = &shared.counters;
        let dec = c.decoded.load(Ordering::Relaxed);
        let bytes = c.bytes.load(Ordering::Relaxed);
        let dec_us = c.decode_us.load(Ordering::Relaxed);
        let lat = c.latency_us.load(Ordering::Relaxed);
        let lat_n = c.latency_samples.load(Ordering::Relaxed);
        let size = c.size.load(Ordering::Relaxed);
        let frames = dec - last_dec;
        let lat_frames = lat_n - last_lat_n;
        let mut s = lock(&shared.stats);
        s.fps = frames as f32 / dt;
        s.bitrate_kbps = (bytes - last_bytes) as f32 * 8.0 / 1000.0 / dt;
        s.rtt_ms = shared.session.rtt().as_secs_f32() * 1000.0;
        if frames > 0 {
            s.decode_ms = (dec_us - last_dec_us) as f32 / 1000.0 / frames as f32;
        }
        if lat_frames > 0 {
            s.latency_ms = (lat - last_lat) as f32 / 1000.0 / lat_frames as f32;
        }
        s.frames_received = c.received.load(Ordering::Relaxed);
        s.frames_decoded = dec;
        s.width = (size >> 32) as u32;
        s.height = size as u32;
        let snapshot = *s;
        drop(s);
        (last_dec, last_bytes, last_dec_us, last_lat, last_lat_n) =
            (dec, bytes, dec_us, lat, lat_n);
        shared.emit(ViewerEvent::Stats(snapshot));
    }
}
