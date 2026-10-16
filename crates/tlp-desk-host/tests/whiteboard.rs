//! Whiteboard end to end over loopback QUIC: a real host and viewer, a synthetic capturer and
//! a fake overlay that renders offscreen (no window ever appears on the developer's screen).

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tlp_desk_capture::{BgraFrame, CaptureError, CursorState, MonitorInfo, ScreenCapturer};
use tlp_desk_crypto::Identity;
use tlp_desk_host::{
    Host, HostConfig, HostEvent, HostEvents, SessionId, WhiteboardHost, WhiteboardHostEvent,
    DEFAULT_PERMISSIONS,
};
use tlp_desk_input::whiteboard::{
    lock_board, render_full, ChannelOrder, ClearRequestFn, SharedBoard, WhiteboardOp,
    WhiteboardOverlay, FEATURE_NAME, MAX_STROKES, MAX_WIDTH, MAX_WIRE_POINTS,
};
use tlp_desk_input::{InputError, InputEvent, InputInjector, LocalActivity, MonitorRect};
use tlp_desk_net::{Endpoint, EndpointConfig, FeatureSet};
use tlp_desk_proto::{Message, Permissions};
use tlp_desk_viewer::{
    AuthMethod, ConnectTarget, KnownHostsStore, PixelFormat, Viewer, ViewerConfig, ViewerEvent,
    ViewerEvents, ViewerSession, WhiteboardEvent, WhiteboardViewer,
};

const WAIT: Duration = Duration::from_secs(15);

/// The fake host monitor: left of the primary and lower, so mapping must use its origin.
fn monitor() -> MonitorInfo {
    MonitorInfo {
        index: 0,
        name: "FAKE".into(),
        width: 320,
        height: 240,
        primary: true,
        x: -320,
        y: 40,
    }
}

fn monitor_rect() -> MonitorRect {
    MonitorRect::new(-320, 40, 320, 240)
}

struct FakeCapturer {
    mon: MonitorInfo,
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
        self.next_at = Instant::now() + Duration::from_millis(30);
        let mut f = BgraFrame::new_tight(self.mon.width, self.mon.height);
        for y in 0..self.mon.height {
            for px in f.row_mut(y).chunks_exact_mut(4) {
                px.copy_from_slice(&[40, 40, 40, 255]);
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

/// Counts injected events: a `MouseMove` sent after other control messages is a barrier that
/// proves the host core processed them (the control stream is ordered).
struct CountingInput(Arc<AtomicUsize>);

impl InputInjector for CountingInput {
    fn inject(&mut self, _: &InputEvent) -> Result<(), InputError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct NoLocal;

impl LocalActivity for NoLocal {
    fn last_physical_input(&self) -> Option<Instant> {
        None
    }
}

/// What the fake overlays saw.
#[derive(Default)]
struct OverlayLog {
    created: usize,
    alive: usize,
    targets: Vec<MonitorRect>,
    board: Option<SharedBoard>,
    on_clear: Option<ClearRequestFn>,
    /// Inked pixels and their centroid in the last offscreen render.
    inked: usize,
    centroid: (f64, f64),
}

type Log = Arc<Mutex<OverlayLog>>;

/// Renders the board offscreen on every change, like the real window would.
struct FakeOverlay {
    log: Log,
    board: SharedBoard,
    target: Option<MonitorRect>,
}

impl WhiteboardOverlay for FakeOverlay {
    fn set_target(&mut self, rect: MonitorRect) {
        self.target = Some(rect);
        self.log.lock().unwrap().targets.push(rect);
    }

    fn changed(&mut self) {
        let Some(t) = self.target else { return };
        let pix = render_full(
            &lock_board(&self.board),
            t.width,
            t.height,
            ChannelOrder::Rgba,
        )
        .expect("pixmap");
        let (mut n, mut sx, mut sy) = (0usize, 0.0, 0.0);
        for (i, px) in pix.data().chunks_exact(4).enumerate() {
            if px[3] > 0 {
                n += 1;
                sx += (i as u32 % t.width) as f64;
                sy += (i as u32 / t.width) as f64;
            }
        }
        let mut log = self.log.lock().unwrap();
        log.inked = n;
        log.centroid = if n > 0 {
            (sx / n as f64, sy / n as f64)
        } else {
            (0.0, 0.0)
        };
    }
}

impl Drop for FakeOverlay {
    fn drop(&mut self) {
        self.log.lock().unwrap().alive -= 1;
    }
}

struct Harness {
    host: Host,
    events: HostEvents,
    log: Log,
    session: ViewerSession,
    viewer_events: ViewerEvents,
    host_session: SessionId,
    injected: Arc<AtomicUsize>,
    _viewer: Viewer,
}

impl Harness {
    /// Waits until the host core has handled every control message sent so far.
    async fn barrier(&self) {
        let before = self.injected.load(Ordering::SeqCst);
        assert!(self
            .session
            .send_input(InputEvent::MouseMove { x: 1, y: 1 }));
        until("input barrier", || {
            self.injected.load(Ordering::SeqCst) > before
        })
        .await;
    }
}

fn endpoint(listen: bool) -> Arc<Endpoint> {
    Arc::new(
        Endpoint::bind(
            Arc::new(Identity::generate().unwrap()),
            EndpointConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                listen,
                device_name: if listen {
                    "host-pc"
                } else {
                    "حاسوب المشاهد"
                }
                .into(),
                ..EndpointConfig::default()
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

async fn until(what: &str, cond: impl Fn() -> bool) {
    within(what, async {
        while !cond() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
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

async fn viewer_event(events: &mut ViewerEvents, pred: impl Fn(&ViewerEvent) -> bool) {
    within("viewer event", async {
        loop {
            let ev = events.recv().await.expect("viewer events closed");
            if pred(&ev) {
                return;
            }
        }
    })
    .await
}

fn is_whiteboard_event(ev: &HostEvent, want: WhiteboardHostEvent) -> bool {
    matches!(ev, HostEvent::Feature { event, .. }
        if event.feature == FEATURE_NAME && event.payload::<WhiteboardHostEvent>() == Some(&want))
}

/// Host with the whiteboard (fake overlay) and a connected viewer holding `perms`, after the
/// first video frame (capture runs, so the streamed rectangle is known).
async fn connect(perms: Permissions) -> Harness {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_test_writer()
        .try_init();
    let log: Log = Arc::default();
    let log2 = log.clone();
    let injected = Arc::new(AtomicUsize::new(0));
    let injected2 = injected.clone();
    let wb = Arc::new(WhiteboardHost::with_overlay_factory(Arc::new(
        move |board: SharedBoard, on_clear: ClearRequestFn| {
            let mut l = log2.lock().unwrap();
            l.created += 1;
            l.alive += 1;
            l.board = Some(board.clone());
            l.on_clear = Some(on_clear);
            Ok(Box::new(FakeOverlay {
                log: log2.clone(),
                board,
                target: None,
            }) as Box<dyn WhiteboardOverlay>)
        },
    )));
    let cfg = HostConfig {
        advertise: false,
        capturer_factory: Arc::new(|_, _| {
            Ok(Box::new(FakeCapturer {
                mon: monitor(),
                next_at: Instant::now(),
            }) as Box<dyn ScreenCapturer>)
        }),
        monitor_lister: Arc::new(|| Ok(vec![monitor()])),
        injector_factory: Arc::new(move |_| {
            Ok(Box::new(CountingInput(injected2.clone())) as Box<dyn InputInjector>)
        }),
        local_activity_factory: Arc::new(|| Ok(Arc::new(NoLocal) as Arc<dyn LocalActivity>)),
        remote_pointer_marker: false,
        features: FeatureSet::new().with(wb.clone()),
        ..HostConfig::default()
    };
    let host_ep = endpoint(true);
    let target = ConnectTarget::IdAt(host_ep.device_id(), host_ep.local_addr().unwrap());
    let (host, mut events) = Host::start(host_ep, cfg).await.unwrap();
    wb.set_monitor_source(host.streamed_monitor_fn());
    let viewer = Viewer::new(
        endpoint(false),
        ViewerConfig {
            known_hosts: KnownHostsStore::InMemory,
            pixel_format: PixelFormat::Rgba,
            features: FeatureSet::new().with(Arc::new(WhiteboardViewer)),
            ..ViewerConfig::default()
        },
    )
    .unwrap();
    let conn = viewer.connect(target, AuthMethod::AcceptPrompt);
    let answer = async {
        let id = match host_event(&mut events, |e| matches!(e, HostEvent::IncomingRequest(_))).await
        {
            HostEvent::IncomingRequest(r) => r.id,
            _ => unreachable!(),
        };
        assert!(host.accept(id, Some(perms)));
    };
    let (res, ()) = within("connect", async { tokio::join!(conn, answer) }).await;
    let (session, viewer_events) = res.expect("connected");
    let host_session =
        match host_event(&mut events, |e| matches!(e, HostEvent::SessionStarted(_))).await {
            HostEvent::SessionStarted(v) => v.id,
            _ => unreachable!(),
        };
    within("first frame", async {
        while session.take_frame().is_none() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    Harness {
        host,
        events,
        log,
        session,
        viewer_events,
        host_session,
        injected,
        _viewer: viewer,
    }
}

fn stroke(id: u64, points: Vec<(u16, u16)>) -> Message {
    Message::WhiteboardStroke {
        id,
        points,
        color_rgba: 0xE53935FF,
        width: 400,
    }
}

fn board_ids(log: &Log) -> Vec<u64> {
    let l = log.lock().unwrap();
    l.board
        .as_ref()
        .map(|b| lock_board(b).strokes().iter().map(|s| s.id).collect())
        .unwrap_or_default()
}

const WITH_WB: Permissions = Permissions(DEFAULT_PERMISSIONS.0 | Permissions::WHITEBOARD.0);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn strokes_need_the_permission_and_land_on_the_streamed_monitor() {
    let mut h = connect(DEFAULT_PERMISSIONS).await;
    assert_eq!(
        h.host.streamed_monitor(h.host_session),
        Some(monitor_rect())
    );
    // Without WHITEBOARD the host drops the stroke: no overlay is even created.
    assert!(h
        .session
        .send_message(stroke(1, vec![(0, 0), (65535, 65535)])));
    h.barrier().await;
    assert!(h.host.set_permissions(h.host_session, WITH_WB));
    viewer_event(
        &mut h.viewer_events,
        |e| matches!(e, ViewerEvent::PermissionsChanged(p) if p.contains(Permissions::WHITEBOARD)),
    )
    .await;
    // A horizontal line through the middle, from 25 % to 75 % of the width.
    assert!(h.session.send_message(stroke(2, vec![(16384, 32768)])));
    assert!(h.session.send_message(stroke(2, vec![(49151, 32768)])));
    until("stroke 2 on the host board", || {
        board_ids(&h.log) == vec![2]
            && lock_board(h.log.lock().unwrap().board.as_ref().unwrap())
                .get(2)
                .is_some_and(|s| s.points.len() == 2)
    })
    .await;
    host_event(&mut h.events, |e| {
        is_whiteboard_event(e, WhiteboardHostEvent::Drawing)
    })
    .await;
    until("overlay rendered", || h.log.lock().unwrap().inked > 0).await;
    let l = h.log.lock().unwrap();
    assert_eq!(l.created, 1);
    assert_eq!(l.targets, vec![monitor_rect()]);
    // 160 px long, 9.6 px wide (400/10000 of 240 px), centered on the monitor.
    assert!((1500..=2300).contains(&l.inked), "{}", l.inked);
    let (cx, cy) = l.centroid;
    assert!(
        (cx - 160.0).abs() < 1.5 && (cy - 120.0).abs() < 1.5,
        "{cx},{cy}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_clear_badge_wipes_the_screen_and_tells_the_viewer() {
    let mut h = connect(WITH_WB).await;
    assert!(h
        .session
        .send_message(stroke(1, vec![(100, 100), (30000, 30000)])));
    until("overlay created", || {
        h.log.lock().unwrap().on_clear.is_some()
    })
    .await;
    until("stroke on the board", || board_ids(&h.log) == vec![1]).await;
    let clear = h.log.lock().unwrap().on_clear.clone().unwrap();
    // The host user clicks the badge (called on the overlay's thread in real life).
    std::thread::spawn(move || clear()).join().unwrap();
    viewer_event(&mut h.viewer_events, |e| {
        matches!(e, ViewerEvent::Feature(f) if f.feature == FEATURE_NAME
            && f.payload::<WhiteboardEvent>() == Some(&WhiteboardEvent::Remote(WhiteboardOp::Clear)))
    })
    .await;
    assert!(board_ids(&h.log).is_empty());
    host_event(&mut h.events, |e| {
        is_whiteboard_event(e, WhiteboardHostEvent::Cleared { by_host: true })
    })
    .await;
    // A viewer-side clear works too.
    assert!(h.session.send_message(stroke(2, vec![(5, 5)])));
    until("stroke 2", || board_ids(&h.log) == vec![2]).await;
    assert!(h.session.send_message(Message::WhiteboardClear));
    host_event(&mut h.events, |e| {
        is_whiteboard_event(e, WhiteboardHostEvent::Cleared { by_host: false })
    })
    .await;
    assert!(board_ids(&h.log).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoking_removes_the_overlay_and_session_end_destroys_it() {
    let mut h = connect(WITH_WB).await;
    assert!(h
        .session
        .send_message(stroke(1, vec![(100, 100), (30000, 30000)])));
    until("overlay up", || {
        h.log.lock().unwrap().alive == 1 && board_ids(&h.log) == vec![1]
    })
    .await;
    // Live revocation: drawings disappear and the window goes away at once.
    assert!(h.host.set_permissions(h.host_session, DEFAULT_PERMISSIONS));
    until("overlay removed", || h.log.lock().unwrap().alive == 0).await;
    assert!(board_ids(&h.log).is_empty());
    viewer_event(
        &mut h.viewer_events,
        |e| matches!(e, ViewerEvent::PermissionsChanged(p) if !p.contains(Permissions::WHITEBOARD)),
    )
    .await;
    // Strokes sent while revoked are dropped; after a re-grant drawing works again.
    assert!(h.session.send_message(stroke(2, vec![(1, 1)])));
    h.barrier().await;
    assert!(h.host.set_permissions(h.host_session, WITH_WB));
    viewer_event(
        &mut h.viewer_events,
        |e| matches!(e, ViewerEvent::PermissionsChanged(p) if p.contains(Permissions::WHITEBOARD)),
    )
    .await;
    assert!(h.session.send_message(stroke(3, vec![(2, 2)])));
    until("new overlay with stroke 3", || {
        let l = h.log.lock().unwrap();
        l.alive == 1 && l.created == 2
    })
    .await;
    until("only stroke 3", || board_ids(&h.log) == vec![3]).await;
    // The session ends: the overlay is destroyed.
    h.session.disconnect();
    until("overlay destroyed", || h.log.lock().unwrap().alive == 0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hostile_strokes_are_capped() {
    let h = connect(WITH_WB).await;
    let huge = Message::WhiteboardStroke {
        id: 7,
        points: vec![(65535, 0); 100_000],
        color_rgba: 0,
        width: u16::MAX,
    };
    assert!(h.session.send_message(huge));
    until("huge stroke", || board_ids(&h.log) == vec![7]).await;
    {
        let l = h.log.lock().unwrap();
        let board = lock_board(l.board.as_ref().unwrap());
        let s = board.get(7).unwrap();
        assert_eq!(s.points.len(), MAX_WIRE_POINTS);
        assert_eq!(s.width, MAX_WIDTH);
    }
    // A flood of strokes keeps only the newest MAX_STROKES.
    for id in 100..(100 + MAX_STROKES as u64 + 50) {
        assert!(h.session.send_message(stroke(id, vec![(3, 3)])));
    }
    let last = 100 + MAX_STROKES as u64 + 49;
    until("last stroke", || board_ids(&h.log).last() == Some(&last)).await;
    let ids = board_ids(&h.log);
    assert_eq!(ids.len(), MAX_STROKES);
    assert!(!ids.contains(&7), "the oldest stroke was evicted");
    // The session is still alive and usable.
    assert!(h.session.end_reason().is_none());
}
