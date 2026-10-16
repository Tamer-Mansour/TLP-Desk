//! Several viewers on one host at the same time (the "two colleagues with full control"
//! scenario): every viewer gets its own video, its own input path and its own permissions,
//! and one viewer leaving does not disturb the others. Loopback QUIC, fake capturer and
//! recording injector only: nothing touches the real screen, mouse or keyboard.

use std::future::Future;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tlp_desk_capture::{BgraFrame, CaptureError, CursorState, MonitorInfo, ScreenCapturer};
use tlp_desk_crypto::Identity;
use tlp_desk_host::{ConnectedViewer, Host, HostConfig, HostEvent, HostEvents, SessionId};
use tlp_desk_input::{InputError, InputEvent, InputInjector, LocalActivity, MonitorRect};
use tlp_desk_net::{Endpoint, EndpointConfig};
use tlp_desk_proto::Permissions;
use tlp_desk_viewer::{
    AuthMethod, ConnectTarget, DecodedFrame, KnownHostsStore, PixelFormat, Viewer, ViewerConfig,
    ViewerEvent, ViewerEvents, ViewerSession,
};

const W: u32 = 320;
const H: u32 = 240;
const WAIT: Duration = Duration::from_secs(20);

fn monitor(index: u32) -> MonitorInfo {
    match index {
        0 => MonitorInfo {
            index: 0,
            name: "FAKE".into(),
            width: W,
            height: H,
            primary: true,
            x: 0,
            y: 0,
        },
        _ => MonitorInfo {
            index: 1,
            name: "FAKE-2".into(),
            width: 480,
            height: 272,
            primary: false,
            x: W as i32,
            y: 0,
        },
    }
}

/// Synthetic screen with a moving bar (~60 fps).
struct FakeCapturer {
    mon: MonitorInfo,
    n: u32,
    next_at: Instant,
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
                let v = if x >= bar && x < bar + 32 {
                    230
                } else {
                    (y % 64) as u8
                };
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

#[derive(Default)]
struct NoLocalInput;

impl LocalActivity for NoLocalInput {
    fn last_physical_input(&self) -> Option<Instant> {
        None
    }
}

struct FakeInjector(Recorded);

impl InputInjector for FakeInjector {
    fn inject(&mut self, event: &InputEvent) -> Result<(), InputError> {
        self.0.lock().unwrap().push(event.clone());
        Ok(())
    }
}

fn endpoint(listen: bool, name: &str) -> Arc<Endpoint> {
    Arc::new(
        Endpoint::bind(
            Arc::new(Identity::generate().unwrap()),
            EndpointConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                listen,
                device_name: name.into(),
                ..EndpointConfig::default()
            },
        )
        .unwrap(),
    )
}

struct Rig {
    host: Host,
    events: HostEvents,
    target: ConnectTarget,
    recorded: Recorded,
    /// How many capturers the host opened (one per session and monitor switch today).
    captures_opened: Arc<AtomicU32>,
}

async fn rig(tweak: impl FnOnce(&mut HostConfig)) -> Rig {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_test_writer()
        .try_init();
    let recorded: Recorded = Arc::default();
    let rec = recorded.clone();
    let captures_opened = Arc::new(AtomicU32::new(0));
    let opened = captures_opened.clone();
    let mut cfg = HostConfig {
        advertise: false,
        injector_factory: Arc::new(move |_r: MonitorRect| {
            Ok(Box::new(FakeInjector(rec.clone())) as Box<dyn InputInjector>)
        }),
        local_activity_factory: Arc::new(|| Ok(Arc::new(NoLocalInput) as Arc<dyn LocalActivity>)),
        remote_pointer_marker: false,
        capturer_factory: Arc::new(move |i, _| {
            if i > 1 {
                return Err(CaptureError::Unsupported);
            }
            opened.fetch_add(1, Ordering::Relaxed);
            Ok(Box::new(FakeCapturer {
                mon: monitor(i),
                n: 0,
                next_at: Instant::now(),
            }) as Box<dyn ScreenCapturer>)
        }),
        monitor_lister: Arc::new(|| Ok(vec![monitor(0), monitor(1)])),
        ..HostConfig::default()
    };
    tweak(&mut cfg);
    let host_ep = endpoint(true, "host-pc");
    let target = ConnectTarget::IdAt(host_ep.device_id(), host_ep.local_addr().unwrap());
    let (host, events) = Host::start(host_ep, cfg).await.unwrap();
    Rig {
        host,
        events,
        target,
        recorded,
        captures_opened,
    }
}

fn viewer(name: &str) -> Arc<Viewer> {
    Arc::new(
        Viewer::new(
            endpoint(false, name),
            ViewerConfig {
                known_hosts: KnownHostsStore::InMemory,
                pixel_format: PixelFormat::Rgba,
                ..ViewerConfig::default()
            },
        )
        .unwrap(),
    )
}

async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, fut)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

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

/// Connects `v` through the accept prompt, accepting with `perms`. Returns the session, its
/// events and the host-side entry.
async fn join(
    rig: &mut Rig,
    v: &Arc<Viewer>,
    perms: Permissions,
) -> (ViewerSession, ViewerEvents, ConnectedViewer) {
    let (viewer, target) = (v.clone(), rig.target.clone());
    let conn = tokio::spawn(async move { viewer.connect(target, AuthMethod::AcceptPrompt).await });
    let HostEvent::IncomingRequest(req) =
        host_event(&mut rig.events, |e| matches!(e, HostEvent::IncomingRequest(_))).await
    else {
        unreachable!()
    };
    assert!(rig.host.accept(req.id, Some(perms)));
    let (session, events) = conn.await.unwrap().expect("connect");
    let HostEvent::SessionStarted(cv) =
        host_event(&mut rig.events, |e| matches!(e, HostEvent::SessionStarted(_))).await
    else {
        unreachable!()
    };
    (session, events, cv)
}

async fn frame_of_size(s: &ViewerSession, w: u32, h: u32) -> DecodedFrame {
    within("a frame of the expected size", async {
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

/// Frames keep coming: two frames with growing sequence numbers.
async fn streaming(s: &ViewerSession, w: u32, h: u32) {
    let a = frame_of_size(s, w, h).await;
    let b = frame_of_size(s, w, h).await;
    assert!(b.seq > a.seq);
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

fn mv(x: u16) -> InputEvent {
    InputEvent::MouseMove { x, y: 7 }
}

fn has(rec: &[InputEvent], ev: &InputEvent) -> bool {
    rec.iter().any(|e| e == ev)
}

const FULL: Permissions =
    Permissions(Permissions::VIEW.0 | Permissions::CONTROL.0 | Permissions::KEYBOARD.0);

/// Two viewers with full control at once: both stream video and both inject input; their
/// permissions are independent; when one leaves, the other keeps streaming and controlling.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_viewers_with_full_control_at_once() {
    let mut rig = rig(|_| {}).await;
    let (va, vb) = (viewer("زميل-1"), viewer("colleague-2"));
    let (sa, _ea, ca) = join(&mut rig, &va, FULL).await;
    let (sb, mut eb, cb) = join(&mut rig, &vb, FULL).await;
    assert_ne!(ca.id, cb.id);
    let who = rig.host.sessions();
    assert_eq!(who.len(), 2, "the banner must list both viewers");
    assert_eq!(who[0].peer.name, "زميل-1");

    streaming(&sa, W, H).await;
    streaming(&sb, W, H).await;

    // Both inject: their events interleave in the host's injectors.
    let key_a = InputEvent::Key {
        scancode: 0x1e,
        extended: false,
        down: true,
    };
    for i in 0..5 {
        assert!(sa.send_input(mv(1000 + i)));
        assert!(sb.send_input(mv(2000 + i)));
    }
    sa.send_input(key_a.clone());
    wait_recorded(&rig.recorded, |r| {
        has(r, &mv(1004)) && has(r, &mv(2004)) && has(r, &key_a)
    })
    .await;

    // Per-viewer permissions: B becomes view-only, A keeps full control.
    assert!(rig.host.set_permissions(cb.id, Permissions::VIEW));
    within("B told view-only", async {
        loop {
            if let Some(ViewerEvent::PermissionsChanged(p)) = eb.recv().await {
                assert_eq!(p, Permissions::VIEW);
                return;
            }
        }
    })
    .await;
    assert_eq!(sa.permissions(), FULL);
    sb.send_input(mv(2100));
    sa.send_input(mv(1100));
    wait_recorded(&rig.recorded, |r| has(r, &mv(1100))).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !has(&rig.recorded.lock().unwrap(), &mv(2100)),
        "view-only viewer's input leaked"
    );

    // A leaves; B keeps streaming and, once control is given back, controlling.
    sa.disconnect();
    let HostEvent::SessionEnded { id, .. } =
        host_event(&mut rig.events, |e| matches!(e, HostEvent::SessionEnded { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(id, ca.id);
    assert_eq!(rig.host.sessions().len(), 1);
    streaming(&sb, W, H).await;
    assert!(rig.host.set_permissions(cb.id, FULL));
    within("B gets control back", async {
        loop {
            if sb.permissions() == FULL {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    sb.send_input(mv(2200));
    wait_recorded(&rig.recorded, |r| has(r, &mv(2200))).await;
    assert!(sb.end_reason().is_none(), "B must not be affected by A leaving");
}

/// Each session has its own capture pipeline: one viewer switching monitors does not move
/// the other viewer's picture or input target.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn monitor_switch_by_one_viewer_does_not_affect_the_other() {
    let mut rig = rig(|_| {}).await;
    let (va, vb) = (viewer("viewer-a"), viewer("viewer-b"));
    let (sa, _ea, ca) = join(&mut rig, &va, FULL).await;
    let (sb, _eb, cb) = join(&mut rig, &vb, FULL).await;
    streaming(&sa, W, H).await;
    streaming(&sb, W, H).await;

    assert!(sa.select_monitor(1));
    frame_of_size(&sa, 480, 272).await;
    within("A's input mapped to monitor 1", async {
        loop {
            if rig.host.streamed_monitor(ca.id) == Some(MonitorRect::new(W as i32, 0, 480, 272)) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    // B still sees and controls monitor 0.
    streaming(&sb, W, H).await;
    assert_eq!(sb.monitors().1, 0);
    assert_eq!(
        rig.host.streamed_monitor(cb.id),
        Some(MonitorRect::new(0, 0, W, H))
    );
    assert!(rig.captures_opened.load(Ordering::Relaxed) >= 3);
}

/// Revoking VIEW from one viewer ends only that viewer's session.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ending_one_session_keeps_the_others() {
    let mut rig = rig(|_| {}).await;
    let (va, vb) = (viewer("viewer-a"), viewer("viewer-b"));
    let (sa, _ea, ca) = join(&mut rig, &va, FULL).await;
    let (sb, _eb, _cb) = join(&mut rig, &vb, FULL).await;
    assert!(rig.host.disconnect(ca.id));
    within("A ended", sa.ended()).await;
    streaming(&sb, W, H).await;
    let left: Vec<SessionId> = rig.host.sessions().iter().map(|s| s.id).collect();
    assert_eq!(left.len(), 1);
    assert_ne!(left[0], ca.id);
}

/// The host user pauses one viewer's input: that viewer's events are dropped (held keys
/// released) and it is shown the priority notice, the other viewer keeps control, and
/// resuming lets the paused viewer's input through again. Permissions do not change.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pausing_one_viewers_input_keeps_the_other_in_control() {
    let mut rig = rig(|_| {}).await;
    let (va, vb) = (viewer("viewer-a"), viewer("viewer-b"));
    let (sa, mut ea, ca) = join(&mut rig, &va, FULL).await;
    let (sb, _eb, _cb) = join(&mut rig, &vb, FULL).await;
    streaming(&sa, W, H).await;

    assert!(rig.host.set_input_paused(ca.id, true));
    assert_eq!(rig.host.input_paused(ca.id), Some(true));
    assert!(!rig.host.set_input_paused(SessionId(9999), true));
    within("A told input is paused", async {
        loop {
            if let Some(ViewerEvent::HostInputPriority(true)) = ea.recv().await {
                return;
            }
        }
    })
    .await;
    wait_recorded(&rig.recorded, |r| r.contains(&InputEvent::ReleaseAll)).await;
    sa.send_input(mv(1300));
    sb.send_input(mv(2300));
    wait_recorded(&rig.recorded, |r| has(r, &mv(2300))).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !has(&rig.recorded.lock().unwrap(), &mv(1300)),
        "paused viewer's input leaked"
    );
    assert_eq!(rig.host.sessions()[0].permissions, FULL);

    assert!(rig.host.set_input_paused(ca.id, false));
    within("A told input resumed", async {
        loop {
            if let Some(ViewerEvent::HostInputPriority(false)) = ea.recv().await {
                return;
            }
        }
    })
    .await;
    sa.send_input(mv(1400));
    wait_recorded(&rig.recorded, |r| has(r, &mv(1400))).await;
}
