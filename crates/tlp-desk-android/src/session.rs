//! Connection controller: dials a host on a background tokio runtime and exposes a small state
//! the UI polls every frame.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tlp_desk_crypto::{HostKey, Identity};
use tlp_desk_net::{Discovery, DiscoveryConfig, Endpoint, EndpointConfig};
use tlp_desk_viewer::{
    AuthMethod, ConnectTarget, EndReason, KnownHostsStore, PixelFormat, RepaintFn, Viewer,
    ViewerConfig, ViewerError, ViewerEvent, ViewerSession,
};

/// What the connect screen shows.
#[derive(Debug, Clone)]
pub enum ConnState {
    /// Nothing started.
    Idle,
    /// Dialing; `awaiting_accept` once the host user is being asked.
    Connecting {
        /// The host user was asked and has not answered yet.
        awaiting_accept: bool,
    },
    /// A session is running (see [`Controller::session`]).
    Connected,
    /// The attempt failed or the session ended.
    Failed {
        /// Readable reason (logical order; shape for display).
        message: String,
        /// Set when the host key changed: the user may trust the new key.
        changed_key: Option<HostKey>,
    },
}

/// Errors creating the controller.
#[derive(Debug, thiserror::Error)]
pub enum ControllerError {
    /// The async runtime could not start.
    #[error("cannot start the network runtime: {0}")]
    Runtime(std::io::Error),
}

struct Inner {
    state: ConnState,
    session: Option<Arc<ViewerSession>>,
    viewer: Option<Arc<Viewer>>,
    /// Bumped by every connect/disconnect so a stale attempt cannot overwrite newer state.
    generation: u64,
}

/// Owns the runtime and the connection state.
pub struct Controller {
    rt: tokio::runtime::Runtime,
    data_dir: PathBuf,
    repaint: RepaintFn,
    inner: Arc<Mutex<Inner>>,
}

fn lock(m: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Chooses the authentication from what the user typed: empty asks the host user to accept, six
/// digits (spaces allowed) is the one-time code, anything else the permanent password.
pub fn auth_from_secret(secret: &str) -> AuthMethod {
    let s = secret.trim();
    if s.is_empty() {
        return AuthMethod::AcceptPrompt;
    }
    let digits: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if digits.len() == 6 && digits.chars().all(|c| c.is_ascii_digit()) {
        AuthMethod::OneTimeCode(digits)
    } else {
        AuthMethod::Password(s.to_owned())
    }
}

impl Controller {
    /// Creates the controller; `data_dir` holds the device identity and known hosts (the app's
    /// private files directory on Android). `repaint` wakes the UI on frames and events.
    pub fn new(data_dir: PathBuf, repaint: RepaintFn) -> Result<Self, ControllerError> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(ControllerError::Runtime)?;
        Ok(Self {
            rt,
            data_dir,
            repaint,
            inner: Arc::new(Mutex::new(Inner {
                state: ConnState::Idle,
                session: None,
                viewer: None,
                generation: 0,
            })),
        })
    }

    /// Current state.
    pub fn state(&self) -> ConnState {
        lock(&self.inner).state.clone()
    }

    /// The running session, if connected.
    pub fn session(&self) -> Option<Arc<ViewerSession>> {
        lock(&self.inner).session.clone()
    }

    /// Starts connecting to `address` (device ID, IP, `id@ip`) with the user's `secret`.
    pub fn connect(&self, address: &str, secret: &str) {
        let target: ConnectTarget = match address.parse() {
            Ok(t) => t,
            Err(never) => match never {},
        };
        let auth = auth_from_secret(secret);
        let generation = {
            let mut g = lock(&self.inner);
            g.generation += 1;
            g.state = ConnState::Connecting {
                awaiting_accept: false,
            };
            g.generation
        };
        let inner = self.inner.clone();
        let repaint = self.repaint.clone();
        let data_dir = self.data_dir.clone();
        self.rt.spawn(async move {
            let result = dial(&data_dir, target, auth, repaint.clone(), &inner, generation).await;
            let mut g = lock(&inner);
            if g.generation != generation {
                return;
            }
            match result {
                Ok((viewer, session, events)) => {
                    g.viewer = Some(viewer);
                    g.session = Some(session.clone());
                    g.state = ConnState::Connected;
                    drop(g);
                    watch_events(inner.clone(), generation, session, events, repaint.clone());
                }
                Err(e) => {
                    let changed_key = match &e {
                        ViewerError::HostKeyChanged { key, .. } => Some(key.clone()),
                        _ => None,
                    };
                    g.state = ConnState::Failed {
                        message: e.to_string(),
                        changed_key,
                    };
                }
            }
            repaint();
        });
    }

    /// Trusts the new key of a host whose key changed, then the user can connect again.
    pub fn forget_host_key(&self, key: &HostKey) {
        let viewer = lock(&self.inner).viewer.clone();
        if let Some(v) = viewer {
            if let Err(e) = v.forget(key) {
                tracing::warn!(%e, "cannot forget host key");
            }
        }
        lock(&self.inner).state = ConnState::Idle;
    }

    /// Ends the session (or cancels a connect) and returns to the connect screen.
    pub fn disconnect(&self) {
        let session = {
            let mut g = lock(&self.inner);
            g.generation += 1;
            g.state = ConnState::Idle;
            g.session.take()
        };
        if let Some(s) = session {
            s.disconnect();
        }
        (self.repaint)();
    }

    /// Returns from a failed/ended state to the connect screen.
    pub fn acknowledge(&self) {
        let mut g = lock(&self.inner);
        if matches!(g.state, ConnState::Failed { .. }) {
            g.state = ConnState::Idle;
        }
    }
}

async fn dial(
    data_dir: &std::path::Path,
    target: ConnectTarget,
    auth: AuthMethod,
    repaint: RepaintFn,
    inner: &Arc<Mutex<Inner>>,
    generation: u64,
) -> Result<
    (
        Arc<Viewer>,
        Arc<ViewerSession>,
        tlp_desk_viewer::ViewerEvents,
    ),
    ViewerError,
> {
    let identity = Identity::load_or_generate(data_dir).map_err(ViewerError::KnownHosts)?;
    let endpoint = Endpoint::bind(
        Arc::new(identity),
        EndpointConfig {
            bind: std::net::SocketAddr::new(std::net::Ipv6Addr::UNSPECIFIED.into(), 0),
            listen: false,
            ..EndpointConfig::default()
        },
    )
    .map_err(ViewerError::Net)?;
    // Browsing the LAN is only needed for ID/name targets; an IP address always works.
    let discovery = match target {
        ConnectTarget::Id(_) | ConnectTarget::Name(_) => {
            Discovery::start(DiscoveryConfig::default())
                .await
                .ok()
                .map(Arc::new)
        }
        _ => None,
    };
    let viewer = Arc::new(Viewer::new(
        Arc::new(endpoint),
        ViewerConfig {
            known_hosts: KnownHostsStore::File(data_dir.join(tlp_desk_crypto::KNOWN_HOSTS_FILE)),
            pixel_format: PixelFormat::Rgba,
            on_frame: Some(repaint.clone()),
            discovery,
            ..ViewerConfig::default()
        },
    )?);
    let inner2 = inner.clone();
    let awaiting: tlp_desk_viewer::AwaitingAcceptFn = Arc::new(move || {
        let mut g = lock(&inner2);
        if g.generation == generation {
            g.state = ConnState::Connecting {
                awaiting_accept: true,
            };
        }
        drop(g);
        repaint();
    });
    let (session, events) = viewer
        .connect_with_progress(target, auth, Some(awaiting))
        .await?;
    Ok((viewer, Arc::new(session), events))
}

/// Moves the controller to `Failed` when the session ends.
fn watch_events(
    inner: Arc<Mutex<Inner>>,
    generation: u64,
    session: Arc<ViewerSession>,
    mut events: tlp_desk_viewer::ViewerEvents,
    repaint: RepaintFn,
) {
    let _ = session;
    tokio::spawn(async move {
        while let Some(ev) = events.recv().await {
            if let ViewerEvent::Ended(reason) = ev {
                let mut g = lock(&inner);
                if g.generation == generation {
                    g.session = None;
                    g.state = ConnState::Failed {
                        message: end_text(&reason),
                        changed_key: None,
                    };
                }
                drop(g);
                repaint();
                return;
            }
            repaint();
        }
    });
}

/// Human text for why a session ended.
pub fn end_text(reason: &EndReason) -> String {
    match reason {
        EndReason::Local => "disconnected".to_owned(),
        EndReason::Remote(r) => format!("the other computer ended the session ({r:?})"),
        EndReason::ConnectionLost(e) => format!("connection lost: {e}"),
        EndReason::Error(e) => e.clone(),
    }
}

/// How often the UI should repaint while connecting (to show the progress text).
pub const CONNECT_REPAINT: Duration = Duration::from_millis(500);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_pick_the_right_auth() {
        assert!(matches!(auth_from_secret(""), AuthMethod::AcceptPrompt));
        assert!(matches!(auth_from_secret("   "), AuthMethod::AcceptPrompt));
        assert!(matches!(auth_from_secret("042 917"), AuthMethod::OneTimeCode(c) if c == "042917"));
        assert!(matches!(
            auth_from_secret("123456"),
            AuthMethod::OneTimeCode(_)
        ));
        // Seven digits or letters are a password, kept verbatim (Arabic too).
        assert!(matches!(
            auth_from_secret("1234567"),
            AuthMethod::Password(_)
        ));
        assert!(
            matches!(auth_from_secret("\u{643}\u{644}\u{645}\u{629}"), AuthMethod::Password(p) if p.chars().count() == 4)
        );
    }

    #[test]
    fn end_reasons_are_readable() {
        assert!(end_text(&EndReason::ConnectionLost("timeout".into())).contains("timeout"));
        assert_eq!(end_text(&EndReason::Local), "disconnected");
    }

    #[test]
    fn controller_starts_idle_and_disconnect_is_harmless() {
        let dir = std::env::temp_dir();
        let c = Controller::new(dir, Arc::new(|| {})).expect("runtime");
        assert!(matches!(c.state(), ConnState::Idle));
        c.disconnect();
        c.acknowledge();
        assert!(c.session().is_none());
    }
}
