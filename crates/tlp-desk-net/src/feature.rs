//! Plug-in point for session features (files, clipboard, chat, audio, tunnels, ...).
//!
//! Each feature lane implements [`Feature`] in its own module/crate and is registered once in
//! a [`FeatureSet`] (`HostConfig::features` / `ViewerConfig::features`). For every session the
//! host and viewer crates start a [`FeatureDispatch`], which
//!
//! * calls [`Feature::start`] with a [`FeatureCtx`] (the shared [`Session`], live permissions,
//!   an event sink to the UI) and a private inbox,
//! * routes every control [`Message`] the core does not handle itself to the first feature
//!   whose [`Feature::handles`] returns `true` (the host checks
//!   `Message::required_permissions` before routing),
//! * closes all inboxes when the session ends (dropping the dispatch), which is the feature's
//!   signal to stop.
//!
//! Features open and accept their own side streams with [`Session::open_uni`],
//! [`Session::open_bi`] and [`Session::accept_side_stream`] and must check
//! [`FeatureCtx::permissions`] before serving them. This way a new feature adds files instead
//! of editing the host/viewer message loops.

use std::any::Any;
use std::sync::Arc;

use tlp_desk_proto::{Message, Permissions, Role};
use tokio::sync::{mpsc, watch};

use crate::Session;

/// Something a feature wants the UI to know (progress, a chat line, a transfer request...).
/// The payload type is owned by the feature; the UI downcasts it after matching `feature`.
#[derive(Clone)]
pub struct FeatureEvent {
    /// [`Feature::name`] of the sender.
    pub feature: &'static str,
    /// [`FeatureCtx::session_key`] of the session it concerns.
    pub session_key: u64,
    pub payload: Arc<dyn Any + Send + Sync>,
}

impl FeatureEvent {
    pub fn new<T: Any + Send + Sync>(feature: &'static str, session_key: u64, payload: T) -> Self {
        Self {
            feature,
            session_key,
            payload: Arc::new(payload),
        }
    }

    /// The payload as `T`, if that is its type.
    pub fn payload<T: Any>(&self) -> Option<&T> {
        self.payload.downcast_ref::<T>()
    }
}

impl std::fmt::Debug for FeatureEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeatureEvent")
            .field("feature", &self.feature)
            .field("session_key", &self.session_key)
            .finish_non_exhaustive()
    }
}

impl PartialEq for FeatureEvent {
    /// Same sender, same session and the very same payload allocation.
    fn eq(&self, other: &Self) -> bool {
        self.feature == other.feature
            && self.session_key == other.session_key
            && Arc::ptr_eq(&self.payload, &other.payload)
    }
}

/// Delivers [`FeatureEvent`]s to the UI (wrapped into the host's/viewer's own event enum).
pub type FeatureEventSink = Arc<dyn Fn(FeatureEvent) + Send + Sync>;

/// Everything a feature gets for one session.
#[derive(Clone)]
pub struct FeatureCtx {
    /// The session: send control messages, open/accept side streams, read the peer.
    pub session: Arc<Session>,
    /// Our role in the session.
    pub role: Role,
    /// Identifies the session towards the UI and the feature's own API (host: `SessionId`,
    /// viewer: `ViewerSession::feature_key`).
    pub session_key: u64,
    /// Current permissions of the session (live; changes when the host user edits them).
    pub permissions: watch::Receiver<Permissions>,
    events: FeatureEventSink,
}

impl FeatureCtx {
    /// Sends an event to the UI.
    pub fn emit<T: Any + Send + Sync>(&self, feature: &'static str, payload: T) {
        (self.events)(FeatureEvent::new(feature, self.session_key, payload));
    }

    /// Whether the session currently holds all of `needed`.
    pub fn allowed(&self, needed: Permissions) -> bool {
        self.permissions.borrow().contains(needed)
    }
}

impl std::fmt::Debug for FeatureCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeatureCtx")
            .field("role", &self.role)
            .field("session_key", &self.session_key)
            .finish_non_exhaustive()
    }
}

/// A session feature. Implementations must not block: spawn tasks in [`Feature::start`].
pub trait Feature: Send + Sync + 'static {
    /// Short stable name, e.g. `"files"`, `"chat"` (used for routing UI events).
    fn name(&self) -> &'static str;

    /// Whether this feature handles `msg` (checked in registration order; first wins).
    fn handles(&self, msg: &Message) -> bool;

    /// Called once when a session starts. Spawn the feature's tasks here. `inbox` yields the
    /// control messages routed to this feature and ends when the session ends.
    fn start(&self, ctx: FeatureCtx, inbox: mpsc::UnboundedReceiver<Message>);
}

/// The registered features, in routing order. Cheap to clone.
#[derive(Clone, Default)]
pub struct FeatureSet(Vec<Arc<dyn Feature>>);

impl FeatureSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a feature (builder style).
    // FEATURE HOOK: the app/CLI registers each lane's feature here, e.g.
    // `FeatureSet::new().with(files.clone()).with(chat.clone())`.
    #[must_use]
    pub fn with(mut self, feature: Arc<dyn Feature>) -> Self {
        self.0.push(feature);
        self
    }

    pub fn push(&mut self, feature: Arc<dyn Feature>) {
        self.0.push(feature);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Names in routing order.
    pub fn names(&self) -> Vec<&'static str> {
        self.0.iter().map(|f| f.name()).collect()
    }
}

impl std::fmt::Debug for FeatureSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.names()).finish()
    }
}

/// Per-session routing to the started features. Dropping it ends every feature's inbox.
pub struct FeatureDispatch {
    routes: Vec<(Arc<dyn Feature>, mpsc::UnboundedSender<Message>)>,
    permissions: watch::Sender<Permissions>,
}

impl FeatureDispatch {
    /// Starts every feature of `set` for `session`.
    pub fn start(
        set: &FeatureSet,
        session: Arc<Session>,
        session_key: u64,
        permissions: Permissions,
        events: FeatureEventSink,
    ) -> Self {
        let (perm_tx, perm_rx) = watch::channel(permissions);
        let role = session.role();
        let mut routes = Vec::with_capacity(set.0.len());
        for feature in &set.0 {
            let (tx, rx) = mpsc::unbounded_channel();
            let ctx = FeatureCtx {
                session: session.clone(),
                role,
                session_key,
                permissions: perm_rx.clone(),
                events: events.clone(),
            };
            feature.start(ctx, rx);
            routes.push((feature.clone(), tx));
        }
        Self {
            routes,
            permissions: perm_tx,
        }
    }

    /// Routes `msg` to the first feature that handles it and returns its name. `Err(msg)` if
    /// none does (or that feature already stopped), so the caller can log it.
    pub fn dispatch(&self, msg: Message) -> Result<&'static str, Box<Message>> {
        match self.routes.iter().find(|(f, _)| f.handles(&msg)) {
            Some((f, tx)) => tx.send(msg).map(|()| f.name()).map_err(|e| Box::new(e.0)),
            None => Err(Box::new(msg)),
        }
    }

    /// Publishes a permission change to every feature.
    pub fn set_permissions(&self, permissions: Permissions) {
        self.permissions.send_replace(permissions);
    }

    /// Number of started features.
    pub fn len(&self) -> usize {
        self.routes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }
}

impl std::fmt::Debug for FeatureDispatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.routes.iter().map(|(f, _)| f.name()))
            .finish()
    }
}
