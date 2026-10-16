//! The session agent's side of the **user-session features** (IPC v2).
//!
//! When the service hosts, everything that must happen on the logged-in user's desktop runs
//! in the agent: the clipboard (sync and Explorer paste of the peer's files), the whiteboard
//! overlay, the remote-pointer marker, the local-input hooks behind "local priority", the
//! privacy shield and loopback audio capture. The service keeps the session logic and talks
//! to these objects through proxies (`feature_proxy`); the agent creates and drives the real
//! ones here.
//!
//! * [`AgentFeatures`]: the OS seam, implemented by the app (it owns the clipboard, shell and
//!   audio crates); every method defaults to "unsupported", and tests pass fakes.
//! * [`FeatureHost`]: the agent's per-process state. [`FeatureHost::serve`] answers one
//!   [`AgentChannel::Feature`](super::ipc::AgentChannel::Feature) connection,
//!   [`FeatureHost::serve_audio`] streams one audio connection, and the objects' own signals
//!   (clipboard changed, local input, shield turned off, overlay clear clicked, Explorer
//!   started or abandoned a paste) leave on the events connection as [`AgentEvent`]s.
//!
//! The service is trusted (same binary, SYSTEM-only pipe), but counts are still capped so a
//! bug cannot pile up windows or hooks.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use tlp_desk_input::whiteboard::{
    lock_board, ClearRequestFn, Rgba, SharedBoard, Stroke, WhiteboardOverlay,
};
use tlp_desk_input::{
    InputError, LocalActivity, PointerMarker, PrivacyScreenText, PrivacyShield, ScreenLine,
    ShieldSignal, ShieldSignalFn,
};
use tlp_desk_proto::TransferItem;
use tokio::sync::watch;

use super::agent::monitor_rect;
use super::ipc::{
    self, AgentEvent, AudioPacket, ClipData, FeatureReply, FeatureRequest, WireError,
    WireScreenLine, WireShieldSignal, WireStage, WireStroke,
};
use crate::HostError;

/// Most overlays, markers, shields and virtual-file offers alive at once in the agent.
pub const MAX_FEATURE_OBJECTS: usize = 16;

/// Events queued for the service before new ones are dropped (the service reads them
/// continuously; a full queue means it is gone or stuck).
const EVENT_QUEUE: usize = 1024;

/// How often the local-input hooks are sampled for [`AgentEvent::LocalInput`].
pub const ACTIVITY_POLL: Duration = Duration::from_millis(20);

/// How often the clipboard's change number is sampled.
const CLIPBOARD_POLL: Duration = Duration::from_millis(100);

/// Longest wait for audio before a keep-alive packet.
pub const AUDIO_KEEPALIVE: Duration = Duration::from_millis(500);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A source of the session's system sound (loopback), as interleaved `f32` chunks.
pub trait AudioSource: Send {
    /// Waits up to `timeout` for the next chunk `(rate, channels, samples)`; `Ok(None)` when
    /// nothing arrived in time. An error ends the stream.
    fn next_chunk(&mut self, timeout: Duration) -> Result<Option<(u32, u16, Vec<f32>)>, WireError>;
}

/// The agent's handle on one virtual-files offer: Explorer's reads drive it.
pub trait StageLink: Send + Sync {
    /// Explorer asked for the bytes: start pulling (idempotent).
    fn begin(&self);
    /// How far the pull has come (pushed by the service).
    fn state(&self) -> WireStage;
    /// The offer left the clipboard: stop pulling.
    fn cancel(&self);
}

/// The user-session OS features the agent runs (implemented by the app; tests use fakes).
/// Every method defaults to "unsupported".
pub trait AgentFeatures: Send + Sync + 'static {
    /// The clipboard content in the richest format synced (`None`: empty or unsupported).
    fn clipboard_read(&self) -> Result<Option<ClipData>, WireError> {
        Err(WireError::Unsupported)
    }

    /// Replaces the clipboard; returns its change number right after the write.
    fn clipboard_write(&self, clip: &ClipData) -> Result<u64, WireError> {
        let _ = clip;
        Err(WireError::Unsupported)
    }

    /// The files copied in Explorer (`None`: no files on the clipboard).
    fn clipboard_files(&self) -> Option<Vec<String>> {
        None
    }

    /// Change notifications of the clipboard (its change number), if supported.
    fn clipboard_changes(&self) -> Option<watch::Receiver<u64>> {
        None
    }

    /// Puts the peer's announced `items` (staged in `dir`) on the clipboard as virtual
    /// files; `stage` reports Explorer's reads and the pull's progress.
    fn shell_offer(
        &self,
        offer: u64,
        dir: PathBuf,
        items: Vec<TransferItem>,
        stage: Arc<dyn StageLink>,
    ) {
        let _ = (offer, dir, items, stage);
    }

    /// Takes `offer` off the clipboard if it is still there.
    fn shell_clear(&self, offer: u64) {
        let _ = offer;
    }

    /// Creates the whiteboard overlay window for `board`.
    fn open_overlay(
        &self,
        board: SharedBoard,
        on_clear: ClearRequestFn,
    ) -> Result<Box<dyn WhiteboardOverlay>, InputError> {
        let _ = (board, on_clear);
        Err(InputError::Unsupported)
    }

    /// Creates the remote-pointer marker.
    fn open_marker(&self, label: &str) -> Result<Box<dyn PointerMarker>, InputError> {
        let _ = label;
        Err(InputError::Unsupported)
    }

    /// Starts the local-input hooks.
    fn start_activity(&self) -> Result<Arc<dyn LocalActivity>, InputError> {
        Err(InputError::Unsupported)
    }

    /// Creates an (idle) privacy shield.
    fn open_shield(&self, on_signal: ShieldSignalFn) -> Result<Box<dyn PrivacyShield>, InputError> {
        let _ = on_signal;
        Err(InputError::Unsupported)
    }

    /// Starts loopback capture of the session's system sound.
    fn open_audio(&self) -> Result<Box<dyn AudioSource>, WireError> {
        Err(WireError::Unsupported)
    }
}

/// An [`AgentFeatures`] that supports nothing (an agent without user-session features).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoFeatures;

impl AgentFeatures for NoFeatures {}

/// Where the agent's feature events go: the newest events connection.
#[derive(Debug, Default)]
struct EventFanIn {
    tx: Mutex<Option<SyncSender<AgentEvent>>>,
}

impl EventFanIn {
    fn emit(&self, ev: AgentEvent) {
        let delivered = lock(&self.tx)
            .as_ref()
            .is_some_and(|tx| tx.try_send(ev).is_ok());
        if !delivered {
            tracing::debug!("feature event dropped (no service listening)");
        }
    }

    fn subscribe(&self) -> Receiver<AgentEvent> {
        let (tx, rx) = sync_channel(EVENT_QUEUE);
        *lock(&self.tx) = Some(tx);
        rx
    }
}

/// The agent's side of one virtual-files offer.
struct RemoteStage {
    offer: u64,
    state: Mutex<WireStage>,
    began: AtomicBool,
    cancelled: AtomicBool,
    events: Weak<EventFanIn>,
}

impl StageLink for RemoteStage {
    fn begin(&self) {
        if !self.began.swap(true, Ordering::SeqCst) {
            if let Some(ev) = self.events.upgrade() {
                ev.emit(AgentEvent::StageBegin { offer: self.offer });
            }
        }
    }

    fn state(&self) -> WireStage {
        lock(&self.state).clone()
    }

    fn cancel(&self) {
        if !self.cancelled.swap(true, Ordering::SeqCst) {
            if let Some(ev) = self.events.upgrade() {
                ev.emit(AgentEvent::StageCancel { offer: self.offer });
            }
        }
    }
}

/// The running local-input hooks and their sampler.
struct ActivityRun {
    stop: Arc<AtomicBool>,
}

impl Drop for ActivityRun {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

#[derive(Default)]
struct State {
    overlays: HashMap<u64, (SharedBoard, Box<dyn WhiteboardOverlay>)>,
    markers: HashMap<u64, Box<dyn PointerMarker>>,
    shields: HashMap<u64, Box<dyn PrivacyShield>>,
    offers: HashMap<u64, Arc<RemoteStage>>,
    activity: Option<ActivityRun>,
}

/// The agent's user-session feature objects (one per agent process).
pub struct FeatureHost {
    features: Arc<dyn AgentFeatures>,
    events: Arc<EventFanIn>,
    state: Mutex<State>,
    clipboard_watch: AtomicBool,
}

impl std::fmt::Debug for FeatureHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeatureHost").finish_non_exhaustive()
    }
}

fn screen_text(lines: Vec<WireScreenLine>) -> PrivacyScreenText {
    PrivacyScreenText {
        lines: lines
            .into_iter()
            .map(|l| ScreenLine {
                text: l.text,
                large: l.large,
            })
            .collect(),
    }
    .sanitized()
}

fn wire_signal(s: ShieldSignal) -> WireShieldSignal {
    match s {
        ShieldSignal::EmergencyExit => WireShieldSignal::EmergencyExit,
        ShieldSignal::SecureDesktop => WireShieldSignal::SecureDesktop,
        ShieldSignal::Failed => WireShieldSignal::Failed,
    }
}

fn input_wire(e: &InputError) -> WireError {
    match e {
        InputError::Unsupported => WireError::Unsupported,
        InputError::Blocked(s) => WireError::Blocked(s.clone()),
        InputError::Platform(s) => WireError::Other(s.clone()),
    }
}

fn too_many() -> FeatureReply {
    FeatureReply::Error(WireError::Other("too many feature objects".into()))
}

/// A stroke chunk from the wire, with the same caps the board applies to peers.
fn stroke(s: WireStroke) -> Stroke {
    Stroke {
        id: s.id,
        points: s.points,
        color: Rgba(s.color),
        width: s.width,
    }
}

impl FeatureHost {
    /// The agent's feature state around `features`.
    pub fn new(features: Arc<dyn AgentFeatures>) -> Arc<Self> {
        Arc::new(Self {
            features,
            events: Arc::default(),
            state: Mutex::default(),
            clipboard_watch: AtomicBool::new(false),
        })
    }

    /// Takes over the feature events: the newest events connection gets them (an older one
    /// stops receiving).
    pub fn subscribe(self: &Arc<Self>) -> Receiver<AgentEvent> {
        let rx = self.events.subscribe();
        self.start_clipboard_watch();
        rx
    }

    /// Samples the clipboard's change number (once per agent, on the first subscription).
    fn start_clipboard_watch(self: &Arc<Self>) {
        if self.clipboard_watch.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(mut changes) = self.features.clipboard_changes() else {
            return;
        };
        let events = Arc::downgrade(&self.events);
        let spawned = std::thread::Builder::new()
            .name("tlp-agent-clipboard".into())
            .spawn(move || loop {
                std::thread::sleep(CLIPBOARD_POLL);
                let Some(events) = events.upgrade() else {
                    return;
                };
                match changes.has_changed() {
                    Ok(true) => {
                        let seq = *changes.borrow_and_update();
                        events.emit(AgentEvent::ClipboardChanged { seq });
                    }
                    Ok(false) => {}
                    Err(_) => return,
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(%e, "cannot watch the clipboard");
        }
    }

    /// Serves one feature connection (after the welcome) until it closes.
    pub fn serve<C: Read + Write + ?Sized>(&self, conn: &mut C) -> Result<(), HostError> {
        loop {
            let req: FeatureRequest = match ipc::read_msg(conn) {
                Ok(r) => r,
                Err(HostError::IpcClosed) => return Ok(()),
                Err(e) => return Err(e),
            };
            match req {
                FeatureRequest::ClipboardRead => match self.features.clipboard_read() {
                    Ok(clip) => ipc::write_clip_reply(conn, clip.as_ref())?,
                    Err(e) => ipc::write_msg(conn, &FeatureReply::Error(e))?,
                },
                FeatureRequest::ClipboardWrite(header) => {
                    // Read the payload first so the connection stays in step on errors.
                    let reply = match ipc::read_clip_payload(conn, header) {
                        Ok(clip) => match self.features.clipboard_write(&clip) {
                            Ok(seq) => FeatureReply::ClipboardSeq(seq),
                            Err(e) => FeatureReply::Error(e),
                        },
                        Err(HostError::IpcRefused(why)) => {
                            FeatureReply::Error(WireError::Other(why))
                        }
                        Err(e) => return Err(e),
                    };
                    ipc::write_msg(conn, &reply)?;
                }
                other => {
                    let reply = self.handle(other);
                    ipc::write_msg(conn, &reply)?;
                }
            }
        }
    }

    /// Handles every request without a payload.
    fn handle(&self, req: FeatureRequest) -> FeatureReply {
        match req {
            FeatureRequest::ClipboardRead | FeatureRequest::ClipboardWrite(_) => {
                FeatureReply::Error(WireError::Other("payload request".into()))
            }
            FeatureRequest::ClipboardFiles => FeatureReply::Files(self.features.clipboard_files()),
            FeatureRequest::ShellOffer { offer, dir, items } => self.shell_offer(offer, dir, items),
            FeatureRequest::ShellStage { offer, state } => {
                if let Some(s) = lock(&self.state).offers.get(&offer) {
                    *lock(&s.state) = state;
                }
                FeatureReply::Ok
            }
            FeatureRequest::ShellClear { offer } => {
                let removed = lock(&self.state).offers.remove(&offer);
                if removed.is_some() {
                    self.features.shell_clear(offer);
                }
                FeatureReply::Ok
            }
            FeatureRequest::OverlayOpen { id } => self.overlay_open(id),
            FeatureRequest::OverlayTarget { id, rect } => {
                if let Some((_, o)) = lock(&self.state).overlays.get_mut(&id) {
                    o.set_target(monitor_rect(rect));
                }
                FeatureReply::Ok
            }
            FeatureRequest::OverlayStrokes { id, reset, strokes } => {
                if let Some((board, o)) = lock(&self.state).overlays.get_mut(&id) {
                    {
                        let mut b = lock_board(board);
                        if reset {
                            b.clear();
                        }
                        for s in strokes {
                            b.add(&stroke(s));
                        }
                    }
                    o.changed();
                }
                FeatureReply::Ok
            }
            FeatureRequest::OverlayClose { id } => {
                // Dropping the overlay removes the window.
                let removed = lock(&self.state).overlays.remove(&id);
                drop(removed);
                FeatureReply::Ok
            }
            FeatureRequest::MarkerOpen { id, label } => self.marker_open(id, &label),
            FeatureRequest::MarkerVisible { id, visible } => {
                if let Some(m) = lock(&self.state).markers.get_mut(&id) {
                    m.set_visible(visible);
                }
                FeatureReply::Ok
            }
            FeatureRequest::MarkerLabel { id, label } => {
                if let Some(m) = lock(&self.state).markers.get_mut(&id) {
                    m.set_label(&label);
                }
                FeatureReply::Ok
            }
            FeatureRequest::MarkerClose { id } => {
                let removed = lock(&self.state).markers.remove(&id);
                drop(removed);
                FeatureReply::Ok
            }
            FeatureRequest::ActivityStart => self.activity_start(),
            FeatureRequest::ActivityStop => {
                let run = lock(&self.state).activity.take();
                drop(run);
                FeatureReply::Ok
            }
            FeatureRequest::ShieldEngage { id, lines } => self.shield_engage(id, lines),
            FeatureRequest::ShieldText { id, lines } => {
                if let Some(s) = lock(&self.state).shields.get_mut(&id) {
                    s.set_text(&screen_text(lines));
                }
                FeatureReply::Ok
            }
            FeatureRequest::ShieldRelease { id } => {
                let removed = lock(&self.state).shields.remove(&id);
                if let Some(mut s) = removed {
                    s.release();
                }
                FeatureReply::Ok
            }
        }
    }

    fn shell_offer(&self, offer: u64, dir: String, items: Vec<TransferItem>) -> FeatureReply {
        let dir = PathBuf::from(dir);
        if !dir.is_absolute() {
            return FeatureReply::Error(WireError::Other("staging folder is not absolute".into()));
        }
        let stage = {
            let mut st = lock(&self.state);
            if st.offers.len() >= MAX_FEATURE_OBJECTS && !st.offers.contains_key(&offer) {
                return too_many();
            }
            let stage = Arc::new(RemoteStage {
                offer,
                state: Mutex::new(WireStage::Idle),
                began: AtomicBool::new(false),
                cancelled: AtomicBool::new(false),
                events: Arc::downgrade(&self.events),
            });
            st.offers.insert(offer, stage.clone());
            stage
        };
        tracing::info!(
            offer,
            items = items.len(),
            "virtual files offered on the clipboard"
        );
        self.features.shell_offer(offer, dir, items, stage);
        FeatureReply::Ok
    }

    fn overlay_open(&self, id: u64) -> FeatureReply {
        if lock(&self.state).overlays.len() >= MAX_FEATURE_OBJECTS {
            return too_many();
        }
        let board = SharedBoard::default();
        let events = Arc::downgrade(&self.events);
        let on_clear: ClearRequestFn = Arc::new(move || {
            if let Some(ev) = events.upgrade() {
                ev.emit(AgentEvent::OverlayClear { id });
            }
        });
        match self.features.open_overlay(board.clone(), on_clear) {
            Ok(o) => {
                lock(&self.state).overlays.insert(id, (board, o));
                FeatureReply::Ok
            }
            Err(e) => FeatureReply::Error(input_wire(&e)),
        }
    }

    fn marker_open(&self, id: u64, label: &str) -> FeatureReply {
        if lock(&self.state).markers.len() >= MAX_FEATURE_OBJECTS {
            return too_many();
        }
        match self.features.open_marker(label) {
            Ok(m) => {
                lock(&self.state).markers.insert(id, m);
                FeatureReply::Ok
            }
            Err(e) => FeatureReply::Error(input_wire(&e)),
        }
    }

    fn activity_start(&self) -> FeatureReply {
        if lock(&self.state).activity.is_some() {
            return FeatureReply::Ok;
        }
        let activity = match self.features.start_activity() {
            Ok(a) => a,
            Err(e) => return FeatureReply::Error(input_wire(&e)),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let events = Arc::downgrade(&self.events);
        let flag = stop.clone();
        let spawned = std::thread::Builder::new()
            .name("tlp-agent-activity".into())
            .spawn(move || sample_activity(&*activity, &flag, &events));
        match spawned {
            Ok(_) => {
                lock(&self.state).activity = Some(ActivityRun { stop });
                FeatureReply::Ok
            }
            Err(e) => FeatureReply::Error(WireError::Other(e.to_string())),
        }
    }

    fn shield_engage(&self, id: u64, lines: Vec<WireScreenLine>) -> FeatureReply {
        let text = screen_text(lines);
        let mut st = lock(&self.state);
        if let Some(s) = st.shields.get_mut(&id) {
            return match s.engage(&text) {
                Ok(()) => FeatureReply::Ok,
                Err(e) => FeatureReply::Error(input_wire(&e)),
            };
        }
        if st.shields.len() >= MAX_FEATURE_OBJECTS {
            return too_many();
        }
        drop(st);
        let events = Arc::downgrade(&self.events);
        // Runs on the shield's own thread: only queue the event, never block or call back.
        let on_signal: ShieldSignalFn = Arc::new(move |signal| {
            if let Some(ev) = events.upgrade() {
                ev.emit(AgentEvent::Shield {
                    id,
                    signal: wire_signal(signal),
                });
            }
        });
        let mut shield = match self.features.open_shield(on_signal) {
            Ok(s) => s,
            Err(e) => return FeatureReply::Error(input_wire(&e)),
        };
        match shield.engage(&text) {
            Ok(()) => {
                lock(&self.state).shields.insert(id, shield);
                FeatureReply::Ok
            }
            Err(e) => FeatureReply::Error(input_wire(&e)),
        }
    }

    /// Streams loopback audio on one connection (after the welcome) until the service closes
    /// it or capture fails. Dropping the source stops the capture.
    pub fn serve_audio<C: Write + ?Sized>(&self, conn: &mut C) -> Result<(), HostError> {
        let mut source = match self.features.open_audio() {
            Ok(s) => s,
            Err(e) => {
                tracing::info!(?e, "agent audio capture unavailable");
                return Ok(());
            }
        };
        let mut bytes = Vec::new();
        loop {
            let (rate, channels, samples) = match source.next_chunk(AUDIO_KEEPALIVE) {
                Ok(Some(c)) => c,
                Ok(None) => (0, 0, Vec::new()),
                Err(e) => {
                    tracing::info!(?e, "agent audio capture ended");
                    return Ok(());
                }
            };
            bytes.clear();
            bytes.reserve(samples.len().saturating_mul(4));
            for s in &samples {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
            let hdr = AudioPacket {
                rate,
                channels,
                payload_len: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            };
            // A failed write means the service closed the connection: stop capturing.
            if ipc::write_msg_with_payload(conn, &hdr, &bytes).is_err() {
                return Ok(());
            }
        }
    }
}

/// Reports physical local input while the hooks run (at most once per [`ACTIVITY_POLL`]).
fn sample_activity(activity: &dyn LocalActivity, stop: &AtomicBool, events: &Weak<EventFanIn>) {
    // The hooks were started for this sampler: anything they recorded is news.
    let mut last_key: Option<Instant> = None;
    let mut last_ptr: Option<Instant> = None;
    loop {
        std::thread::sleep(ACTIVITY_POLL);
        if stop.load(Ordering::Acquire) {
            return;
        }
        let key = activity.last_physical_keyboard();
        let ptr = activity.last_physical_pointer();
        let keyboard = key > last_key;
        let pointer = ptr > last_ptr;
        if keyboard || pointer {
            let Some(ev) = events.upgrade() else {
                return;
            };
            ev.emit(AgentEvent::LocalInput { keyboard, pointer });
            last_key = key;
            last_ptr = ptr;
        }
    }
}

#[cfg(test)]
mod tests;
