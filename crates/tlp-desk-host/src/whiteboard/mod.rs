//! Host side of the whiteboard: strokes from the viewer are drawn on the host's own screen in
//! a topmost, click-through overlay over the streamed monitor.
//!
//! * Gated by `WHITEBOARD`: the core drops messages without it, and this feature checks again
//!   and reacts to live revocation by clearing and destroying the overlay at once.
//! * The overlay window is created lazily on the first stroke (so a session that never draws
//!   costs nothing) and destroyed when the session ends.
//! * It follows the streamed monitor ([`crate::Host::streamed_monitor_fn`], polled twice a
//!   second and on every stroke); strokes are normalized, so they re-map on a switch.
//! * The host user can wipe the drawings with the overlay's clear badge; the viewer is told
//!   with `WhiteboardClear`.
//! * The overlay is excluded from screen capture: the viewer draws the strokes locally, which
//!   avoids a second round of capture/encode latency and keeps thin lines crisp.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tlp_desk_input::whiteboard::{
    lock_board, start_whiteboard_overlay, ClearRequestFn, SharedBoard, WhiteboardOp,
    WhiteboardOverlay, FEATURE_NAME,
};
use tlp_desk_input::{InputError, MonitorRect};
use tlp_desk_net::{Feature, FeatureCtx};
use tlp_desk_proto::{Message, Permissions};
use tokio::sync::mpsc;

use crate::StreamedMonitorFn;

/// Creates the overlay for a session's board (default:
/// [`tlp_desk_input::whiteboard::start_whiteboard_overlay`]). Tests pass a fake that never
/// touches the screen.
pub type OverlayFactory = Arc<
    dyn Fn(SharedBoard, ClearRequestFn) -> Result<Box<dyn WhiteboardOverlay>, InputError>
        + Send
        + Sync,
>;

/// How often the overlay re-checks which monitor is streamed.
const FOLLOW_MONITOR: Duration = Duration::from_millis(500);

/// What the host whiteboard reports to the UI (payload of `HostEvent::Feature` with
/// `feature == tlp_desk_input::whiteboard::FEATURE_NAME`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhiteboardHostEvent {
    /// The viewer started drawing on this screen (first stroke on an empty board).
    Drawing,
    /// The drawings were wiped; `by_host`: the local user clicked the clear badge.
    Cleared { by_host: bool },
}

/// The host's whiteboard feature. Register it in `HostConfig::features`, then hand it
/// [`crate::Host::streamed_monitor_fn`] once the host runs.
pub struct WhiteboardHost {
    factory: OverlayFactory,
    monitors: Arc<Mutex<Option<StreamedMonitorFn>>>,
}

impl std::fmt::Debug for WhiteboardHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WhiteboardHost").finish_non_exhaustive()
    }
}

impl Default for WhiteboardHost {
    fn default() -> Self {
        Self::new()
    }
}

impl WhiteboardHost {
    /// With the platform overlay window.
    pub fn new() -> Self {
        Self::with_overlay_factory(Arc::new(start_whiteboard_overlay))
    }

    /// With a custom overlay (tests, other platforms).
    pub fn with_overlay_factory(factory: OverlayFactory) -> Self {
        Self {
            factory,
            monitors: Arc::default(),
        }
    }

    /// Tells the feature where each session's streamed monitor is (usually
    /// [`crate::Host::streamed_monitor_fn`]). Without it the overlay has no target and stays
    /// hidden.
    pub fn set_monitor_source(&self, source: StreamedMonitorFn) {
        *self.monitors.lock().unwrap_or_else(PoisonError::into_inner) = Some(source);
    }
}

impl Feature for WhiteboardHost {
    fn name(&self) -> &'static str {
        FEATURE_NAME
    }

    fn handles(&self, msg: &Message) -> bool {
        matches!(
            msg,
            Message::WhiteboardStroke { .. } | Message::WhiteboardClear
        )
    }

    fn start(&self, ctx: FeatureCtx, inbox: mpsc::UnboundedReceiver<Message>) {
        let (clear_tx, clear_rx) = mpsc::unbounded_channel();
        let state = SessionBoard {
            ctx,
            board: SharedBoard::default(),
            overlay: None,
            overlay_failed: false,
            target: None,
            factory: self.factory.clone(),
            monitors: self.monitors.clone(),
            clear_tx,
        };
        tokio::spawn(state.run(inbox, clear_rx));
    }
}

/// One session's whiteboard on the host.
struct SessionBoard {
    ctx: FeatureCtx,
    board: SharedBoard,
    overlay: Option<Box<dyn WhiteboardOverlay>>,
    /// Creating the overlay failed once; do not retry on every stroke.
    overlay_failed: bool,
    target: Option<MonitorRect>,
    factory: OverlayFactory,
    monitors: Arc<Mutex<Option<StreamedMonitorFn>>>,
    clear_tx: mpsc::UnboundedSender<()>,
}

impl SessionBoard {
    async fn run(
        mut self,
        mut inbox: mpsc::UnboundedReceiver<Message>,
        mut clear_rx: mpsc::UnboundedReceiver<()>,
    ) {
        let mut perms = self.ctx.permissions.clone();
        let mut follow = tokio::time::interval(FOLLOW_MONITOR);
        follow.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                msg = inbox.recv() => match msg {
                    Some(msg) => self.on_message(&msg).await,
                    // The session ended.
                    None => break,
                },
                Some(()) = clear_rx.recv() => self.on_host_clear().await,
                changed = perms.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    if !self.allowed() {
                        self.revoke().await;
                    }
                }
                _ = follow.tick(), if self.overlay.is_some() => self.retarget(),
            }
        }
        self.drop_overlay().await;
    }

    fn allowed(&self) -> bool {
        self.ctx.allowed(Permissions::WHITEBOARD)
    }

    async fn on_message(&mut self, msg: &Message) {
        if !self.allowed() {
            tracing::debug!(
                session = self.ctx.session_key,
                "whiteboard message without WHITEBOARD; dropped"
            );
            return;
        }
        let Some(op) = WhiteboardOp::from_message(msg) else {
            tracing::debug!(
                session = self.ctx.session_key,
                "empty whiteboard stroke; ignored"
            );
            return;
        };
        if matches!(op, WhiteboardOp::Stroke(_)) {
            self.ensure_overlay().await;
        }
        let was_empty = {
            let mut board = lock_board(&self.board);
            let was_empty = board.is_empty();
            board.apply(&op);
            was_empty
        };
        match op {
            WhiteboardOp::Stroke(_) if was_empty => {
                tracing::info!(
                    session = self.ctx.session_key,
                    "viewer is drawing on this screen"
                );
                self.ctx.emit(FEATURE_NAME, WhiteboardHostEvent::Drawing);
            }
            WhiteboardOp::Clear if !was_empty => {
                self.ctx.emit(
                    FEATURE_NAME,
                    WhiteboardHostEvent::Cleared { by_host: false },
                );
            }
            _ => {}
        }
        self.retarget();
        if let Some(o) = self.overlay.as_mut() {
            o.changed();
        }
    }

    /// The host user clicked the overlay's clear badge.
    async fn on_host_clear(&mut self) {
        lock_board(&self.board).clear();
        if let Some(o) = self.overlay.as_mut() {
            o.changed();
        }
        tracing::info!(
            session = self.ctx.session_key,
            "host user cleared the whiteboard"
        );
        self.ctx
            .emit(FEATURE_NAME, WhiteboardHostEvent::Cleared { by_host: true });
        if let Err(e) = self.ctx.session.send(&Message::WhiteboardClear).await {
            tracing::debug!(%e, "could not tell the viewer about the clear");
        }
    }

    /// `WHITEBOARD` was revoked: wipe and remove the overlay now.
    async fn revoke(&mut self) {
        tracing::info!(
            session = self.ctx.session_key,
            "whiteboard permission revoked; removing the drawings"
        );
        lock_board(&self.board).clear();
        self.drop_overlay().await;
        self.target = None;
    }

    async fn ensure_overlay(&mut self) {
        if self.overlay.is_some() || self.overlay_failed {
            return;
        }
        let factory = self.factory.clone();
        let board = self.board.clone();
        let tx = self.clear_tx.clone();
        let on_clear: ClearRequestFn = Arc::new(move || {
            let _ = tx.send(());
        });
        // Creating the window waits for its thread: keep that off the async workers.
        match tokio::task::spawn_blocking(move || factory(board, on_clear)).await {
            Ok(Ok(overlay)) => {
                if !overlay.excluded_from_capture() {
                    tracing::warn!("whiteboard overlay cannot be excluded from capture; strokes may also show in the video");
                }
                self.overlay = Some(overlay);
            }
            Ok(Err(e)) => {
                tracing::warn!(%e, "cannot show the whiteboard on this screen");
                self.overlay_failed = true;
            }
            Err(e) => {
                tracing::warn!(%e, "whiteboard overlay creation failed");
                self.overlay_failed = true;
            }
        }
    }

    /// Points the overlay at the currently streamed monitor.
    fn retarget(&mut self) {
        let source = self
            .monitors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let rect = source.and_then(|f| f(self.ctx.session_key));
        if let (Some(rect), Some(o)) = (rect, self.overlay.as_mut()) {
            if self.target != Some(rect) {
                self.target = Some(rect);
                o.set_target(rect);
            }
        }
    }

    async fn drop_overlay(&mut self) {
        if let Some(o) = self.overlay.take() {
            // Dropping joins the window thread; keep that off the async workers.
            let _ = tokio::task::spawn_blocking(move || drop(o)).await;
        }
    }
}
