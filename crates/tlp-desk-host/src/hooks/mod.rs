//! Host-wide hooks shared by every viewer session.
//!
//! The local-activity monitor (low-level keyboard/mouse hooks that tell whether the host user
//! is working) and the green remote-pointer marker are OS-global: one set serves the whole
//! machine. With two viewers connected, creating one set *per session* would install every
//! hook twice and draw two markers over each other. [`SharedHooks`] creates each lazily on
//! first use and reference-counts it:
//!
//! * [`ActivityLease`]: the monitor lives while at least one session holds a lease.
//! * [`MarkerLease`]: one marker window; it is visible while any session's viewer is moving the
//!   pointer, and its label names those viewers (`Ali, Sara`; one name when only one moves).
//!   Each session still hides only its own part, so one viewer going idle never hides the
//!   marker of the other.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use tlp_desk_input::{LocalActivity, PointerMarker};

use crate::{LocalActivityFactory, PointerMarkerFactory};

/// Names shown on the marker at most (further movers are summarized by the first names only).
const MAX_LABEL_NAMES: usize = 3;

/// Creates and counts the machine-wide activity monitor and pointer marker.
pub(crate) struct SharedHooks {
    activity_factory: LocalActivityFactory,
    marker_factory: PointerMarkerFactory,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    activity: Option<Arc<dyn LocalActivity>>,
    activity_users: usize,
    marker: Option<Box<dyn PointerMarker>>,
    /// Sessions holding a marker lease, with the name each shows.
    marker_users: BTreeMap<u64, String>,
    /// Sessions whose viewer is moving the pointer now.
    moving: Vec<u64>,
    shown: bool,
    label: String,
}

fn lock(m: &Mutex<State>) -> MutexGuard<'_, State> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl SharedHooks {
    pub(crate) fn new(
        activity_factory: LocalActivityFactory,
        marker_factory: PointerMarkerFactory,
    ) -> Arc<Self> {
        Arc::new(Self {
            activity_factory,
            marker_factory,
            state: Mutex::new(State::default()),
        })
    }

    /// Takes a share of the activity monitor, starting it if no session has one yet. `None` if
    /// it cannot be started (local priority is then unavailable for this session).
    pub(crate) fn activity(self: &Arc<Self>) -> Option<ActivityLease> {
        let mut st = lock(&self.state);
        let activity = match &st.activity {
            Some(a) => Arc::clone(a),
            None => match (self.activity_factory)() {
                Ok(a) => {
                    tracing::debug!("local activity monitor started");
                    st.activity = Some(Arc::clone(&a));
                    a
                }
                Err(e) => {
                    tracing::warn!(%e, "local input priority unavailable");
                    return None;
                }
            },
        };
        st.activity_users += 1;
        Some(ActivityLease {
            hooks: Arc::clone(self),
            activity,
        })
    }

    /// Takes part in the shared marker as `session`, showing `name`. Creates the marker
    /// window on first use. `None` if the marker cannot be created.
    pub(crate) fn marker(self: &Arc<Self>, session: u64, name: &str) -> Option<MarkerLease> {
        let mut st = lock(&self.state);
        if st.marker.is_none() {
            match (self.marker_factory)(name) {
                Ok(m) => {
                    if !m.excluded_from_capture() {
                        tracing::warn!("pointer marker could not be excluded from capture; the viewer may see it");
                    }
                    st.label = name.to_owned();
                    st.marker = Some(m);
                }
                Err(e) => {
                    tracing::warn!(%e, "remote pointer marker unavailable");
                    return None;
                }
            }
        }
        st.marker_users.insert(session, name.to_owned());
        Some(MarkerLease {
            hooks: Arc::clone(self),
            session,
        })
    }

    /// Number of sessions currently holding the activity monitor (for tests and diagnostics).
    #[cfg(test)]
    pub(crate) fn activity_users(&self) -> usize {
        lock(&self.state).activity_users
    }

    #[cfg(test)]
    pub(crate) fn marker_exists(&self) -> bool {
        lock(&self.state).marker.is_some()
    }
}

/// Re-evaluates what the marker shows after a change of who is moving.
fn refresh(st: &mut State) {
    let mut names: Vec<&str> = Vec::new();
    for s in &st.moving {
        if let Some(n) = st.marker_users.get(s) {
            if !n.is_empty() && !names.contains(&n.as_str()) {
                names.push(n);
            }
        }
    }
    names.truncate(MAX_LABEL_NAMES);
    let label = if names.is_empty() {
        st.label.clone()
    } else {
        names.join(", ")
    };
    let want = !st.moving.is_empty();
    let Some(marker) = st.marker.as_mut() else {
        return;
    };
    if label != st.label {
        marker.set_label(&label);
        st.label = label;
    }
    if want != st.shown {
        st.shown = want;
        marker.set_visible(want);
    }
}

/// A session's share of the activity monitor. Dropping the last lease stops the monitor.
pub(crate) struct ActivityLease {
    hooks: Arc<SharedHooks>,
    activity: Arc<dyn LocalActivity>,
}

impl ActivityLease {
    pub(crate) fn get(&self) -> &Arc<dyn LocalActivity> {
        &self.activity
    }
}

impl Drop for ActivityLease {
    fn drop(&mut self) {
        let mut st = lock(&self.hooks.state);
        st.activity_users = st.activity_users.saturating_sub(1);
        if st.activity_users == 0 {
            st.activity = None;
            tracing::debug!("local activity monitor stopped");
        }
    }
}

/// A session's share of the pointer marker. Dropping the last lease removes the marker.
pub(crate) struct MarkerLease {
    hooks: Arc<SharedHooks>,
    session: u64,
}

impl MarkerLease {
    /// This session's viewer started (`true`) or stopped moving the pointer.
    pub(crate) fn set_visible(&self, visible: bool) {
        let mut st = lock(&self.hooks.state);
        let present = st.moving.contains(&self.session);
        if visible && !present {
            st.moving.push(self.session);
        } else if !visible && present {
            st.moving.retain(|s| *s != self.session);
        } else {
            return;
        }
        refresh(&mut st);
    }
}

impl Drop for MarkerLease {
    fn drop(&mut self) {
        let mut st = lock(&self.hooks.state);
        st.moving.retain(|s| *s != self.session);
        st.marker_users.remove(&self.session);
        refresh(&mut st);
        if st.marker_users.is_empty() {
            if let Some(mut m) = st.marker.take() {
                m.set_visible(false);
            }
            st.shown = false;
            st.label.clear();
        }
    }
}

#[cfg(test)]
mod tests;
