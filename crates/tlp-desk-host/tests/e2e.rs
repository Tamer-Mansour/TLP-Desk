//! End-to-end loopback tests: a real host and a real viewer in one process over QUIC on
//! 127.0.0.1, with a synthetic capturer and a recording injector.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tlp_desk_capture::{BgraFrame, CaptureError, CursorState, MonitorInfo, ScreenCapturer};
use tlp_desk_crypto::{Fingerprint, HostKey, Identity};
use tlp_desk_host::{
    CancelReason, EndReason as HostEnd, Host, HostConfig, HostEvent, HostEvents, RequestId,
};
use tlp_desk_input::{InputError, InputEvent, InputInjector, LocalActivity, MonitorRect};
use tlp_desk_net::{Endpoint, EndpointConfig};
use tlp_desk_proto::{ByeReason, MouseButton, Permissions, RejectReason};
use tlp_desk_viewer::{
    AuthMethod, ConnectTarget, DecodedFrame, EndReason as ViewerEnd, KnownHostsStore, PixelFormat,
    Viewer, ViewerConfig, ViewerError, ViewerEvent, ViewerEvents, ViewerSession,
};

const W: u32 = 320;
const H: u32 = 240;
const WAIT: Duration = Duration::from_secs(15);

/// Synthetic screen: a bright bar moving across a dark background, ~60 frames per second.
struct FakeCapturer {
    mon: MonitorInfo,
    n: u32,
    next_at: Instant,
}

fn fake_monitor() -> MonitorInfo {
    MonitorInfo {
        index: 0,
        name: "FAKE".into(),
        width: W,
        height: H,
        primary: true,
        x: 0,
        y: 0,
    }
}

/// A second, differently sized fake monitor to the right of the first.
fn fake_monitor_2() -> MonitorInfo {
    MonitorInfo {
        index: 1,
        name: "FAKE-2".into(),
        width: 480,
        height: 272,
        primary: false,
        x: W as i32,
        y: 0,
    }
}

impl FakeCapturer {
    fn new() -> Self {
        Self::on(fake_monitor())
    }

    fn on(mon: MonitorInfo) -> Self {
        Self {
            mon,
            n: 0,
            next_at: Instant::now(),
        }
    }
}

impl ScreenCapturer for FakeCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<BgraFrame>, CaptureError> {
        let now = Instant::now();
        if now < self.next_at {
            std::thread::sleep(timeout.min(self.next_at - now));
            if Instant::now() < self.next_at {
                return Ok(None);
            }
        }
        self.next_at = Instant::now() + Duration::from_millis(16);
        self.n += 1;
        let (w, h) = (self.mon.width, self.mon.height);
        let mut f = BgraFrame::new_tight(w, h);
        let bar = (self.n * 8) % w;
        for y in 0..h {
            let row = f.row_mut(y);
            for x in 0..w {
                let i = x as usize * 4;
                let on = x >= bar && x < bar + 32;
                let v = if on { 230 } else { (y % 64) as u8 };
                row[i..i + 4].copy_from_slice(&[v, v / 2, 255 - v, 255]);
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

type Recorded = Arc<Mutex<Vec<InputEvent>>>;

/// Fake "last physical local input" source (instead of real low-level hooks).
#[derive(Default)]
struct FakeLocal(Mutex<Option<Instant>>);

impl LocalActivity for FakeLocal {
    fn last_physical_input(&self) -> Option<Instant> {
        *self.0.lock().unwrap()
    }
}

struct FakeInjector(Recorded);

impl InputInjector for FakeInjector {
    fn inject(&mut self, event: &InputEvent) -> Result<(), InputError> {
        self.0.lock().unwrap().push(event.clone());
        Ok(())
    }
}

fn endpoint(listen: bool) -> Arc<Endpoint> {
    Arc::new(
        Endpoint::bind(
            Arc::new(Identity::generate().unwrap()),
            EndpointConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                listen,
                device_name: if listen { "host-pc" } else { "viewer-pc" }.into(),
                ..EndpointConfig::default()
            },
        )
        .unwrap(),
    )
}

struct Harness {
    host: Host,
    events: HostEvents,
    viewer: Arc<Viewer>,
    viewer_ep: Arc<Endpoint>,
    recorded: Recorded,
    /// Monitor rectangles the injector factory was asked for, in order.
    rects: Arc<Mutex<Vec<MonitorRect>>>,
    local: Arc<FakeLocal>,
    target: ConnectTarget,
}

async fn harness(real_capture: bool, tweak: impl FnOnce(&mut HostConfig)) -> Harness {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_test_writer()
        .try_init();
    let recorded: Recorded = Arc::default();
    let rec = recorded.clone();
    let rects: Arc<Mutex<Vec<MonitorRect>>> = Arc::default();
    let rects2 = rects.clone();
    let local = Arc::new(FakeLocal::default());
    let local2 = local.clone();
    let mut cfg = HostConfig {
        advertise: false,
        injector_factory: Arc::new(move |r: MonitorRect| {
            rects2.lock().unwrap().push(r);
            Ok(Box::new(FakeInjector(rec.clone())) as Box<dyn InputInjector>)
        }),
        // Never install real hooks or show the marker on the developer's screen in tests.
        local_activity_factory: Arc::new(move || Ok(local2.clone() as Arc<dyn LocalActivity>)),
        remote_pointer_marker: false,
        ..HostConfig::default()
    };
    if !real_capture {
        cfg.capturer_factory =
            Arc::new(|_| Ok(Box::new(FakeCapturer::new()) as Box<dyn ScreenCapturer>));
        cfg.monitor_lister = Arc::new(|| Ok(vec![fake_monitor()]));
    }
    tweak(&mut cfg);
    let host_ep = endpoint(true);
    let addr = host_ep.local_addr().unwrap();
    let target = ConnectTarget::IdAt(host_ep.device_id(), addr);
    let (host, events) = Host::start(host_ep, cfg).await.unwrap();
    let viewer_ep = endpoint(false);
    let viewer = Arc::new(
        Viewer::new(
            viewer_ep.clone(),
            ViewerConfig {
                known_hosts: KnownHostsStore::InMemory,
                pixel_format: PixelFormat::Rgba,
                ..ViewerConfig::default()
            },
        )
        .unwrap(),
    );
    Harness {
        host,
        events,
        viewer,
        viewer_ep,
        recorded,
        rects,
        local,
        target,
    }
}

async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, fut)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// Next host event matching `pred` (others are skipped).
async fn host_event(events: &mut HostEvents, pred: impl Fn(&HostEvent) -> bool) -> HostEvent {
    within("host event", async {
        loop {
            let ev = events.recv().await.expect("host event channel closed");
            if pred(&ev) {
                return ev;
            }
        }
    })
    .await
}

async fn incoming(events: &mut HostEvents) -> RequestId {
    match host_event(events, |e| matches!(e, HostEvent::IncomingRequest(_))).await {
        HostEvent::IncomingRequest(r) => r.id,
        _ => unreachable!(),
    }
}

type ConnectResult = Result<(ViewerSession, ViewerEvents), ViewerError>;

fn spawn_connect(h: &Harness, auth: AuthMethod) -> tokio::task::JoinHandle<ConnectResult> {
    let viewer = h.viewer.clone();
    let target = h.target.clone();
    tokio::spawn(async move { viewer.connect(target, auth).await })
}

async fn first_frame(s: &ViewerSession) -> DecodedFrame {
    within("a decoded frame", async {
        loop {
            if let Some(f) = s.take_frame() {
                return f;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
}

async fn wait_recorded(rec: &Recorded, pred: impl Fn(&[InputEvent]) -> bool) {
    within("recorded input", async {
        loop {
            if pred(&rec.lock().unwrap()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
}

/// Accept prompt -> session; decoded frames have the right size, input reaches the injector,
/// the "who is connected" list shows the viewer, and a viewer disconnect releases all keys.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accept_prompt_streams_video_and_injects_input() {
    let mut h = harness(false, |_| {}).await;
    let conn = spawn_connect(&h, AuthMethod::AcceptPrompt);
    let req = incoming(&mut h.events).await;
    let pending = h.host.pending_requests();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].peer.device_id, h.viewer_ep.device_id());
    assert_eq!(pending[0].peer.name, "viewer-pc");
    assert!(h.host.accept(req, None));
    let (session, mut vevents) = conn.await.unwrap().expect("connect");
    assert_eq!(session.peer().device_id, h.host.device_id());
    assert_eq!(
        session.permissions(),
        Permissions::VIEW | Permissions::CONTROL | Permissions::KEYBOARD
    );

    let started = host_event(&mut h.events, |e| matches!(e, HostEvent::SessionStarted(_))).await;
    let HostEvent::SessionStarted(cv) = started else {
        unreachable!()
    };
    let who = h.host.sessions();
    assert_eq!(who.len(), 1);
    assert_eq!(who[0].id, cv.id);
    assert_eq!(who[0].peer.device_id, h.viewer_ep.device_id());

    let f = first_frame(&session).await;
    assert_eq!((f.width, f.height), (W, H));
    assert_eq!(f.pixels.len(), (W * H * 4) as usize);
    assert_eq!(f.format, PixelFormat::Rgba);
    // Frames keep coming (the bar moves).
    let f2 = first_frame(&session).await;
    assert!(f2.seq > f.seq);

    let events = [
        InputEvent::MouseMove { x: 100, y: 200 },
        InputEvent::MouseButton {
            button: MouseButton::Left,
            down: true,
        },
        InputEvent::MouseButton {
            button: MouseButton::Left,
            down: false,
        },
        InputEvent::Key {
            scancode: 0x1e,
            extended: false,
            down: true,
        },
    ];
    for ev in &events {
        assert!(session.send_input(ev.clone()));
    }
    wait_recorded(&h.recorded, |r| r.len() >= events.len()).await;
    assert_eq!(&h.recorded.lock().unwrap()[..events.len()], &events);

    // Stats arrive about once per second.
    let stats = within("stats", async {
        loop {
            if let Some(ViewerEvent::Stats(s)) = vevents.recv().await {
                if s.frames_decoded > 0 {
                    return s;
                }
            }
        }
    })
    .await;
    assert!(stats.fps > 0.0 && stats.bitrate_kbps > 0.0, "{stats:?}");
    assert_eq!((stats.width, stats.height), (W, H));

    session.disconnect();
    assert_eq!(
        within("viewer end", session.ended()).await,
        ViewerEnd::Local
    );
    let ended = host_event(&mut h.events, |e| {
        matches!(e, HostEvent::SessionEnded { .. })
    })
    .await;
    let HostEvent::SessionEnded { reason, .. } = ended else {
        unreachable!()
    };
    assert_eq!(reason, HostEnd::Remote(ByeReason::UserClosed));
    assert!(h.host.sessions().is_empty());
    wait_recorded(&h.recorded, |r| r.last() == Some(&InputEvent::ReleaseAll)).await;
}

/// VIEW-only sessions see the screen but none of their input is injected; granting CONTROL
/// live lets mouse input through, keys stay blocked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn view_only_permissions_block_input() {
    let mut h = harness(false, |_| {}).await;
    let conn = spawn_connect(&h, AuthMethod::AcceptPrompt);
    let req = incoming(&mut h.events).await;
    h.host.accept(req, Some(Permissions::VIEW));
    let (session, mut vevents) = conn.await.unwrap().expect("connect");
    assert_eq!(session.permissions(), Permissions::VIEW);
    let f = first_frame(&session).await;
    assert_eq!((f.width, f.height), (W, H));

    let mv = InputEvent::MouseMove { x: 1, y: 1 };
    let key = InputEvent::Key {
        scancode: 0x10,
        extended: false,
        down: true,
    };
    session.send_input(mv.clone());
    session.send_input(key.clone());
    session.send_input(InputEvent::ReleaseAll);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        h.recorded.lock().unwrap().is_empty(),
        "view-only input leaked"
    );

    let id = h.host.sessions()[0].id;
    assert!(h
        .host
        .set_permissions(id, Permissions::VIEW | Permissions::CONTROL));
    within("permissions change", async {
        loop {
            if let Some(ViewerEvent::PermissionsChanged(p)) = vevents.recv().await {
                assert_eq!(p, Permissions::VIEW | Permissions::CONTROL);
                return;
            }
        }
    })
    .await;
    assert_eq!(
        session.permissions(),
        Permissions::VIEW | Permissions::CONTROL
    );
    session.send_input(key.clone());
    session.send_input(mv.clone());
    wait_recorded(&h.recorded, |r| !r.is_empty()).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        *h.recorded.lock().unwrap(),
        vec![mv],
        "key must stay blocked"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reject_reaches_viewer() {
    let mut h = harness(false, |_| {}).await;
    let conn = spawn_connect(&h, AuthMethod::AcceptPrompt);
    let req = incoming(&mut h.events).await;
    assert!(h.host.reject(req));
    let err = conn.await.unwrap().expect_err("must be rejected");
    assert!(
        matches!(err, ViewerError::Rejected(RejectReason::UserDeclined)),
        "{err:?}"
    );
    assert!(h.host.sessions().is_empty());
    assert!(h.host.pending_requests().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unanswered_prompt_times_out_and_rejects() {
    let mut h = harness(false, |c| c.accept_timeout = Duration::from_millis(300)).await;
    let conn = spawn_connect(&h, AuthMethod::AcceptPrompt);
    let req = incoming(&mut h.events).await;
    let ev = host_event(&mut h.events, |e| {
        matches!(e, HostEvent::RequestCancelled { .. })
    })
    .await;
    let HostEvent::RequestCancelled { id, reason } = ev else {
        unreachable!()
    };
    assert_eq!((id, reason), (req, CancelReason::TimedOut));
    let err = conn.await.unwrap().expect_err("must be rejected");
    assert!(matches!(
        err,
        ViewerError::Rejected(RejectReason::UserDeclined)
    ));
    assert!(!h.host.accept(req, None), "request is gone");
}

/// Accept-prompt requests are rate limited per peer IP (Lane A follow-up).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accept_prompts_are_rate_limited_per_ip() {
    let mut h = harness(false, |c| c.prompt_rate_limit.max_attempts = 2).await;
    for _ in 0..2 {
        let conn = spawn_connect(&h, AuthMethod::AcceptPrompt);
        let req = incoming(&mut h.events).await;
        h.host.reject(req);
        assert!(matches!(
            conn.await.unwrap(),
            Err(ViewerError::Rejected(RejectReason::UserDeclined))
        ));
    }
    let conn = spawn_connect(&h, AuthMethod::AcceptPrompt);
    let err = within("rate-limited connect", conn)
        .await
        .unwrap()
        .expect_err("limited");
    assert!(
        matches!(err, ViewerError::Rejected(RejectReason::RateLimited)),
        "{err:?}"
    );
    let ev = host_event(&mut h.events, |e| matches!(e, HostEvent::AuthFailed { .. })).await;
    assert!(matches!(
        ev,
        HostEvent::AuthFailed {
            reason: RejectReason::RateLimited,
            ..
        }
    ));
    assert!(h.host.pending_requests().is_empty());
}

/// Wrong one-time code is rejected; the right one connects without a prompt and rotates.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_time_code_flow() {
    let mut h = harness(false, |_| {}).await;
    let code = h.host.one_time_code();
    let right = code.current();
    let wrong = if right == "000000" {
        "111111"
    } else {
        "000000"
    };
    let err = spawn_connect(&h, AuthMethod::OneTimeCode(wrong.into()))
        .await
        .unwrap()
        .expect_err("wrong code");
    assert!(
        matches!(err, ViewerError::Rejected(RejectReason::WrongCredentials)),
        "{err:?}"
    );
    let ev = host_event(&mut h.events, |e| matches!(e, HostEvent::AuthFailed { .. })).await;
    assert!(matches!(
        ev,
        HostEvent::AuthFailed {
            reason: RejectReason::WrongCredentials,
            ..
        }
    ));
    assert_eq!(
        code.current(),
        right,
        "one failure does not rotate the code"
    );

    let display = format!("{} {}", &right[..3], &right[3..]);
    let (session, _ev) = spawn_connect(&h, AuthMethod::OneTimeCode(display))
        .await
        .unwrap()
        .expect("right code connects");
    host_event(&mut h.events, |e| matches!(e, HostEvent::CodeChanged)).await;
    assert_ne!(code.current(), right, "code rotates after use");
    let f = first_frame(&session).await;
    assert_eq!((f.width, f.height), (W, H));
    assert_eq!(h.host.sessions().len(), 1);
    assert_eq!(
        h.host.sessions()[0].auth,
        tlp_desk_host::AuthKind::OneTimeCode
    );

    // Code access can be disabled.
    drop(session);
    let mut h2 = harness(false, |c| c.allow_one_time_code = false).await;
    let current = h2.host.one_time_code().current();
    let err = spawn_connect(&h2, AuthMethod::OneTimeCode(current))
        .await
        .unwrap()
        .expect_err("disabled");
    assert!(matches!(
        err,
        ViewerError::Rejected(RejectReason::NotAllowed)
    ));
    let _ = host_event(&mut h2.events, |e| {
        matches!(e, HostEvent::AuthFailed { .. })
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_disconnect_ends_viewer_session() {
    let mut h = harness(false, |_| {}).await;
    let conn = spawn_connect(&h, AuthMethod::AcceptPrompt);
    let req = incoming(&mut h.events).await;
    h.host.accept(req, None);
    let (session, mut vevents) = conn.await.unwrap().expect("connect");
    first_frame(&session).await;
    let id = h.host.sessions()[0].id;
    assert!(h.host.disconnect(id));
    assert_eq!(
        within("viewer end", session.ended()).await,
        ViewerEnd::Remote(ByeReason::UserClosed)
    );
    within("Ended event", async {
        loop {
            match vevents.recv().await {
                Some(ViewerEvent::Ended(r)) => {
                    assert_eq!(r, ViewerEnd::Remote(ByeReason::UserClosed));
                    return;
                }
                Some(_) => {}
                None => panic!("events closed without Ended"),
            }
        }
    })
    .await;
    assert!(!session.send_input(InputEvent::ReleaseAll));
    let ev = host_event(&mut h.events, |e| {
        matches!(e, HostEvent::SessionEnded { .. })
    })
    .await;
    assert!(matches!(
        ev,
        HostEvent::SessionEnded {
            reason: HostEnd::Local,
            ..
        }
    ));
    assert!(h.host.sessions().is_empty());
}

/// Trust on first use by address, then the remembered key is pinned: a different key at the
/// same address fails loudly with `HostKeyChanged`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tofu_remembers_key_and_detects_change() {
    let h = harness(false, |_| {}).await;
    let ConnectTarget::IdAt(host_id, addr) = h.target.clone() else {
        unreachable!()
    };
    let dir = std::env::temp_dir().join(format!("tlp-desk-viewer-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("known_hosts");
    let _ = std::fs::remove_file(&path);
    let viewer = Viewer::new(
        h.viewer_ep.clone(),
        ViewerConfig {
            known_hosts: KnownHostsStore::File(path.clone()),
            ..ViewerConfig::default()
        },
    )
    .unwrap();
    let code = h.host.one_time_code();
    let (s, _e) = viewer
        .connect(
            ConnectTarget::Addr(addr),
            AuthMethod::OneTimeCode(code.current()),
        )
        .await
        .expect("TOFU connect");
    let fp = s.peer().fingerprint;
    drop(s);
    let key = tlp_desk_viewer::addr_host_key(addr);
    assert_eq!(viewer.known_fingerprint(&key), Some(fp));
    assert_eq!(viewer.known_fingerprint(&HostKey::Id(host_id)), Some(fp));
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains(&fp.to_hex()));

    // Pretend a different key was remembered for this address.
    let bogus = Fingerprint([7; 32]);
    let HostKey::Addr(k) = &key else {
        unreachable!()
    };
    std::fs::write(&path, format!("addr:{k} {}\n", bogus.to_hex())).unwrap();
    let viewer = Viewer::new(
        h.viewer_ep.clone(),
        ViewerConfig {
            known_hosts: KnownHostsStore::File(path.clone()),
            ..ViewerConfig::default()
        },
    )
    .unwrap();
    let err = viewer
        .connect(
            ConnectTarget::Addr(addr),
            AuthMethod::OneTimeCode(code.current()),
        )
        .await
        .expect_err("key change must fail");
    match err {
        ViewerError::HostKeyChanged {
            previous,
            presented,
            presented_id,
            ..
        } => {
            assert_eq!(previous, bogus);
            assert_eq!(presented, fp);
            assert_eq!(presented_id, host_id);
        }
        other => panic!("expected HostKeyChanged, got {other:?}"),
    }
    // After the user confirms, forgetting the key lets TOFU proceed again.
    viewer.forget(&key).unwrap();
    viewer
        .connect(
            ConnectTarget::Addr(addr),
            AuthMethod::OneTimeCode(code.current()),
        )
        .await
        .expect("connect after forget");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Dialing an ID at an address whose host has a different key fails with IdentityMismatch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrong_device_id_is_rejected() {
    let h = harness(false, |_| {}).await;
    let ConnectTarget::IdAt(host_id, addr) = h.target.clone() else {
        unreachable!()
    };
    let other = Identity::generate().unwrap().device_id();
    assert_ne!(other, host_id);
    let err = h
        .viewer
        .connect(ConnectTarget::IdAt(other, addr), AuthMethod::AcceptPrompt)
        .await
        .expect_err("pin mismatch");
    assert!(
        matches!(err, ViewerError::IdentityMismatch { expected, presented } if expected == other && presented == host_id),
        "{err:?}"
    );
}

/// Non-LAN addresses are never dialed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_addresses_are_refused() {
    let h = harness(false, |_| {}).await;
    let err = h
        .viewer
        .connect(
            ConnectTarget::Addr("8.8.8.8:47400".parse().unwrap()),
            AuthMethod::AcceptPrompt,
        )
        .await
        .expect_err("refused");
    assert!(matches!(err, ViewerError::NotAllowed(_)), "{err:?}");
}

/// Real Windows capture + H.264 through the whole pipeline (input still goes to the fake
/// injector so the test never moves the real mouse).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a real interactive desktop session"]
async fn real_capture_end_to_end() {
    let mut h = harness(true, |c| c.fps = 30).await;
    let conn = spawn_connect(&h, AuthMethod::AcceptPrompt);
    let req = incoming(&mut h.events).await;
    h.host.accept(req, None);
    let (session, _ev) = conn.await.unwrap().expect("connect");
    let mon = tlp_desk_capture::list_monitors().unwrap()[0].clone();
    let f = first_frame(&session).await;
    // The default capturer downscales on the GPU to fit MAX_CAPTURE_SIZE; the encoder pads odd
    // sizes to even.
    let even = |v: u32| (v + 1) & !1;
    let (sw, sh) =
        tlp_desk_capture::fit_within((mon.width, mon.height), tlp_desk_host::MAX_CAPTURE_SIZE);
    let (ew, eh) = (even(sw), even(sh));
    println!(
        "monitor {}x{}, decoded frame {}x{}",
        mon.width, mon.height, f.width, f.height
    );
    assert!(
        (f.width, f.height) == (ew, eh)
            || (f.width.abs_diff(ew) <= 2 && f.height.abs_diff(eh) <= 2),
        "decoded {}x{}, expected about {ew}x{eh}",
        f.width,
        f.height
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    let s = session.stats();
    println!("stats after 3 s: {s:?}");
    session.disconnect();
    within("end", session.ended()).await;
}

/// Next viewer event matching `pred` (others are skipped).
async fn viewer_event(
    events: &mut ViewerEvents,
    pred: impl Fn(&ViewerEvent) -> bool,
) -> ViewerEvent {
    within("viewer event", async {
        loop {
            let ev = events.recv().await.expect("viewer event channel closed");
            if pred(&ev) {
                return ev;
            }
        }
    })
    .await
}

/// Waits for a decoded frame of the given size (older-size frames are skipped).
async fn frame_of_size(s: &ViewerSession, w: u32, h: u32) -> DecodedFrame {
    within("a frame of the new size", async {
        loop {
            if let Some(f) = s.take_frame() {
                if (f.width, f.height) == (w, h) {
                    return f;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
}

/// Two fake monitors of different sizes: the viewer receives the list, switches to monitor 1,
/// frames arrive at the new size and input is mapped to monitor 1's rectangle; an unknown
/// index is ignored.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn viewer_switches_monitor_live() {
    let mut h = harness(false, |c| {
        c.capturer_factory = Arc::new(|i| match i {
            0 => Ok(Box::new(FakeCapturer::on(fake_monitor())) as Box<dyn ScreenCapturer>),
            1 => Ok(Box::new(FakeCapturer::on(fake_monitor_2())) as Box<dyn ScreenCapturer>),
            _ => Err(CaptureError::Unsupported),
        });
        c.monitor_lister = Arc::new(|| Ok(vec![fake_monitor(), fake_monitor_2()]));
    })
    .await;
    let conn = spawn_connect(&h, AuthMethod::AcceptPrompt);
    let req = incoming(&mut h.events).await;
    h.host.accept(req, None);
    let (session, mut vevents) = conn.await.unwrap().expect("connect");

    let ev = viewer_event(&mut vevents, |e| {
        matches!(e, ViewerEvent::MonitorsChanged { .. })
    })
    .await;
    let ViewerEvent::MonitorsChanged { monitors, active } = ev else {
        unreachable!()
    };
    assert_eq!(active, 0);
    assert_eq!(monitors.len(), 2);
    assert_eq!(
        (monitors[1].index, monitors[1].width, monitors[1].height),
        (1, 480, 272)
    );
    assert_eq!(monitors[1].x, W as i32);
    assert!(monitors[0].primary && !monitors[1].primary);
    assert_eq!(session.monitors().1, 0);
    frame_of_size(&session, W, H).await;
    session.send_input(InputEvent::MouseMove { x: 1, y: 1 });
    wait_recorded(&h.recorded, |r| !r.is_empty()).await;
    assert_eq!(
        h.rects.lock().unwrap().last().copied(),
        Some(MonitorRect::new(0, 0, W, H))
    );

    assert!(session.select_monitor(1));
    viewer_event(&mut vevents, |e| {
        matches!(e, ViewerEvent::MonitorsChanged { active: 1, .. })
    })
    .await;
    assert_eq!(session.monitors().1, 1);
    let f = frame_of_size(&session, 480, 272).await;
    assert_eq!(f.pixels.len(), 480 * 272 * 4);

    let mv = InputEvent::MouseMove { x: 30000, y: 30000 };
    session.send_input(mv.clone());
    wait_recorded(&h.recorded, |r| r.last() == Some(&mv)).await;
    assert_eq!(
        h.rects.lock().unwrap().last().copied(),
        Some(MonitorRect::new(W as i32, 0, 480, 272)),
        "input must target monitor 1"
    );

    // Unknown index: ignored, the stream stays on monitor 1.
    assert!(session.select_monitor(7));
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(session.monitors().1, 1);
    frame_of_size(&session, 480, 272).await;
    while let Ok(ev) = vevents.try_recv() {
        if let ViewerEvent::MonitorsChanged { active, .. } = ev {
            assert_eq!(active, 1, "unknown index must not switch");
        }
    }

    // And back to monitor 0.
    session.select_monitor(0);
    viewer_event(&mut vevents, |e| {
        matches!(e, ViewerEvent::MonitorsChanged { active: 0, .. })
    })
    .await;
    frame_of_size(&session, W, H).await;
}

/// Local physical input on the host blocks remote input, releases it, and the viewer is told;
/// after the grace window the viewer's input flows again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_input_priority_reaches_viewer() {
    let mut h = harness(false, |c| c.local_input_grace = Duration::from_millis(400)).await;
    let conn = spawn_connect(&h, AuthMethod::AcceptPrompt);
    let req = incoming(&mut h.events).await;
    h.host.accept(req, None);
    let (session, mut vevents) = conn.await.unwrap().expect("connect");
    first_frame(&session).await;
    let key = InputEvent::Key {
        scancode: 0x1e,
        extended: false,
        down: true,
    };
    session.send_input(key.clone());
    wait_recorded(&h.recorded, |r| r.last() == Some(&key)).await;
    assert!(!session.host_input_priority());

    *h.local.0.lock().unwrap() = Some(Instant::now());
    viewer_event(&mut vevents, |e| {
        matches!(e, ViewerEvent::HostInputPriority(true))
    })
    .await;
    assert!(session.host_input_priority());
    wait_recorded(&h.recorded, |r| r.last() == Some(&InputEvent::ReleaseAll)).await;
    let blocked = InputEvent::MouseMove { x: 9, y: 9 };
    session.send_input(blocked.clone());

    viewer_event(&mut vevents, |e| {
        matches!(e, ViewerEvent::HostInputPriority(false))
    })
    .await;
    assert!(!session.host_input_priority());
    let ok = InputEvent::MouseMove { x: 7, y: 7 };
    session.send_input(ok.clone());
    wait_recorded(&h.recorded, |r| r.last() == Some(&ok)).await;
    assert!(
        !h.recorded.lock().unwrap().contains(&blocked),
        "remote input leaked during local priority"
    );
}

/// Fast capturer (one frame every 5 ms) that writes its frame number into the first pixel.
struct NumberedCapturer {
    mon: MonitorInfo,
    produced: Arc<std::sync::atomic::AtomicU32>,
    next_at: Instant,
}

impl ScreenCapturer for NumberedCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<BgraFrame>, CaptureError> {
        let now = Instant::now();
        if now < self.next_at {
            std::thread::sleep(timeout.min(self.next_at - now));
            if Instant::now() < self.next_at {
                return Ok(None);
            }
        }
        self.next_at = Instant::now() + Duration::from_millis(5);
        let n = self
            .produced
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        let mut f = BgraFrame::new_tight(self.mon.width, self.mon.height);
        for (i, b) in f.data.iter_mut().enumerate() {
            *b = (i / 4 % 251) as u8;
        }
        f.data[..4].copy_from_slice(&n.to_le_bytes());
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

/// Real H.264 encoder that takes 80 ms per frame and records, for each encoded frame, its
/// number and how many frames the capturer had produced by then.
struct SlowEncoder {
    inner: Box<dyn tlp_desk_codec::VideoEncoder>,
    produced: Arc<std::sync::atomic::AtomicU32>,
    log: Arc<Mutex<Vec<(u32, u32)>>>,
}

impl tlp_desk_codec::VideoEncoder for SlowEncoder {
    fn encode(
        &mut self,
        frame: &BgraFrame,
    ) -> Result<Option<tlp_desk_codec::EncodedFrame>, tlp_desk_codec::CodecError> {
        let n = u32::from_le_bytes(frame.data[..4].try_into().unwrap());
        let now = self.produced.load(std::sync::atomic::Ordering::SeqCst);
        self.log.lock().unwrap().push((n, now));
        std::thread::sleep(Duration::from_millis(80));
        self.inner.encode(frame)
    }
    fn request_keyframe(&mut self) {
        self.inner.request_keyframe();
    }
    fn set_bitrate(&mut self, kbps: u32) -> Result<(), tlp_desk_codec::CodecError> {
        self.inner.set_bitrate(kbps)
    }
    fn config(&self) -> tlp_desk_codec::EncoderConfig {
        self.inner.config()
    }
}

/// Capture and encode run on separate threads with a newest-wins hand-over: with an encoder
/// far slower than the capturer, frames are dropped (not queued) and every encoded frame is
/// (nearly) the newest one captured, so latency cannot build up.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_encoder_drops_frames_instead_of_queueing() {
    let produced = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let log: Arc<Mutex<Vec<(u32, u32)>>> = Arc::default();
    let (p1, p2, log2) = (produced.clone(), produced.clone(), log.clone());
    let mut h = harness(false, move |c| {
        c.fps = 60;
        c.capturer_factory = Arc::new(move |_| {
            Ok(Box::new(NumberedCapturer {
                mon: fake_monitor(),
                produced: p1.clone(),
                next_at: Instant::now(),
            }) as Box<dyn ScreenCapturer>)
        });
        c.encoder_factory = Arc::new(move |cfg| {
            Ok(Box::new(SlowEncoder {
                inner: tlp_desk_codec::new_encoder(cfg)?,
                produced: p2.clone(),
                log: log2.clone(),
            }) as Box<dyn tlp_desk_codec::VideoEncoder>)
        });
    })
    .await;
    let conn = spawn_connect(&h, AuthMethod::AcceptPrompt);
    let req = incoming(&mut h.events).await;
    h.host.accept(req, None);
    let (session, _ev) = conn.await.unwrap().expect("connect");
    first_frame(&session).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let log = log.lock().unwrap().clone();
    let total = produced.load(std::sync::atomic::Ordering::SeqCst);
    println!("captured {total}, encoded {}: {log:?}", log.len());
    assert!(log.len() >= 10, "encoder starved: {log:?}");
    // At 80 ms per encode only ~1 in 16 captured frames can be encoded.
    assert!(
        (log.len() as u32) * 4 < total,
        "{} encoded of {total} captured: frames were not dropped",
        log.len()
    );
    for w in log.windows(2) {
        assert!(w[1].0 > w[0].0, "frames must be encoded in order: {log:?}");
    }
    // Newest wins: the encoded frame is at most a few capture periods old when encoding starts
    // (a queue would make this gap grow by ~15 frames per encode).
    for &(n, now) in &log[1..] {
        assert!(
            now - n <= 4,
            "encoded frame {n} while {now} were captured: queued"
        );
    }
    assert!(session.stats().frames_decoded >= 5, "{:?}", session.stats());
    session.disconnect();
    within("end", session.ended()).await;
}

/// The viewer maps the pointer by its position over the displayed image (normalized), and the
/// host maps normalized coordinates over the real monitor rectangle, so a 1920x1200 video of a
/// 3840x2400 monitor still clicks on the right physical pixel.
#[test]
fn scaled_video_maps_clicks_to_physical_monitor_pixels() {
    use tlp_desk_input::ViewRect;
    let monitor = MonitorRect::new(-3840, 0, 3840, 2400); // e.g. left of the primary
    let (vw, vh) = tlp_desk_capture::fit_within((3840, 2400), tlp_desk_host::MAX_CAPTURE_SIZE);
    assert_eq!((vw, vh), (1920, 1200));
    // The viewer shows the video letterboxed in a 1000x700 window area.
    let shown = ViewRect::fit(ViewRect::new(0.0, 0.0, 1000.0, 700.0), vw, vh);
    for &(fx, fy) in &[(0.0f32, 0.0f32), (0.25, 0.75), (0.5, 0.5), (0.999, 0.999)] {
        let px = shown.x + fx * shown.width;
        let py = shown.y + fy * shown.height;
        let (nx, ny) = shown.point_to_normalized(px, py, vw, vh);
        let (hx, hy) = monitor.normalized_to_pixel(nx, ny);
        let want_x = monitor.x as f32 + fx * monitor.width as f32;
        let want_y = monitor.y as f32 + fy * monitor.height as f32;
        // One video pixel covers 2x2 monitor pixels (plus one window point of rounding).
        let tol = 2.0 * 3840.0 / shown.width + 2.0;
        assert!(
            (hx as f32 - want_x).abs() <= tol && (hy as f32 - want_y).abs() <= tol,
            "({fx},{fy}) -> ({hx},{hy}), want about ({want_x},{want_y})"
        );
        assert!(monitor.contains(hx, hy));
    }
    // Corners land exactly on the monitor's first and last pixels.
    let (nx, ny) = shown.point_to_normalized(shown.x, shown.y, vw, vh);
    assert_eq!(monitor.normalized_to_pixel(nx, ny), (-3840, 0));
    let (nx, ny) = shown.point_to_normalized(
        shown.x + shown.width - 0.01,
        shown.y + shown.height - 0.01,
        vw,
        vh,
    );
    assert_eq!(monitor.normalized_to_pixel(nx, ny), (-1, 2399));
}

/// Next HostEvent::Feature (other events are skipped).
async fn next_feature_event(
    events: &mut HostEvents,
) -> (tlp_desk_host::SessionId, tlp_desk_host::FeatureEvent) {
    match host_event(events, |e| matches!(e, HostEvent::Feature { .. })).await {
        HostEvent::Feature { id, event } => (id, event),
        _ => unreachable!(),
    }
}

/// Test feature: records the chat texts it receives and answers each with "ack: <text>".
struct ChatProbe {
    name: &'static str,
    reply: bool,
}

impl tlp_desk_host::Feature for ChatProbe {
    fn name(&self) -> &'static str {
        self.name
    }

    fn handles(&self, msg: &tlp_desk_proto::Message) -> bool {
        matches!(msg, tlp_desk_proto::Message::ChatMessage { .. })
    }

    fn start(
        &self,
        ctx: tlp_desk_host::FeatureCtx,
        mut inbox: tokio::sync::mpsc::UnboundedReceiver<tlp_desk_proto::Message>,
    ) {
        let (name, reply) = (self.name, self.reply);
        tokio::spawn(async move {
            while let Some(tlp_desk_proto::Message::ChatMessage { id, text, .. }) =
                inbox.recv().await
            {
                ctx.emit(name, text.clone());
                if reply {
                    let ack = tlp_desk_proto::Message::ChatMessage {
                        id,
                        text: format!("ack: {text}"),
                        sent_unix_ms: 0,
                    };
                    let _ = ctx.session.send(&ack).await;
                }
            }
        });
    }
}

/// Features plug in without touching the session loops: the host routes a viewer's chat to
/// its feature only while the session holds `CHAT`, and the viewer routes the host's answer
/// to its own feature; both surface as `Feature` events.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn features_receive_permitted_messages_on_both_sides() {
    let host_chat: Arc<dyn tlp_desk_host::Feature> = Arc::new(ChatProbe {
        name: "chat",
        reply: true,
    });
    let mut h = harness(false, |cfg| {
        cfg.features = tlp_desk_host::FeatureSet::new().with(host_chat);
    })
    .await;
    let viewer_chat: Arc<dyn tlp_desk_viewer::Feature> = Arc::new(ChatProbe {
        name: "chat-viewer",
        reply: false,
    });
    let viewer = Viewer::new(
        h.viewer_ep.clone(),
        ViewerConfig {
            known_hosts: KnownHostsStore::InMemory,
            features: tlp_desk_viewer::FeatureSet::new().with(viewer_chat),
            ..ViewerConfig::default()
        },
    )
    .unwrap();
    let target = h.target.clone();
    let conn = tokio::spawn(async move { viewer.connect(target, AuthMethod::AcceptPrompt).await });
    let req = incoming(&mut h.events).await;
    let with_chat = tlp_desk_host::DEFAULT_PERMISSIONS | Permissions::CHAT;
    assert!(h.host.accept(req, Some(with_chat)));
    let (session, mut vevents) = conn.await.unwrap().expect("connect");
    let HostEvent::SessionStarted(cv) =
        host_event(&mut h.events, |e| matches!(e, HostEvent::SessionStarted(_))).await
    else {
        unreachable!()
    };
    let chat = |text: &str| tlp_desk_proto::Message::ChatMessage {
        id: 1,
        text: text.into(),
        sent_unix_ms: 0,
    };
    let wait_chat_perm = |want: bool| {
        let s = &session;
        within("viewer sees the CHAT change", async move {
            while s.permissions().contains(Permissions::CHAT) != want {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
    };

    // Routed while CHAT is held.
    assert!(session.send_message(chat("مرحبا")));
    let (id, event) = next_feature_event(&mut h.events).await;
    assert_eq!(
        (id, event.feature, event.session_key),
        (cv.id, "chat", cv.id.0)
    );
    assert_eq!(event.payload::<String>().map(String::as_str), Some("مرحبا"));

    // Revoked: the host updates its gate before telling the viewer, so anything sent after the
    // viewer saw the change is checked against the new permissions and dropped. (The viewer's
    // feature still gets the earlier answer below.)
    assert!(h
        .host
        .set_permissions(cv.id, tlp_desk_host::DEFAULT_PERMISSIONS));
    wait_chat_perm(false).await;
    assert!(session.send_message(chat("blocked")));
    let leaked = tokio::time::timeout(
        Duration::from_millis(500),
        next_feature_event(&mut h.events),
    )
    .await;
    assert!(
        leaked.is_err(),
        "a message without CHAT reached the feature: {leaked:?}"
    );
    let ev = within("viewer feature event", async {
        loop {
            if let Some(ViewerEvent::Feature(e)) = vevents.recv().await {
                return e;
            }
        }
    })
    .await;
    assert_eq!(ev.feature, "chat-viewer");
    assert_eq!(ev.session_key, session.feature_key());
    assert_eq!(
        ev.payload::<String>().map(String::as_str),
        Some("ack: مرحبا")
    );
    session.disconnect();
}

/// Settings changed while the host runs (`Host::set_live_options`) apply to the next viewer:
/// code access can be switched off and on, and new sessions get the new default permissions.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_options_apply_to_the_next_session() {
    let mut h = harness(false, |_| {}).await;
    let base = h.host.live_options();
    h.host.set_live_options(tlp_desk_host::LiveOptions {
        allow_one_time_code: false,
        ..base
    });
    let err = spawn_connect(
        &h,
        AuthMethod::OneTimeCode(h.host.one_time_code().current()),
    )
    .await
    .unwrap()
    .expect_err("code access switched off");
    assert!(
        matches!(err, ViewerError::Rejected(RejectReason::NotAllowed)),
        "{err:?}"
    );
    let _ = host_event(&mut h.events, |e| matches!(e, HostEvent::AuthFailed { .. })).await;

    h.host.set_live_options(tlp_desk_host::LiveOptions {
        allow_one_time_code: true,
        default_permissions: Permissions::VIEW,
        ..base
    });
    assert_eq!(h.host.live_options().default_permissions, Permissions::VIEW);
    let (session, _ev) = spawn_connect(
        &h,
        AuthMethod::OneTimeCode(h.host.one_time_code().current()),
    )
    .await
    .unwrap()
    .expect("code access switched back on");
    assert_eq!(session.permissions(), Permissions::VIEW);
    assert_eq!(h.host.sessions()[0].permissions, Permissions::VIEW);
    session.disconnect();
}

/// Fake device-info source: fixed Arabic data, counts how often it was asked.
struct FakeInfo(Arc<std::sync::atomic::AtomicUsize>);

impl tlp_desk_host::DeviceInfoSource for FakeInfo {
    fn collect(&self) -> tlp_desk_proto::DeviceInfo {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        fake_device_info()
    }
}

fn fake_device_info() -> tlp_desk_proto::DeviceInfo {
    tlp_desk_proto::DeviceInfo {
        os: "Windows 11 Pro".into(),
        version: "10.0.26100".into(),
        cpu: "Fake CPU @ 3.0GHz".into(),
        cores: 12,
        ram_bytes: 32 << 30,
        disks: vec![tlp_desk_proto::DiskInfo {
            mount: "C:\\".into(),
            label: "قرص النظام".into(),
            total_bytes: 512 << 30,
            free_bytes: 128 << 30,
        }],
        uptime_s: 90_061,
        hostname: "حاسوب-المحاسبة".into(),
    }
}

/// Device info end to end: the viewer asks, the host's feature answers from its source (Arabic
/// host name and disk label intact), the host UI hears about it, and a second request within the
/// cache window is answered without collecting again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn device_info_request_is_answered_and_cached() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let feature = tlp_desk_host::DeviceInfoFeature::new(Arc::new(FakeInfo(calls.clone())))
        .with_min_interval(Duration::from_secs(60));
    let mut h = harness(false, |cfg| {
        cfg.features = tlp_desk_host::FeatureSet::new().with(Arc::new(feature));
    })
    .await;
    let viewer = Viewer::new(
        h.viewer_ep.clone(),
        ViewerConfig {
            known_hosts: KnownHostsStore::InMemory,
            features: tlp_desk_viewer::FeatureSet::new()
                .with(Arc::new(tlp_desk_viewer::DeviceInfoClient)),
            ..ViewerConfig::default()
        },
    )
    .unwrap();
    let target = h.target.clone();
    let conn = tokio::spawn(async move { viewer.connect(target, AuthMethod::AcceptPrompt).await });
    let req = incoming(&mut h.events).await;
    assert!(h.host.accept(req, Some(Permissions::VIEW)));
    let (session, mut vevents) = conn.await.unwrap().expect("connect");

    async fn next_info(vevents: &mut ViewerEvents) -> tlp_desk_proto::DeviceInfo {
        within("device info on the viewer", async move {
            loop {
                if let Some(ViewerEvent::Feature(e)) = vevents.recv().await {
                    if e.feature == tlp_desk_viewer::DEVICE_INFO_FEATURE {
                        return e.payload::<tlp_desk_proto::DeviceInfo>().cloned().unwrap();
                    }
                }
            }
        })
        .await
    }
    assert!(tlp_desk_viewer::request_device_info(&session));
    assert_eq!(next_info(&mut vevents).await, fake_device_info());
    let (_, served) = next_feature_event(&mut h.events).await;
    assert_eq!(served.feature, tlp_desk_host::DEVICE_INFO_FEATURE);
    assert!(served
        .payload::<tlp_desk_host::DeviceInfoServed>()
        .is_some());

    assert!(tlp_desk_viewer::request_device_info(&session));
    let again = next_info(&mut vevents).await;
    assert_eq!(again.hostname, "حاسوب-المحاسبة");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    session.disconnect();
}
