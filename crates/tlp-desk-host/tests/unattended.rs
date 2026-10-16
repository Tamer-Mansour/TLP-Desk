//! Unattended access end to end on loopback: permanent password (right, wrong, lockout),
//! restart with a reconnect ticket and getting back in after a simulated host restart, and the
//! RESTART / SYSTEM permission gates. Restart and lock go to a recording fake: nothing here
//! restarts, locks or shuts down the machine.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tlp_desk_capture::{BgraFrame, CaptureError, CursorState, MonitorInfo, ScreenCapturer};
use tlp_desk_crypto::{
    Identity, PasswordVerifier, RateLimitConfig, TicketStore, DEFAULT_TICKET_TTL, TICKETS_FILE,
};
use tlp_desk_host::{
    AuthKind, Host, HostConfig, HostError, HostEvent, HostEvents, SystemPower, UnattendedEvent,
    UnattendedFeature, DEFAULT_PERMISSIONS, UNATTENDED_FEATURE,
};
use tlp_desk_input::{InputError, InputEvent, InputInjector, LocalActivity, MonitorRect};
use tlp_desk_net::{Endpoint, EndpointConfig, FeatureSet};
use tlp_desk_proto::{Message, Permissions, RejectReason};
use tlp_desk_viewer::{
    AuthMethod, ConnectTarget, EndReason as ViewerEnd, KnownHostsStore, PixelFormat,
    ReconnectPolicy, ReconnectTickets, TicketReceived, Viewer, ViewerConfig, ViewerError,
    ViewerEvent, ViewerEvents, ViewerSession, RECONNECT_FEATURE,
};

const WAIT: Duration = Duration::from_secs(20);

async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, fut)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

// ----- fakes ------------------------------------------------------------------------------

struct FakeCapturer {
    mon: MonitorInfo,
    next_at: Instant,
}

fn fake_monitor() -> MonitorInfo {
    MonitorInfo {
        index: 0,
        name: "FAKE".into(),
        width: 160,
        height: 120,
        primary: true,
        x: 0,
        y: 0,
    }
}

impl ScreenCapturer for FakeCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<BgraFrame>, CaptureError> {
        let now = Instant::now();
        if now < self.next_at {
            std::thread::sleep(timeout.min(self.next_at - now));
            return Ok(None);
        }
        self.next_at = Instant::now() + Duration::from_millis(33);
        let mut f = BgraFrame::new_tight(self.mon.width, self.mon.height);
        for y in 0..self.mon.height {
            for px in f.row_mut(y).chunks_exact_mut(4) {
                px.copy_from_slice(&[40, 80, 120, 255]);
            }
        }
        Ok(Some(f))
    }

    fn cursor(&mut self) -> Result<CursorState, CaptureError> {
        Ok(CursorState {
            x: 0,
            y: 0,
            visible: false,
            shape: None,
        })
    }

    fn monitor(&self) -> &MonitorInfo {
        &self.mon
    }
}

struct NoInput;

impl InputInjector for NoInput {
    fn inject(&mut self, _: &InputEvent) -> Result<(), InputError> {
        Ok(())
    }
}

struct NoLocal;

impl LocalActivity for NoLocal {
    fn last_physical_input(&self) -> Option<Instant> {
        None
    }
}

/// Records restart / lock requests instead of touching the machine.
#[derive(Default)]
struct FakePower {
    restarts: AtomicU32,
    locks: AtomicU32,
    fail_restart: bool,
}

impl SystemPower for FakePower {
    fn restart(&self) -> Result<(), HostError> {
        self.restarts.fetch_add(1, Ordering::SeqCst);
        if self.fail_restart {
            return Err(HostError::System("fake restart refused".into()));
        }
        Ok(())
    }

    fn lock_workstation(&self) -> Result<(), HostError> {
        self.locks.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

// ----- harness ----------------------------------------------------------------------------

fn base_host_config() -> HostConfig {
    HostConfig {
        advertise: false,
        remote_pointer_marker: false,
        capturer_factory: Arc::new(|_, _| {
            Ok(Box::new(FakeCapturer {
                mon: fake_monitor(),
                next_at: Instant::now(),
            }) as Box<dyn ScreenCapturer>)
        }),
        monitor_lister: Arc::new(|| Ok(vec![fake_monitor()])),
        injector_factory: Arc::new(
            |_: MonitorRect| Ok(Box::new(NoInput) as Box<dyn InputInjector>),
        ),
        local_activity_factory: Arc::new(|| Ok(Arc::new(NoLocal) as Arc<dyn LocalActivity>)),
        ..HostConfig::default()
    }
}

fn host_endpoint(identity: Arc<Identity>, bind: SocketAddr, rate: RateLimitConfig) -> Endpoint {
    try_host_endpoint(identity, bind, rate).unwrap()
}

fn try_host_endpoint(
    identity: Arc<Identity>,
    bind: SocketAddr,
    rate: RateLimitConfig,
) -> Result<Endpoint, tlp_desk_net::NetError> {
    Endpoint::bind(
        identity,
        EndpointConfig {
            bind,
            listen: true,
            device_name: "حاسوب المضيف".into(),
            rate_limit: rate,
            ..EndpointConfig::default()
        },
    )
}

/// Binds the host port, waiting for the previous endpoint on it to let go (a "reboot" reuses
/// it); the teardown of its sessions is asynchronous, so allow a few seconds.
async fn host_endpoint_when_free(
    identity: Arc<Identity>,
    bind: SocketAddr,
    rate: RateLimitConfig,
) -> Endpoint {
    for _ in 0..50 {
        match try_host_endpoint(identity.clone(), bind, rate) {
            Ok(ep) => return ep,
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    host_endpoint(identity, bind, rate)
}

/// A host with the unattended feature, a persistent ticket store and a fake power source.
async fn start_host(
    identity: Arc<Identity>,
    bind: SocketAddr,
    tickets: TicketStore,
    power: Arc<FakePower>,
    tweak: impl FnOnce(&mut HostConfig),
) -> (Host, HostEvents) {
    let ep = Arc::new(host_endpoint_when_free(identity, bind, RateLimitConfig::default()).await);
    let feature = Arc::new(UnattendedFeature::new(Some(tickets.clone()), power));
    let mut cfg = HostConfig {
        reconnect_tickets: Some(tickets),
        default_permissions: DEFAULT_PERMISSIONS | Permissions::RESTART | Permissions::SYSTEM,
        features: FeatureSet::new().with(feature),
        ..base_host_config()
    };
    tweak(&mut cfg);
    Host::start(ep, cfg).await.unwrap()
}

fn viewer(keeper: Option<&ReconnectTickets>) -> Viewer {
    let ep = Arc::new(
        Endpoint::bind(
            Arc::new(Identity::generate().unwrap()),
            EndpointConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                listen: false,
                device_name: "viewer".into(),
                ..EndpointConfig::default()
            },
        )
        .unwrap(),
    );
    let mut features = FeatureSet::new();
    if let Some(k) = keeper {
        features.push(k.feature());
    }
    Viewer::new(
        ep,
        ViewerConfig {
            known_hosts: KnownHostsStore::InMemory,
            pixel_format: PixelFormat::Rgba,
            features,
            ..ViewerConfig::default()
        },
    )
    .unwrap()
}

async fn host_event(events: &mut HostEvents, pred: impl Fn(&HostEvent) -> bool) -> HostEvent {
    within("host event", async {
        loop {
            let ev = events.recv().await.expect("host events closed");
            if pred(&ev) {
                return ev;
            }
        }
    })
    .await
}

async fn viewer_event(
    events: &mut ViewerEvents,
    pred: impl Fn(&ViewerEvent) -> bool,
) -> ViewerEvent {
    within("viewer event", async {
        loop {
            let ev = events.recv().await.expect("viewer events closed");
            if pred(&ev) {
                return ev;
            }
        }
    })
    .await
}

async fn eventually(what: &str, cond: impl Fn() -> bool) {
    within(what, async {
        while !cond() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

async fn connect_code(v: &Viewer, host: &Host, addr: SocketAddr) -> (ViewerSession, ViewerEvents) {
    within(
        "code login",
        v.connect(
            ConnectTarget::IdAt(host.device_id(), addr),
            AuthMethod::OneTimeCode(host.one_time_code().current()),
        ),
    )
    .await
    .unwrap()
}

// ----- tests ------------------------------------------------------------------------------

/// The permanent password lets a viewer in without a prompt; a wrong one is refused and, after
/// the attempt limit, the peer is locked out even with the right password.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn password_login_right_wrong_and_lockout() {
    let identity = Arc::new(Identity::generate().unwrap());
    let rate = RateLimitConfig {
        max_attempts: 2,
        window: Duration::from_secs(60),
        lockout: Duration::from_secs(600),
    };
    let ep = Arc::new(host_endpoint(
        identity.clone(),
        "127.0.0.1:0".parse().unwrap(),
        rate,
    ));
    let addr = ep.local_addr().unwrap();
    let (host, mut events) = Host::start(ep, base_host_config()).await.unwrap();
    // Arabic password with digits and Latin letters.
    let password = "كلمة المرور 2026 Desk";
    let verifier = PasswordVerifier::derive(password, &identity.fingerprint()).unwrap();
    let target = ConnectTarget::IdAt(host.device_id(), addr);
    let v = viewer(None);

    // Not set up yet: refused.
    let err = v
        .connect(target.clone(), AuthMethod::Password(password.into()))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ViewerError::Rejected(RejectReason::NotAllowed)),
        "{err}"
    );

    host.set_password(Some(verifier));
    let (session, _ev) = within(
        "password login",
        v.connect(target.clone(), AuthMethod::Password(password.into())),
    )
    .await
    .unwrap();
    let started = host_event(&mut events, |e| matches!(e, HostEvent::SessionStarted(_))).await;
    let HostEvent::SessionStarted(info) = started else {
        unreachable!()
    };
    assert_eq!(info.auth, AuthKind::Password);
    session.disconnect();
    within("session end", session.ended()).await;

    for _ in 0..rate.max_attempts {
        let err = v
            .connect(target.clone(), AuthMethod::Password("كلمة خاطئة".into()))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ViewerError::Rejected(RejectReason::WrongCredentials)),
            "{err}"
        );
    }
    // Locked out now: even the right password gets "rate limited" (the UI says "locked out
    // for N minutes").
    let err = v
        .connect(target, AuthMethod::Password(password.into()))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ViewerError::Rejected(RejectReason::RateLimited)),
        "{err}"
    );
    host.shutdown();
}

/// Restart with reconnect: the viewer gets a ticket, the (fake) restart happens, the host comes
/// back as a new process with the same identity, and the viewer gets back in with the ticket,
/// with the permissions it had. The ticket works once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_and_reconnect_with_ticket() {
    let dir = tempfile::tempdir().unwrap();
    let tickets_path = dir.path().join("بيانات TLP").join(TICKETS_FILE);
    let identity = Arc::new(Identity::generate().unwrap());
    let power = Arc::new(FakePower::default());
    let tickets = TicketStore::load(&tickets_path, DEFAULT_TICKET_TTL).unwrap();
    let (host, mut host_events) = start_host(
        identity.clone(),
        "127.0.0.1:0".parse().unwrap(),
        tickets,
        power.clone(),
        |_| {},
    )
    .await;
    let addr = host.endpoint().local_addr().unwrap();
    let keeper = ReconnectTickets::new();
    let v = viewer(Some(&keeper));
    let (session, mut events) = connect_code(&v, &host, addr).await;
    let granted = session.permissions();
    assert!(granted.contains(Permissions::RESTART));

    assert!(session.send_message(Message::RestartRemote { reconnect: true }));
    let ev = viewer_event(
        &mut events,
        |e| matches!(e, ViewerEvent::Feature(f) if f.feature == RECONNECT_FEATURE),
    )
    .await;
    let ViewerEvent::Feature(f) = ev else {
        unreachable!()
    };
    assert!(f.payload::<TicketReceived>().is_some());
    let ev = host_event(
        &mut host_events,
        |e| matches!(e, HostEvent::Feature { event, .. } if event.feature == UNATTENDED_FEATURE),
    )
    .await;
    let HostEvent::Feature { event, .. } = ev else {
        unreachable!()
    };
    assert_eq!(
        event.payload::<UnattendedEvent>(),
        Some(&UnattendedEvent::RestartStarted { reconnect: true })
    );
    assert_eq!(power.restarts.load(Ordering::SeqCst), 1);
    assert!(keeper.has(session.feature_key()));

    // The old process must not swallow the ticket while the restart is still in progress.
    let ticket = keeper.take(session.feature_key()).unwrap();
    let quick = ReconnectPolicy {
        give_up_after: Duration::from_secs(3),
        first_delay: Duration::from_millis(50),
        ..ReconnectPolicy::default()
    };
    let err = v
        .reconnect_with_ticket(ticket.clone(), quick, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ViewerError::Rejected(RejectReason::NotAllowed)),
        "{err}"
    );

    // "Reboot": the host goes away, then a new process with the same identity, the same port
    // and the ticket store loaded from disk again.
    host.shutdown();
    let ended = within("viewer session end", session.ended()).await;
    assert!(!matches!(ended, ViewerEnd::Local), "{ended:?}");
    // Dropping the host must be enough to free its port (no manual `Endpoint::close`).
    drop(host);
    let tickets = TicketStore::load(&tickets_path, DEFAULT_TICKET_TTL).unwrap();
    assert_eq!(tickets.len(), 1);
    let (host2, mut host_events2) =
        start_host(identity, addr, tickets.clone(), power.clone(), |_| {}).await;

    let tries = Arc::new(AtomicU32::new(0));
    let t2 = tries.clone();
    let policy = ReconnectPolicy {
        first_delay: Duration::from_millis(100),
        ..ReconnectPolicy::default()
    };
    let (session2, mut events2) = within(
        "reconnect",
        v.reconnect_with_ticket(
            ticket.clone(),
            policy,
            Some(Arc::new(move |n| t2.store(n, Ordering::SeqCst))),
        ),
    )
    .await
    .unwrap();
    assert_eq!(tries.load(Ordering::SeqCst), 1);
    assert_eq!(session2.permissions(), granted);
    assert_eq!(session2.peer().device_id, host2.device_id());
    let started = host_event(&mut host_events2, |e| {
        matches!(e, HostEvent::SessionStarted(_))
    })
    .await;
    let HostEvent::SessionStarted(info) = started else {
        unreachable!()
    };
    assert_eq!(info.auth, AuthKind::ReconnectTicket);
    assert!(tickets.is_empty(), "ticket consumed");
    // The video resumes.
    within("a frame after reconnect", async {
        while session2.take_frame().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let _ = viewer_event(&mut events2, |e| {
        matches!(e, ViewerEvent::MonitorsChanged { .. })
    })
    .await;

    // Single use: the same ticket does not work twice.
    let err = v
        .reconnect_with_ticket(ticket, quick, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ViewerError::Rejected(RejectReason::WrongCredentials)),
        "{err}"
    );
    host2.shutdown();
}

/// Revoking RESTART stops restarts (and revokes an issued ticket); SYSTEM gates locking; a
/// failed restart revokes its ticket.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn permissions_gate_restart_and_lock() {
    let identity = Arc::new(Identity::generate().unwrap());
    let power = Arc::new(FakePower::default());
    let tickets = TicketStore::in_memory(DEFAULT_TICKET_TTL);
    let (host, mut host_events) = start_host(
        identity,
        "127.0.0.1:0".parse().unwrap(),
        tickets.clone(),
        power.clone(),
        |_| {},
    )
    .await;
    let addr = host.endpoint().local_addr().unwrap();
    let keeper = ReconnectTickets::new();
    let v = viewer(Some(&keeper));
    let (session, mut events) = connect_code(&v, &host, addr).await;
    let started = host_event(&mut host_events, |e| {
        matches!(e, HostEvent::SessionStarted(_))
    })
    .await;
    let HostEvent::SessionStarted(info) = started else {
        unreachable!()
    };
    let fp = info.peer.fingerprint;

    // Lock works with SYSTEM.
    assert!(session.send_message(Message::LockWorkstation));
    eventually("lock", || power.locks.load(Ordering::SeqCst) == 1).await;

    // A restart with reconnect issues a ticket...
    assert!(session.send_message(Message::RestartRemote { reconnect: true }));
    eventually("restart", || power.restarts.load(Ordering::SeqCst) == 1).await;
    eventually("ticket", || tickets.len() == 1).await;

    // ...which a permission change revokes; without RESTART and SYSTEM nothing happens.
    let reduced = info
        .permissions
        .difference(Permissions::RESTART)
        .difference(Permissions::SYSTEM);
    assert!(host.set_permissions(info.id, reduced));
    let _ = viewer_event(
        &mut events,
        |e| matches!(e, ViewerEvent::PermissionsChanged(p) if *p == reduced),
    )
    .await;
    eventually("ticket revoked", || tickets.is_empty()).await;
    assert!(session.send_message(Message::RestartRemote { reconnect: true }));
    assert!(session.send_message(Message::LockWorkstation));
    // Nothing to wait for when nothing happens: give the host time to (not) act.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(power.restarts.load(Ordering::SeqCst), 1);
    assert_eq!(power.locks.load(Ordering::SeqCst), 1);
    assert!(!tickets.has_ticket(&fp, tlp_desk_crypto::now_unix_ms()));
    host.shutdown();

    // A restart the OS refuses revokes the ticket it issued.
    let failing = Arc::new(FakePower {
        fail_restart: true,
        ..FakePower::default()
    });
    let tickets = TicketStore::in_memory(DEFAULT_TICKET_TTL);
    let (host, mut host_events) = start_host(
        Arc::new(Identity::generate().unwrap()),
        "127.0.0.1:0".parse().unwrap(),
        tickets.clone(),
        failing.clone(),
        |_| {},
    )
    .await;
    let addr = host.endpoint().local_addr().unwrap();
    let (session, _events) = connect_code(&v, &host, addr).await;
    assert!(session.send_message(Message::RestartRemote { reconnect: true }));
    let ev = host_event(&mut host_events, |e| matches!(e, HostEvent::Feature { .. })).await;
    let HostEvent::Feature { event, .. } = ev else {
        unreachable!()
    };
    assert!(matches!(
        event.payload::<UnattendedEvent>(),
        Some(UnattendedEvent::RestartFailed(_))
    ));
    assert!(tickets.is_empty());
    host.shutdown();
}

/// Ending the session on purpose (host user disconnects) revokes the viewer's ticket.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn explicit_disconnect_revokes_ticket() {
    let power = Arc::new(FakePower::default());
    let tickets = TicketStore::in_memory(DEFAULT_TICKET_TTL);
    let (host, mut host_events) = start_host(
        Arc::new(Identity::generate().unwrap()),
        "127.0.0.1:0".parse().unwrap(),
        tickets.clone(),
        power.clone(),
        |_| {},
    )
    .await;
    let addr = host.endpoint().local_addr().unwrap();
    let v = viewer(Some(&ReconnectTickets::new()));
    let (session, _events) = connect_code(&v, &host, addr).await;
    let started = host_event(&mut host_events, |e| {
        matches!(e, HostEvent::SessionStarted(_))
    })
    .await;
    let HostEvent::SessionStarted(info) = started else {
        unreachable!()
    };
    assert!(session.send_message(Message::RestartRemote { reconnect: true }));
    eventually("ticket", || tickets.len() == 1).await;
    assert!(host.disconnect(info.id));
    within("viewer end", session.ended()).await;
    eventually("ticket revoked", || tickets.is_empty()).await;
    host.shutdown();
}
