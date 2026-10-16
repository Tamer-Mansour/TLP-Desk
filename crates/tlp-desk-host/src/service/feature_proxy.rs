//! The service's side of the **user-session features** (IPC v2): proxies that look like the
//! local OS objects to the host's session features but live in the session agent.
//!
//! [`AgentFeatureHub`] is created once per service around its [`AgentLink`]. It hands out
//! factories for [`HostConfig`](crate::HostConfig) and the session features:
//!
//! | factory | local object it replaces | agent request |
//! |---|---|---|
//! | [`AgentFeatureHub::local_activity_factory`] | low-level input hooks | `ActivityStart` / `ActivityStop` |
//! | [`AgentFeatureHub::pointer_marker_factory`] | green remote-pointer marker | `Marker*` |
//! | [`AgentFeatureHub::overlay_factory`] | whiteboard overlay window | `Overlay*` |
//! | [`AgentFeatureHub::shield_factory`] | privacy shield | `Shield*` |
//!
//! and plain calls for the app's clipboard, Explorer-paste and audio proxies
//! ([`AgentFeatureHub::clipboard_read`], [`AgentFeatureHub::shell_offer`],
//! [`AgentFeatureHub::open_audio`], ...).
//!
//! * **Never block a caller that must not block.** Overlay, marker and shield-text updates are
//!   queued to one worker thread (in order); only calls whose result matters (clipboard,
//!   engaging the shield) wait for the agent.
//! * **Agent restarts** (logon, logoff, user switch, crash): the service calls
//!   [`AgentFeatureHub::agent_changed`]. Overlays and markers re-create themselves in the new
//!   agent on their next update, the local-input hooks are restarted, and an engaged privacy
//!   shield reports [`ShieldSignal::Failed`] (the old agent's shield died with it, so privacy
//!   mode turns off and the viewer is told).
//! * Events from the agent ([`AgentEvent`]) come back through [`AgentFeatureHub::on_event`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Instant;

use tlp_desk_input::whiteboard::{
    lock_board, ClearRequestFn, SharedBoard, WhiteboardOverlay, MAX_POINTS_PER_STROKE,
};
use tlp_desk_input::{
    ActivityStamp, InputError, LocalActivity, MonitorRect, PointerMarker, PrivacyScreenText,
    PrivacyShield, ShieldSignal, ShieldSignalFn,
};
use tlp_desk_proto::TransferItem;
use tokio::sync::watch;

use super::ipc::{
    self, AgentChannel, AgentEvent, AudioPacket, ClipData, FeatureReply, FeatureRequest, WireError,
    WireRect, WireScreenLine, WireShieldSignal, WireStage, WireStroke,
};
use super::remote::{AgentConn, AgentLink};
use crate::{HostError, LocalActivityFactory, OverlayFactory, PointerMarkerFactory, ShieldFactory};

/// Jobs queued for the agent before new ones are dropped (the agent is gone or stuck).
const QUEUE: usize = 1024;

/// Most stroke points sent in one overlay request (keeps every header far below the IPC cap).
pub const OVERLAY_POINTS_PER_REQUEST: usize = 8192;

type Job = Box<dyn FnOnce(&AgentLink) + Send>;

/// Called with `true` when Explorer starts pasting an offer, `false` when the offer left the
/// clipboard (see [`AgentFeatureHub::shell_offer`]).
pub type StageHandler = Arc<dyn Fn(bool) + Send + Sync>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn wire_rect(r: MonitorRect) -> WireRect {
    WireRect {
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
    }
}

fn screen_lines(text: &PrivacyScreenText) -> Vec<WireScreenLine> {
    text.sanitized()
        .lines
        .into_iter()
        .map(|l| WireScreenLine {
            text: l.text,
            large: l.large,
        })
        .collect()
}

fn shield_signal(s: WireShieldSignal) -> ShieldSignal {
    match s {
        WireShieldSignal::EmergencyExit => ShieldSignal::EmergencyExit,
        WireShieldSignal::SecureDesktop => ShieldSignal::SecureDesktop,
        WireShieldSignal::Failed => ShieldSignal::Failed,
    }
}

fn wire_input(e: WireError) -> InputError {
    match e {
        WireError::Unsupported => InputError::Unsupported,
        WireError::Blocked(s) => InputError::Blocked(s),
        other => InputError::Platform(format!("agent: {other:?}")),
    }
}

/// One request without payload; app-level errors come back as [`FeatureReply::Error`], only
/// I/O and framing errors are `Err` (and reconnect).
fn call(link: &AgentLink, req: &FeatureRequest) -> Result<FeatureReply, HostError> {
    link.feature(|c| {
        ipc::write_msg(c, req)?;
        ipc::read_msg(c)
    })
}

/// Logs the outcome of a queued request.
fn call_logged(link: &AgentLink, req: &FeatureRequest) {
    match call(link, req) {
        Ok(FeatureReply::Ok) => {}
        Ok(other) => tracing::debug!(?other, "agent feature request not applied"),
        Err(e) => tracing::debug!(%e, "agent feature request failed"),
    }
}

#[derive(Default)]
struct HubState {
    activity_users: usize,
    stamps: Vec<Weak<ActivityStamp>>,
    shields: HashMap<u64, ShieldSignalFn>,
    overlays: HashMap<u64, ClearRequestFn>,
    stages: HashMap<u64, StageHandler>,
}

/// The service's handle on the agent's user-session features.
pub struct AgentFeatureHub {
    link: Arc<AgentLink>,
    queue: SyncSender<Job>,
    next_id: AtomicU64,
    state: Mutex<HubState>,
    clipboard: watch::Sender<u64>,
}

impl std::fmt::Debug for AgentFeatureHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentFeatureHub").finish_non_exhaustive()
    }
}

impl AgentFeatureHub {
    /// A hub over `link`; starts the worker thread that sends queued updates.
    pub fn new(link: Arc<AgentLink>) -> Arc<Self> {
        let (tx, rx) = sync_channel::<Job>(QUEUE);
        let worker_link = link.clone();
        let spawned = std::thread::Builder::new()
            .name("tlp-svc-features".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    job(&worker_link);
                }
            });
        if let Err(e) = spawned {
            tracing::error!(%e, "cannot start the feature worker; overlays and markers are off");
        }
        let (clipboard, _) = watch::channel(0);
        Arc::new(Self {
            link,
            queue: tx,
            next_id: AtomicU64::new(0),
            state: Mutex::default(),
            clipboard,
        })
    }

    /// The link this hub talks through.
    pub fn link(&self) -> &Arc<AgentLink> {
        &self.link
    }

    /// A fresh id for an agent object or offer.
    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn enqueue(&self, job: Job) {
        match self.queue.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                tracing::warn!("agent feature queue full; update dropped")
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    fn enqueue_request(&self, req: FeatureRequest) {
        self.enqueue(Box::new(move |link| call_logged(link, &req)));
    }

    /// An event from the agent's events connection (desktop events are handled elsewhere).
    pub fn on_event(&self, ev: AgentEvent) {
        match ev {
            AgentEvent::Desktop { .. } => {}
            AgentEvent::ClipboardChanged { seq } => {
                self.clipboard.send_replace(seq);
            }
            AgentEvent::LocalInput { keyboard, pointer } => {
                let now = Instant::now();
                let mut st = lock(&self.state);
                st.stamps.retain(|w| w.strong_count() > 0);
                for s in st.stamps.iter().filter_map(Weak::upgrade) {
                    if keyboard {
                        s.record_keyboard(false, now);
                    }
                    if pointer {
                        s.record_pointer(false, now);
                    }
                }
            }
            AgentEvent::Shield { id, signal } => {
                let f = lock(&self.state).shields.remove(&id);
                if let Some(f) = f {
                    tracing::info!(id, ?signal, "privacy shield in the agent turned off");
                    f(shield_signal(signal));
                }
            }
            AgentEvent::OverlayClear { id } => {
                let f = lock(&self.state).overlays.get(&id).cloned();
                if let Some(f) = f {
                    f();
                }
            }
            AgentEvent::StageBegin { offer } => self.stage(offer, true),
            AgentEvent::StageCancel { offer } => self.stage(offer, false),
        }
    }

    fn stage(&self, offer: u64, begin: bool) {
        let f = lock(&self.state).stages.get(&offer).cloned();
        if let Some(f) = f {
            f(begin);
        }
    }

    /// The service is now connected to a new agent (or lost it): restart the hooks there and
    /// turn off privacy mode, whose shield died with the old agent.
    pub fn agent_changed(&self) {
        let (restart, shields) = {
            let mut st = lock(&self.state);
            let shields: Vec<ShieldSignalFn> = st.shields.drain().map(|(_, f)| f).collect();
            (st.activity_users > 0, shields)
        };
        if restart {
            self.enqueue_request(FeatureRequest::ActivityStart);
        }
        for f in shields {
            f(ShieldSignal::Failed);
        }
    }

    // ----- local input (local priority) ---------------------------------------------------

    /// [`HostConfig::local_activity_factory`](crate::HostConfig::local_activity_factory):
    /// physical input reported by the agent's hooks, which run while at least one returned
    /// handle is alive.
    pub fn local_activity_factory(self: &Arc<Self>) -> LocalActivityFactory {
        let hub = Arc::downgrade(self);
        Arc::new(move || {
            let hub = hub.upgrade().ok_or(InputError::Unsupported)?;
            Ok(hub.acquire_activity() as Arc<dyn LocalActivity>)
        })
    }

    fn acquire_activity(self: &Arc<Self>) -> Arc<AgentActivity> {
        let stamps = Arc::new(ActivityStamp::new());
        let start = {
            let mut st = lock(&self.state);
            st.stamps.push(Arc::downgrade(&stamps));
            st.activity_users = st.activity_users.saturating_add(1);
            st.activity_users == 1
        };
        if start {
            self.enqueue_request(FeatureRequest::ActivityStart);
        }
        Arc::new(AgentActivity {
            stamps,
            hub: Arc::downgrade(self),
        })
    }

    fn release_activity(&self) {
        let stop = {
            let mut st = lock(&self.state);
            st.activity_users = st.activity_users.saturating_sub(1);
            st.activity_users == 0
        };
        if stop {
            self.enqueue_request(FeatureRequest::ActivityStop);
        }
    }

    // ----- pointer marker -------------------------------------------------------------------

    /// [`HostConfig::pointer_marker_factory`](crate::HostConfig::pointer_marker_factory): the
    /// marker window lives in the agent.
    pub fn pointer_marker_factory(self: &Arc<Self>) -> PointerMarkerFactory {
        let hub = Arc::downgrade(self);
        Arc::new(move |label: &str| {
            let hub = hub.upgrade().ok_or(InputError::Unsupported)?;
            Ok(Box::new(AgentMarker::new(hub, label)) as Box<dyn PointerMarker>)
        })
    }

    // ----- whiteboard overlay ---------------------------------------------------------------

    /// The whiteboard's [`OverlayFactory`]: strokes are mirrored to an overlay window in the
    /// agent; its clear badge calls `on_clear` here.
    pub fn overlay_factory(self: &Arc<Self>) -> OverlayFactory {
        let hub = Arc::downgrade(self);
        Arc::new(move |board: SharedBoard, on_clear: ClearRequestFn| {
            let hub = hub.upgrade().ok_or(InputError::Unsupported)?;
            Ok(Box::new(AgentOverlay::new(hub, board, on_clear)) as Box<dyn WhiteboardOverlay>)
        })
    }

    // ----- privacy shield -------------------------------------------------------------------

    /// The privacy feature's [`ShieldFactory`]: the shield covers the screens in the agent.
    pub fn shield_factory(self: &Arc<Self>) -> ShieldFactory {
        let hub = Arc::downgrade(self);
        Arc::new(move |on_signal: ShieldSignalFn| {
            let hub = hub.upgrade().ok_or(InputError::Unsupported)?;
            let id = hub.next_id();
            Ok(Box::new(AgentShield {
                id,
                hub,
                engaged: false,
                on_signal,
            }) as Box<dyn PrivacyShield>)
        })
    }

    // ----- clipboard ------------------------------------------------------------------------

    /// The user's clipboard content (blocking; call from a blocking context).
    pub fn clipboard_read(&self) -> Result<Option<ClipData>, HostError> {
        self.link.feature(|c| {
            ipc::write_msg(c, &FeatureRequest::ClipboardRead)?;
            match ipc::read_msg::<_, FeatureReply>(c)? {
                FeatureReply::Clip(None) => Ok(Ok(None)),
                FeatureReply::Clip(Some(h)) => match ipc::read_clip_payload(c, h) {
                    Ok(clip) => Ok(Ok(Some(clip))),
                    Err(HostError::IpcRefused(why)) => Ok(Err(HostError::IpcRefused(why))),
                    Err(e) => Err(e),
                },
                FeatureReply::Error(e) => Ok(Err(HostError::IpcRefused(format!("{e:?}")))),
                other => Ok(Err(HostError::IpcRefused(format!("unexpected {other:?}")))),
            }
        })?
    }

    /// Replaces the user's clipboard; returns its change number after the write.
    pub fn clipboard_write(&self, clip: &ClipData) -> Result<u64, HostError> {
        let header = clip.header();
        let payload = clip.payload();
        let reply = self.link.feature(|c| {
            ipc::write_msg_with_payload(c, &FeatureRequest::ClipboardWrite(header), &payload)?;
            ipc::read_msg::<_, FeatureReply>(c)
        })?;
        match reply {
            FeatureReply::ClipboardSeq(seq) => Ok(seq),
            FeatureReply::Error(e) => Err(HostError::IpcRefused(format!("{e:?}"))),
            other => Err(HostError::IpcRefused(format!("unexpected {other:?}"))),
        }
    }

    /// The files copied in Explorer (`None` without files or without an agent).
    pub fn clipboard_files(&self) -> Option<Vec<String>> {
        match call(&self.link, &FeatureRequest::ClipboardFiles) {
            Ok(FeatureReply::Files(f)) => f,
            _ => None,
        }
    }

    /// Change notifications of the user's clipboard (its change number).
    pub fn clipboard_changes(&self) -> watch::Receiver<u64> {
        self.clipboard.subscribe()
    }

    // ----- Explorer paste -------------------------------------------------------------------

    /// Puts virtual files on the user's clipboard (queued); `on_stage` hears when Explorer
    /// starts pasting (`true`) and when the offer leaves the clipboard (`false`).
    pub fn shell_offer(
        &self,
        offer: u64,
        dir: String,
        items: Vec<TransferItem>,
        on_stage: StageHandler,
    ) {
        lock(&self.state).stages.insert(offer, on_stage);
        self.enqueue_request(FeatureRequest::ShellOffer { offer, dir, items });
    }

    /// Tells the agent how far the pull of `offer` has come (queued).
    pub fn shell_stage(&self, offer: u64, state: WireStage) {
        self.enqueue_request(FeatureRequest::ShellStage { offer, state });
    }

    /// Takes `offer` off the user's clipboard (queued).
    pub fn shell_clear(&self, offer: u64) {
        lock(&self.state).stages.remove(&offer);
        self.enqueue_request(FeatureRequest::ShellClear { offer });
    }

    // ----- audio ----------------------------------------------------------------------------

    /// Opens a loopback audio stream in the agent.
    pub fn open_audio(&self) -> Result<AgentAudio, HostError> {
        let (conn, _) = self.link.open(AgentChannel::Audio)?;
        Ok(AgentAudio {
            conn,
            buf: Vec::new(),
        })
    }
}

/// Physical local input as reported by the agent's hooks.
pub struct AgentActivity {
    stamps: Arc<ActivityStamp>,
    hub: Weak<AgentFeatureHub>,
}

impl LocalActivity for AgentActivity {
    fn last_physical_input(&self) -> Option<Instant> {
        self.stamps.last_physical_input()
    }

    fn last_physical_keyboard(&self) -> Option<Instant> {
        self.stamps.last_physical_keyboard()
    }

    fn last_physical_pointer(&self) -> Option<Instant> {
        self.stamps.last_physical_pointer()
    }
}

impl Drop for AgentActivity {
    fn drop(&mut self) {
        if let Some(hub) = self.hub.upgrade() {
            hub.release_activity();
        }
    }
}

/// What a marker or overlay last told the agent.
struct Remote {
    /// The link generation the object was created in (`None`: not yet).
    opened_in: Option<u64>,
}

impl Remote {
    fn new() -> Self {
        Self { opened_in: None }
    }

    /// Whether the object must be (re-)created in the current agent.
    fn needs_open(&self, link: &AgentLink) -> bool {
        self.opened_in != Some(link.generation())
    }
}

#[derive(Clone)]
struct MarkerWanted {
    label: String,
    visible: bool,
}

/// The remote-pointer marker, drawn by the agent.
pub struct AgentMarker {
    id: u64,
    hub: Arc<AgentFeatureHub>,
    wanted: MarkerWanted,
    remote: Arc<Mutex<Remote>>,
}

impl AgentMarker {
    fn new(hub: Arc<AgentFeatureHub>, label: &str) -> Self {
        let m = Self {
            id: hub.next_id(),
            hub,
            wanted: MarkerWanted {
                label: label.to_owned(),
                visible: false,
            },
            remote: Arc::new(Mutex::new(Remote::new())),
        };
        m.sync(None);
        m
    }

    /// Queues `req` after (re-)creating the marker in the current agent if needed.
    fn sync(&self, req: Option<FeatureRequest>) {
        let (id, wanted, remote) = (self.id, self.wanted.clone(), self.remote.clone());
        self.hub.enqueue(Box::new(move |link| {
            let mut r = lock(&remote);
            if r.needs_open(link) {
                let open = FeatureRequest::MarkerOpen {
                    id,
                    label: wanted.label.clone(),
                };
                match call(link, &open) {
                    Ok(FeatureReply::Ok) => r.opened_in = Some(link.generation()),
                    Ok(other) => {
                        tracing::debug!(?other, "agent cannot show the pointer marker");
                        return;
                    }
                    Err(e) => {
                        tracing::debug!(%e, "pointer marker: no agent");
                        return;
                    }
                }
                if wanted.visible {
                    call_logged(link, &FeatureRequest::MarkerVisible { id, visible: true });
                }
                return;
            }
            if let Some(req) = req {
                call_logged(link, &req);
            }
        }));
    }
}

impl PointerMarker for AgentMarker {
    fn set_visible(&mut self, visible: bool) {
        if self.wanted.visible == visible {
            return;
        }
        self.wanted.visible = visible;
        self.sync(Some(FeatureRequest::MarkerVisible {
            id: self.id,
            visible,
        }));
    }

    fn set_label(&mut self, label: &str) {
        if self.wanted.label == label {
            return;
        }
        label.clone_into(&mut self.wanted.label);
        self.sync(Some(FeatureRequest::MarkerLabel {
            id: self.id,
            label: label.to_owned(),
        }));
    }
}

impl Drop for AgentMarker {
    fn drop(&mut self) {
        self.hub
            .enqueue_request(FeatureRequest::MarkerClose { id: self.id });
    }
}

/// What the agent's copy of a board holds: `(stroke id, points sent)` in board order.
#[derive(Debug, Default)]
struct Sent {
    strokes: Vec<(u64, usize)>,
}

/// The new strokes and points of `board` since `sent` (all of it with `reset` when a stroke
/// went away: clear, eviction, eraser), split into requests of at most
/// [`OVERLAY_POINTS_PER_REQUEST`] points. Updates `sent`.
fn board_delta(
    strokes: &[tlp_desk_input::whiteboard::Stroke],
    sent: &mut Sent,
) -> (bool, Vec<Vec<WireStroke>>) {
    let still_there = sent
        .strokes
        .iter()
        .all(|(id, n)| strokes.iter().any(|s| s.id == *id && s.points.len() >= *n));
    let reset = !still_there;
    if reset {
        sent.strokes.clear();
    }
    let mut chunks: Vec<WireStroke> = Vec::new();
    for s in strokes {
        let have = match sent.strokes.iter_mut().find(|(id, _)| *id == s.id) {
            Some((_, n)) => {
                let have = *n;
                *n = s.points.len();
                have
            }
            None => {
                sent.strokes.push((s.id, s.points.len()));
                0
            }
        };
        let new = s.points.get(have..).unwrap_or_default();
        for piece in new.chunks(MAX_POINTS_PER_STROKE.min(OVERLAY_POINTS_PER_REQUEST)) {
            chunks.push(WireStroke {
                id: s.id,
                color: s.color.0,
                width: s.width,
                points: piece.to_vec(),
            });
        }
    }
    // Pack chunks into requests by point count.
    let mut requests: Vec<Vec<WireStroke>> = Vec::new();
    let mut points = 0usize;
    for c in chunks {
        let n = c.points.len();
        match requests.last_mut() {
            Some(last) if points.saturating_add(n) <= OVERLAY_POINTS_PER_REQUEST => {
                last.push(c);
                points = points.saturating_add(n);
            }
            _ => {
                requests.push(vec![c]);
                points = n;
            }
        }
    }
    (reset, requests)
}

/// A whiteboard overlay mirrored into the agent.
pub struct AgentOverlay {
    id: u64,
    hub: Arc<AgentFeatureHub>,
    board: SharedBoard,
    target: Arc<Mutex<Option<MonitorRect>>>,
    sent: Arc<Mutex<Sent>>,
    remote: Arc<Mutex<Remote>>,
    pending: Arc<AtomicBool>,
}

impl AgentOverlay {
    fn new(hub: Arc<AgentFeatureHub>, board: SharedBoard, on_clear: ClearRequestFn) -> Self {
        let id = hub.next_id();
        lock(&hub.state).overlays.insert(id, on_clear);
        let o = Self {
            id,
            hub,
            board,
            target: Arc::default(),
            sent: Arc::default(),
            remote: Arc::new(Mutex::new(Remote::new())),
            pending: Arc::new(AtomicBool::new(false)),
        };
        o.schedule();
        o
    }

    /// Queues one sync of target and strokes (coalesced: at most one queued at a time).
    fn schedule(&self) {
        if self.pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let id = self.id;
        let (board, target, sent, remote, pending) = (
            self.board.clone(),
            self.target.clone(),
            self.sent.clone(),
            self.remote.clone(),
            self.pending.clone(),
        );
        self.hub.enqueue(Box::new(move |link| {
            pending.store(false, Ordering::Release);
            let mut r = lock(&remote);
            let mut sent = lock(&sent);
            let mut reset = false;
            if r.needs_open(link) {
                match call(link, &FeatureRequest::OverlayOpen { id }) {
                    Ok(FeatureReply::Ok) => {
                        r.opened_in = Some(link.generation());
                        sent.strokes.clear();
                        reset = true;
                        if let Some(rect) = *lock(&target) {
                            call_logged(
                                link,
                                &FeatureRequest::OverlayTarget {
                                    id,
                                    rect: wire_rect(rect),
                                },
                            );
                        }
                    }
                    Ok(other) => {
                        tracing::debug!(?other, "agent cannot show the whiteboard");
                        return;
                    }
                    Err(e) => {
                        tracing::debug!(%e, "whiteboard overlay: no agent");
                        return;
                    }
                }
            }
            let (cleared, requests) = {
                let b = lock_board(&board);
                board_delta(b.strokes(), &mut sent)
            };
            let mut reset = reset || cleared;
            if requests.is_empty() && reset {
                call_logged(
                    link,
                    &FeatureRequest::OverlayStrokes {
                        id,
                        reset: true,
                        strokes: Vec::new(),
                    },
                );
            }
            for strokes in requests {
                call_logged(link, &FeatureRequest::OverlayStrokes { id, reset, strokes });
                reset = false;
            }
        }));
    }
}

impl WhiteboardOverlay for AgentOverlay {
    fn set_target(&mut self, rect: MonitorRect) {
        *lock(&self.target) = Some(rect);
        let (id, remote) = (self.id, self.remote.clone());
        self.hub.enqueue(Box::new(move |link| {
            // Before the overlay exists, the target goes out with the open.
            if !lock(&remote).needs_open(link) {
                call_logged(
                    link,
                    &FeatureRequest::OverlayTarget {
                        id,
                        rect: wire_rect(rect),
                    },
                );
            }
        }));
        self.schedule();
    }

    fn changed(&mut self) {
        self.schedule();
    }
}

impl Drop for AgentOverlay {
    fn drop(&mut self) {
        lock(&self.hub.state).overlays.remove(&self.id);
        self.hub
            .enqueue_request(FeatureRequest::OverlayClose { id: self.id });
    }
}

/// A privacy shield in the agent.
pub struct AgentShield {
    id: u64,
    hub: Arc<AgentFeatureHub>,
    engaged: bool,
    on_signal: ShieldSignalFn,
}

impl PrivacyShield for AgentShield {
    fn engage(&mut self, text: &PrivacyScreenText) -> Result<(), InputError> {
        let req = FeatureRequest::ShieldEngage {
            id: self.id,
            lines: screen_lines(text),
        };
        match call(&self.hub.link, &req) {
            Ok(FeatureReply::Ok) => {
                self.engaged = true;
                lock(&self.hub.state)
                    .shields
                    .insert(self.id, self.on_signal.clone());
                Ok(())
            }
            Ok(FeatureReply::Error(e)) => Err(wire_input(e)),
            Ok(other) => Err(InputError::Platform(format!("agent: unexpected {other:?}"))),
            Err(e) => Err(InputError::Platform(e.to_string())),
        }
    }

    fn set_text(&mut self, text: &PrivacyScreenText) {
        if self.engaged {
            self.hub.enqueue_request(FeatureRequest::ShieldText {
                id: self.id,
                lines: screen_lines(text),
            });
        }
    }

    fn release(&mut self) {
        if !self.engaged {
            return;
        }
        self.engaged = false;
        lock(&self.hub.state).shields.remove(&self.id);
        // Wait for it: the screen must be uncovered when this returns.
        call_logged(
            &self.hub.link,
            &FeatureRequest::ShieldRelease { id: self.id },
        );
    }

    fn is_engaged(&self) -> bool {
        self.engaged
    }
}

impl Drop for AgentShield {
    fn drop(&mut self) {
        self.release();
    }
}

/// Loopback audio streamed from the agent.
pub struct AgentAudio {
    conn: Box<dyn AgentConn>,
    buf: Vec<u8>,
}

impl std::fmt::Debug for AgentAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentAudio").finish_non_exhaustive()
    }
}

impl AgentAudio {
    /// Blocks for the next packet: `Some((rate, channels, samples))`, or `None` for a
    /// keep-alive. An error means the agent stopped streaming.
    pub fn next_packet(&mut self) -> Result<Option<(u32, u16, Vec<f32>)>, HostError> {
        let hdr: AudioPacket = ipc::read_msg(&mut self.conn)?;
        ipc::read_payload_into(&mut self.conn, hdr.payload_len, &mut self.buf)?;
        if hdr.payload_len == 0 {
            return Ok(None);
        }
        if hdr.rate == 0 || hdr.channels == 0 || self.buf.len() % 4 != 0 {
            return Err(HostError::IpcRefused("malformed audio packet".into()));
        }
        let samples = self
            .buf
            .chunks_exact(4)
            .filter_map(|b| <[u8; 4]>::try_from(b).ok())
            .map(f32::from_le_bytes)
            .collect();
        Ok(Some((hdr.rate, hdr.channels, samples)))
    }
}

#[cfg(test)]
mod tests;
