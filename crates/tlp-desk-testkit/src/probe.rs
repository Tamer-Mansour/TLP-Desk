//! A feature that records what is routed to it.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tlp_desk_net::{Feature, FeatureCtx};
use tlp_desk_proto::{Message, Permissions};
use tokio::sync::mpsc;

type Filter = Arc<dyn Fn(&Message) -> bool + Send + Sync>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Claims the control messages its filter accepts and records them, plus the session's live
/// permissions. Register it in `HostConfig::features` (or `ViewerConfig::features`) to see
/// exactly what the core lets through to features.
pub struct ProbeFeature {
    name: &'static str,
    filter: Filter,
    received: Arc<Mutex<Vec<Message>>>,
    permissions: Arc<Mutex<Option<Permissions>>>,
    started: AtomicUsize,
    running: Arc<AtomicUsize>,
}

impl std::fmt::Debug for ProbeFeature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProbeFeature")
            .field("name", &self.name)
            .field("received", &lock(&self.received).len())
            .finish_non_exhaustive()
    }
}

impl ProbeFeature {
    /// Claims every message the core hands to features.
    pub fn all(name: &'static str) -> Arc<Self> {
        Self::matching(name, |_| true)
    }

    /// Claims the messages `filter` accepts.
    pub fn matching(
        name: &'static str,
        filter: impl Fn(&Message) -> bool + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            name,
            filter: Arc::new(filter),
            received: Arc::default(),
            permissions: Arc::default(),
            started: AtomicUsize::new(0),
            running: Arc::default(),
        })
    }

    /// Messages received so far, in order, across sessions.
    pub fn received(&self) -> Vec<Message> {
        lock(&self.received).clone()
    }

    /// Forgets the received messages.
    pub fn clear(&self) {
        lock(&self.received).clear();
    }

    /// The permissions the latest session last published to features.
    pub fn permissions(&self) -> Option<Permissions> {
        *lock(&self.permissions)
    }

    /// Sessions this feature was started for.
    pub fn started(&self) -> usize {
        self.started.load(Ordering::Acquire)
    }

    /// Sessions whose inbox is still open (0 once every session ended).
    pub fn running(&self) -> usize {
        self.running.load(Ordering::Acquire)
    }
}

impl Feature for ProbeFeature {
    fn name(&self) -> &'static str {
        self.name
    }

    fn handles(&self, msg: &Message) -> bool {
        (self.filter)(msg)
    }

    fn start(&self, ctx: FeatureCtx, mut inbox: mpsc::UnboundedReceiver<Message>) {
        self.started.fetch_add(1, Ordering::AcqRel);
        self.running.fetch_add(1, Ordering::AcqRel);
        let received = self.received.clone();
        let perms = self.permissions.clone();
        let running = self.running.clone();
        let mut watch = ctx.permissions.clone();
        *lock(&perms) = Some(*watch.borrow());
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    m = inbox.recv() => match m {
                        Some(m) => lock(&received).push(m),
                        None => break,
                    },
                    changed = watch.changed() => {
                        if changed.is_err() {
                            // Dispatch dropped: drain what is left, then stop.
                            while let Some(m) = inbox.recv().await {
                                lock(&received).push(m);
                            }
                            break;
                        }
                        *lock(&perms) = Some(*watch.borrow());
                    }
                }
            }
            running.fetch_sub(1, Ordering::AcqRel);
        });
    }
}
