//! Smoke tests of the real backend through the `Backend` trait: two backends in one process
//! on 127.0.0.1 (ephemeral ports), real QUIC + TLS + SPAKE2 + H.264, with a synthetic screen
//! and a recording input injector instead of the real desktop.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tlp_desk_app::backend::real::{RealBackend, RealConfig};
use tlp_desk_app::backend::{
    Backend, BackendEvent, ConnectError, ConnectTarget, EndReason, EventReceiver, FrameSlot,
    HostProblem, SessionId,
};
use tlp_desk_capture::{BgraFrame, CaptureError, CursorState, MonitorInfo, ScreenCapturer};
use tlp_desk_host::HostConfig;
use tlp_desk_input::{InputError, InputInjector, MonitorRect};
use tlp_desk_proto::{InputEvent, RejectReason};
use tlp_desk_viewer::KnownHostsStore;
use tokio::runtime::Runtime;

const W: u32 = 320;
const H: u32 = 240;
const WAIT: Duration = Duration::from_secs(20);

struct FakeCapturer {
    mon: MonitorInfo,
    n: u32,
}

impl ScreenCapturer for FakeCapturer {
    fn next_frame(&mut self, _timeout: Duration) -> Result<Option<BgraFrame>, CaptureError> {
        std::thread::sleep(Duration::from_millis(30));
        self.n += 1;
        let mut f = BgraFrame::new_tight(W, H);
        let bar = (self.n * 8) % W;
        for y in 0..H {
            let row = f.row_mut(y);
            for x in 0..W {
                let i = x as usize * 4;
                let v = if x >= bar && x < bar + 32 { 230 } else { 40 };
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

struct FakeInjector(Recorded);

impl InputInjector for FakeInjector {
    fn inject(&mut self, event: &InputEvent) -> Result<(), InputError> {
        self.0.lock().unwrap().push(event.clone());
        Ok(())
    }
}

fn config(dir: &std::path::Path, recorded: Recorded, port: u16) -> RealConfig {
    RealConfig {
        data_dir: dir.to_path_buf(),
        bind: format!("127.0.0.1:{port}").parse().unwrap(),
        device_name: Some("اختبار-pc".into()),
        known_hosts: KnownHostsStore::File(dir.join("known_hosts")),
        host: HostConfig {
            advertise: false,
            capturer_factory: Arc::new(|_| {
                Ok(Box::new(FakeCapturer {
                    mon: MonitorInfo {
                        index: 0,
                        name: "FAKE".into(),
                        width: W,
                        height: H,
                        primary: true,
                        x: 0,
                        y: 0,
                    },
                    n: 0,
                }) as Box<dyn ScreenCapturer>)
            }),
            injector_factory: Arc::new(move |_r: MonitorRect| {
                Ok(Box::new(FakeInjector(recorded.clone())) as Box<dyn InputInjector>)
            }),
            monitor_lister: Arc::new(|| {
                Ok(vec![MonitorInfo {
                    index: 0,
                    name: "FAKE".into(),
                    width: W,
                    height: H,
                    primary: true,
                    x: 0,
                    y: 0,
                }])
            }),
            // No real low-level hooks or on-screen marker in tests.
            local_input_grace: Duration::ZERO,
            remote_pointer_marker: false,
            ..HostConfig::default()
        },
        fallback_discovery: false,
    }
}

/// Backend events with a buffer, so waiting for one event never loses another.
struct Events {
    rx: EventReceiver,
    seen: Vec<BackendEvent>,
}

impl Events {
    fn new(rx: EventReceiver) -> Self {
        Self {
            rx,
            seen: Vec::new(),
        }
    }

    fn wait<T>(
        &mut self,
        rt: &Runtime,
        what: &str,
        mut pick: impl FnMut(&BackendEvent) -> Option<T>,
    ) -> T {
        if let Some(i) = self.seen.iter().position(|e| pick(e).is_some()) {
            let ev = self.seen.remove(i);
            return pick(&ev).unwrap();
        }
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let ev = rt
                .block_on(async { tokio::time::timeout(left, self.rx.recv()).await })
                .unwrap_or_else(|_| panic!("timed out waiting for {what}; seen {:?}", self.seen))
                .expect("event channel closed");
            if let Some(v) = pick(&ev) {
                return v;
            }
            self.seen.push(ev);
        }
    }
}

struct Peer {
    backend: Arc<RealBackend>,
    events: Events,
    _dir: tempfile::TempDir,
}

fn start(
    rt: &Runtime,
    recorded: Recorded,
    port: u16,
    prepare: impl FnOnce(&std::path::Path),
) -> Peer {
    let dir = tempfile::tempdir().unwrap();
    prepare(dir.path());
    let (backend, rx) = RealBackend::start_concrete(
        rt.handle(),
        Arc::new(|| {}),
        config(dir.path(), recorded, port),
    );
    Peer {
        backend,
        events: Events::new(rx),
        _dir: dir,
    }
}

fn wait_frame(frames: &FrameSlot) {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(f) = frames.take() {
            assert_eq!((f.width, f.height), (W, H));
            assert_eq!(f.rgba.len(), (W * H * 4) as usize);
            assert!(
                f.rgba.chunks_exact(4).all(|p| p[3] == 255),
                "frames are opaque RGBA"
            );
            frames.recycle(f.rgba);
            return;
        }
        assert!(Instant::now() < deadline, "no video frame arrived");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn started(ev: &BackendEvent) -> Option<(SessionId, Arc<FrameSlot>)> {
    match ev {
        BackendEvent::SessionStarted {
            session, frames, ..
        } => Some((*session, frames.clone())),
        _ => None,
    }
}

#[test]
fn host_and_viewer_through_the_backend_trait() {
    let rt = Runtime::new().unwrap();
    let recorded: Recorded = Arc::default();
    let mut host = start(&rt, recorded.clone(), 0, |_| {});
    let mut viewer = start(&rt, Arc::default(), 0, |_| {});
    let addr = host.backend.local_addr().unwrap();
    let target = ConnectTarget::Address(addr);

    let info = host.backend.local_info();
    assert_eq!(info.device_name, "اختبار-pc");
    assert_eq!(info.one_time_code.len(), 6);
    assert_ne!(info.device_id, viewer.backend.local_info().device_id);

    // 1. Accept prompt: AwaitingAccept, then the host user accepts.
    let attempt = viewer.backend.connect(target.clone(), None);
    let (request, peer) = host.events.wait(&rt, "incoming request", |e| match e {
        BackendEvent::IncomingRequest { request, peer } => Some((*request, peer.clone())),
        _ => None,
    });
    assert_eq!(peer.device_id, viewer.backend.local_info().device_id);
    viewer.events.wait(&rt, "awaiting accept", |e| {
        matches!(e, BackendEvent::AwaitingAccept { attempt: a } if *a == attempt).then_some(())
    });
    host.backend.respond_to_request(request, true);
    let (session, frames) = viewer.events.wait(&rt, "session started", started);
    let host_session = host.events.wait(&rt, "host session", |e| match e {
        BackendEvent::HostSessionStarted { session, .. } => Some(*session),
        _ => None,
    });
    wait_frame(&frames);
    wait_frame(&frames); // a second frame proves the recycled buffer path works
    let (count, active) = viewer.events.wait(&rt, "monitor list", |e| match e {
        BackendEvent::MonitorsChanged {
            session: s,
            monitors,
            active,
        } if *s == session => Some((monitors.len(), *active)),
        _ => None,
    });
    assert_eq!((count, active), (1, 0));
    // An unknown monitor index is ignored by the host (the session keeps streaming).
    viewer.backend.select_monitor(session, 5);
    wait_frame(&frames);

    viewer
        .backend
        .send_input(session, InputEvent::MouseMove { x: 100, y: 200 });
    let deadline = Instant::now() + WAIT;
    while !recorded
        .lock()
        .unwrap()
        .contains(&InputEvent::MouseMove { x: 100, y: 200 })
    {
        assert!(Instant::now() < deadline, "input never reached the host");
        std::thread::sleep(Duration::from_millis(20));
    }
    viewer.events.wait(&rt, "stats", |e| match e {
        BackendEvent::Stats { session: s, stats } if *s == session && stats.fps > 0.0 => Some(()),
        _ => None,
    });
    // Device info: the host answers from this machine (read-only system query).
    viewer.backend.request_device_info(session);
    let info = viewer.events.wait(&rt, "device info", |e| match e {
        BackendEvent::DeviceInfo { session: s, info } if *s == session => Some(info.clone()),
        _ => None,
    });
    assert!(info.cores >= 1 && info.ram_bytes > 0, "{info:?}");

    // The host user ends the session from the banner.
    host.backend.disconnect(host_session);
    let reason = host.events.wait(&rt, "host session ended", |e| match e {
        BackendEvent::SessionEnded { session, reason } if *session == host_session => {
            Some(reason.clone())
        }
        _ => None,
    });
    assert_eq!(reason, EndReason::Local);
    let reason = viewer
        .events
        .wait(&rt, "viewer session ended", |e| match e {
            BackendEvent::SessionEnded { session: s, reason } if *s == session => {
                Some(reason.clone())
            }
            _ => None,
        });
    assert_eq!(reason, EndReason::Remote);

    // 2. One-time code: no prompt, and the code rotates afterwards.
    let code = host.backend.local_info().one_time_code;
    let attempt = viewer.backend.connect(target.clone(), Some(code.clone()));
    let (session, frames) = viewer.events.wait(&rt, "code session", |e| match e {
        BackendEvent::SessionStarted {
            attempt: a,
            session,
            frames,
            ..
        } if *a == attempt => Some((*session, frames.clone())),
        _ => None,
    });
    wait_frame(&frames);
    host.events.wait(&rt, "rotated code", |e| match e {
        BackendEvent::LocalInfo(i) if i.one_time_code != code => Some(()),
        _ => None,
    });
    assert!(
        !host
            .events
            .seen
            .iter()
            .any(|e| matches!(e, BackendEvent::IncomingRequest { .. })),
        "code auth must not prompt"
    );
    viewer.backend.disconnect(session);
    let reason = viewer.events.wait(&rt, "local end", |e| match e {
        BackendEvent::SessionEnded { session: s, reason } if *s == session => Some(reason.clone()),
        _ => None,
    });
    assert_eq!(reason, EndReason::Local);
    host.events.wait(&rt, "host sees viewer leave", |e| {
        matches!(e, BackendEvent::SessionEnded { .. }).then_some(())
    });

    // 3. Wrong code (the current one with its first digit changed).
    let current = host.backend.local_info().one_time_code;
    let first = (current.as_bytes()[0] - b'0' + 1) % 10;
    let wrong = format!("{first}{}", &current[1..]);
    let attempt = viewer.backend.connect(target.clone(), Some(wrong));
    let err = viewer.events.wait(&rt, "wrong code", |e| match e {
        BackendEvent::ConnectFailed { attempt: a, error } if *a == attempt => Some(error.clone()),
        _ => None,
    });
    assert_eq!(err, ConnectError::Rejected(RejectReason::WrongCredentials));
}

#[test]
fn changed_host_key_is_loud_and_can_be_forgotten() {
    let rt = Runtime::new().unwrap();
    let mut host = start(&rt, Arc::default(), 0, |_| {});
    let addr = host.backend.local_addr().unwrap();
    // The viewer "remembers" a different key for this address.
    let mut viewer = start(&rt, Arc::default(), 0, |dir| {
        std::fs::write(
            dir.join("known_hosts"),
            format!("addr:{addr} {}\n", "00".repeat(32)),
        )
        .unwrap();
    });
    let target = ConnectTarget::Address(addr);
    let code = host.backend.local_info().one_time_code;
    let attempt = viewer.backend.connect(target.clone(), Some(code));
    let err = viewer.events.wait(&rt, "key changed", |e| match e {
        BackendEvent::ConnectFailed { attempt: a, error } if *a == attempt => Some(error.clone()),
        _ => None,
    });
    match err {
        ConnectError::HostKeyChanged {
            previous,
            presented,
        } => {
            assert_eq!(previous, "0000 0000 0000 0000");
            assert_ne!(presented, previous);
        }
        other => panic!("expected HostKeyChanged, got {other:?}"),
    }
    assert!(
        !host
            .events
            .seen
            .iter()
            .any(|e| matches!(e, BackendEvent::IncomingRequest { .. })),
        "nothing may reach the host user"
    );
    assert!(viewer.backend.forget_host_key(attempt));
    assert!(!viewer.backend.forget_host_key(attempt), "only once");

    let code = host.backend.local_info().one_time_code;
    let attempt = viewer.backend.connect(target, Some(code));
    viewer
        .events
        .wait(&rt, "session after forgetting", |e| match e {
            BackendEvent::SessionStarted { attempt: a, .. } if *a == attempt => Some(()),
            _ => None,
        });
    let _ = &mut host;
}

#[test]
fn busy_port_degrades_to_dial_only_with_a_clear_problem() {
    let rt = Runtime::new().unwrap();
    let host = start(&rt, Arc::default(), 0, |_| {});
    let port = host.backend.local_addr().unwrap().port();
    let mut second = start(&rt, Arc::default(), port, |_| {});
    let problem = second.events.wait(&rt, "host unavailable", |e| match e {
        BackendEvent::HostUnavailable(p) => Some(p.clone()),
        _ => None,
    });
    assert_eq!(problem, HostProblem::PortInUse(port));
    let local = second.backend.local_addr().expect("dial-only endpoint");
    assert_ne!(local.port(), port);
    assert!(second.backend.local_info().one_time_code.is_empty());

    // It can still connect out.
    let code = host.backend.local_info().one_time_code;
    let attempt = second.backend.connect(
        ConnectTarget::Address(host.backend.local_addr().unwrap()),
        Some(code),
    );
    second.events.wait(&rt, "outgoing session", |e| match e {
        BackendEvent::SessionStarted { attempt: a, .. } if *a == attempt => Some(()),
        _ => None,
    });
}
