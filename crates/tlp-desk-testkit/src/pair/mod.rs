//! A real host and a real viewer over QUIC on `127.0.0.1`, with fakes for everything that
//! would touch the desktop.
//!
//! ```no_run
//! # async fn demo() {
//! use std::sync::Arc;
//! use tlp_desk_proto::Permissions;
//! use tlp_desk_testkit::{pair::TestPair, probe::ProbeFeature};
//!
//! let chat = ProbeFeature::all("chat-probe");
//! let mut pair = TestPair::start_with(|host, _viewer| {
//!     host.features.push(chat.clone());
//! })
//! .await;
//! let conn = pair.connect(Permissions::VIEW | Permissions::CHAT).await;
//! conn.session.send_message(tlp_desk_proto::Message::ChatMessage {
//!     id: 1,
//!     text: "مرحبا".into(),
//!     sent_unix_ms: 0,
//! });
//! # }
//! ```

use std::sync::{Arc, Mutex};

use tlp_desk_crypto::Identity;
use tlp_desk_host::{Host, HostConfig, HostEvent, HostEvents, SessionId};
use tlp_desk_net::{Endpoint, EndpointConfig};
use tlp_desk_proto::Permissions;
use tlp_desk_viewer::{
    AuthMethod, ConnectTarget, KnownHostsStore, PixelFormat, Viewer, ViewerConfig, ViewerError,
    ViewerEvents, ViewerSession,
};
use tokio::task::JoinHandle;

use crate::fakes::{self, Alive, FakeLocalActivity, Recorded};
use crate::wait;

/// Device name the host announces in `Hello` (Arabic on purpose).
pub const HOST_NAME: &str = "مضيف-host";
/// Device name the viewer announces in `Hello` (Arabic on purpose).
pub const VIEWER_NAME: &str = "عارض-viewer";

/// Monitors the default pair's host has.
pub const MONITORS: u32 = 2;

/// Binds an endpoint on an ephemeral loopback port with a fresh identity.
pub fn endpoint(listen: bool, name: &str) -> Arc<Endpoint> {
    let identity = Identity::generate().unwrap_or_else(|e| panic!("identity: {e}"));
    let ep = Endpoint::bind(
        Arc::new(identity),
        EndpointConfig {
            bind: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            listen,
            device_name: name.to_owned(),
            ..EndpointConfig::default()
        },
    );
    Arc::new(ep.unwrap_or_else(|e| panic!("bind loopback endpoint: {e}")))
}

/// A [`HostConfig`] wired to fakes only: [`MONITORS`] fake monitors, the default (software
/// OpenH264) encoder, a recording injector, a controllable local-activity source, no pointer
/// marker window, no LAN advertising, prompts that never time out during a test.
pub fn host_config(
    alive: &Arc<Alive>,
    rec: &Recorded,
    local: &Arc<FakeLocalActivity>,
) -> HostConfig {
    let mons = fakes::monitors(MONITORS);
    HostConfig {
        device_name: HOST_NAME.into(),
        advertise: false,
        capturer_factory: fakes::capturer_factory(mons.clone(), alive.clone()),
        monitor_lister: fakes::monitor_lister(mons),
        injector_factory: fakes::injector_factory(rec.clone(), alive.clone()),
        local_activity_factory: fakes::local_activity_factory(local.clone()),
        pointer_marker_factory: fakes::pointer_marker_factory(
            Arc::new(Mutex::new(false)),
            alive.clone(),
        ),
        remote_pointer_marker: false,
        ..HostConfig::default()
    }
}

/// A [`ViewerConfig`] for tests: known hosts in memory only, RGBA frames.
pub fn viewer_config() -> ViewerConfig {
    ViewerConfig {
        known_hosts: KnownHostsStore::InMemory,
        pixel_format: PixelFormat::Rgba,
        ..ViewerConfig::default()
    }
}

/// One connected viewer session, as seen from both sides.
#[derive(Debug)]
pub struct Connected {
    /// The viewer's session.
    pub session: ViewerSession,
    /// The viewer's event stream.
    pub events: ViewerEvents,
    /// The host's id for this session ([`Host::set_permissions`], [`Host::disconnect`]).
    pub id: SessionId,
}

/// A running host and a viewer that can dial it.
pub struct TestPair {
    /// The host under test.
    pub host: Host,
    /// The host's event stream (prompts, sessions, features).
    pub events: HostEvents,
    /// The viewer side (shares nothing with the host but the loopback interface).
    pub viewer: Arc<Viewer>,
    /// The viewer's endpoint.
    pub viewer_ep: Arc<Endpoint>,
    /// Where the viewer dials: the host's device ID at its loopback address (pinned).
    pub target: ConnectTarget,
    /// Everything the host injected.
    pub injected: Recorded,
    /// The host user's "physical" mouse/keyboard activity.
    pub local: Arc<FakeLocalActivity>,
    /// Live fakes created for this pair.
    pub alive: Arc<Alive>,
}

impl std::fmt::Debug for TestPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestPair")
            .field("host", &self.host)
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl TestPair {
    /// A pair with the defaults of [`host_config`] and [`viewer_config`].
    pub async fn start() -> Self {
        Self::start_with(|_, _| {}).await
    }

    /// A pair whose configs `tweak` may change first (features, permissions, encoder, ...).
    pub async fn start_with(tweak: impl FnOnce(&mut HostConfig, &mut ViewerConfig)) -> Self {
        Self::start_inner(false, tweak).await
    }

    /// Like [`TestPair::start_with`], but the host uses [`fakes::FakeEncoder`]: no video work
    /// at all (cheap soaks, wire-level checks). The viewer cannot decode these frames.
    pub async fn start_with_fake_encoder(
        tweak: impl FnOnce(&mut HostConfig, &mut ViewerConfig),
    ) -> Self {
        Self::start_inner(true, tweak).await
    }

    async fn start_inner(
        fake_encoder: bool,
        tweak: impl FnOnce(&mut HostConfig, &mut ViewerConfig),
    ) -> Self {
        wait::init_tracing();
        let alive = Arc::new(Alive::default());
        let injected = Recorded::default();
        let local = Arc::new(FakeLocalActivity::default());
        let mut host_cfg = host_config(&alive, &injected, &local);
        if fake_encoder {
            host_cfg.encoder_factory = fakes::encoder_factory(alive.clone());
        }
        let mut viewer_cfg = viewer_config();
        tweak(&mut host_cfg, &mut viewer_cfg);
        let host_ep = endpoint(true, HOST_NAME);
        let addr = host_ep
            .local_addr()
            .unwrap_or_else(|e| panic!("host address: {e}"));
        let target = ConnectTarget::IdAt(host_ep.device_id(), addr);
        let (host, events) = Host::start(host_ep, host_cfg)
            .await
            .unwrap_or_else(|e| panic!("start host: {e}"));
        let viewer_ep = endpoint(false, VIEWER_NAME);
        let viewer = Viewer::new(viewer_ep.clone(), viewer_cfg)
            .unwrap_or_else(|e| panic!("start viewer: {e}"));
        Self {
            host,
            events,
            viewer: Arc::new(viewer),
            viewer_ep,
            target,
            injected,
            local,
            alive,
        }
    }

    /// Dials the host in the background (e.g. to answer the prompt from the test).
    pub fn spawn_connect(
        &self,
        auth: AuthMethod,
    ) -> JoinHandle<Result<(ViewerSession, ViewerEvents), ViewerError>> {
        let viewer = self.viewer.clone();
        let target = self.target.clone();
        tokio::spawn(async move { viewer.connect(target, auth).await })
    }

    /// Connects with the accept prompt and accepts it with `permissions`; waits until the host
    /// reports the session.
    pub async fn connect(&mut self, permissions: Permissions) -> Connected {
        let pending = self.spawn_connect(AuthMethod::AcceptPrompt);
        let req = match wait::host_event(&mut self.events, |e| {
            matches!(e, HostEvent::IncomingRequest(_))
        })
        .await
        {
            HostEvent::IncomingRequest(r) => r.id,
            other => panic!("expected a prompt, got {other:?}"),
        };
        assert!(self.host.accept(req, Some(permissions)), "prompt vanished");
        let (session, events) = wait::within("the viewer to connect", pending)
            .await
            .unwrap_or_else(|e| panic!("connect task: {e}"))
            .unwrap_or_else(|e| panic!("connect: {e}"));
        let id = match wait::host_event(&mut self.events, |e| {
            matches!(e, HostEvent::SessionStarted(_))
        })
        .await
        {
            HostEvent::SessionStarted(cv) => cv.id,
            other => panic!("expected SessionStarted, got {other:?}"),
        };
        Connected {
            session,
            events,
            id,
        }
    }

    /// Waits for the host's next event matching `pred`.
    pub async fn host_event(&mut self, pred: impl Fn(&HostEvent) -> bool) -> HostEvent {
        wait::host_event(&mut self.events, pred).await
    }

    /// Waits until the host has no session and every fake it created was dropped, i.e. the
    /// per-session capture/encode and input threads have exited.
    pub async fn all_released(&self) {
        wait::until("host sessions to end and release their fakes", || {
            self.host.sessions().is_empty() && self.alive.total() == 0
        })
        .await;
    }
}
