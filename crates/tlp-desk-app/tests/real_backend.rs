//! Smoke tests of the real backend through the `Backend` trait: two backends in one process
//! on 127.0.0.1 (ephemeral ports), real QUIC + TLS + SPAKE2 + H.264, with a synthetic screen
//! and a recording input injector instead of the real desktop.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tlp_desk_app::backend::audio::AudioDevices;
use tlp_desk_app::backend::real::{RealBackend, RealConfig};
use tlp_desk_app::backend::{
    Backend, BackendEvent, ConnectError, ConnectTarget, DeviceId, EndReason, EventReceiver,
    FrameSlot, HostProblem, SessionId,
};
use tlp_desk_capture::{BgraFrame, CaptureError, CursorState, MonitorInfo, ScreenCapturer};
use tlp_desk_files::TransferConfig;
use tlp_desk_host::{HostConfig, HostError, SystemPower, DEFAULT_PERMISSIONS};
use tlp_desk_input::{InputError, InputInjector, MonitorRect};
use tlp_desk_proto::{InputEvent, Permissions, RejectReason};
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

/// Refuses every restart/lock: these tests never touch the machine.
struct NoPower;

impl SystemPower for NoPower {
    fn restart(&self) -> Result<(), HostError> {
        Err(HostError::System("not in tests".into()))
    }

    fn lock_workstation(&self) -> Result<(), HostError> {
        Err(HostError::System("not in tests".into()))
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
            capturer_factory: Arc::new(|_, _| {
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
            default_permissions: DEFAULT_PERMISSIONS.union(Permissions::FILES),
            ..HostConfig::default()
        },
        fallback_discovery: false,
        power: Arc::new(NoPower),
        system_permissions: false,
        // Never record or play real sound in tests.
        audio: AudioDevices::fake(),
        clipboard: None,
        transfer: TransferConfig {
            default_upload_dir: Some(dir.join("received")),
            ..TransferConfig::default()
        },
        ..RealConfig::default()
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

/// The address book shows the key pinned in known hosts. (Wake-on-LAN is not exercised here:
/// the real backend would broadcast; `address_book::wol` tests it on loopback.)
#[test]
fn address_book_hooks_read_known_hosts_and_the_neighbour_cache() {
    let rt = Runtime::new().unwrap();
    let peer = start(&rt, Arc::default(), 0, |dir| {
        std::fs::write(
            dir.join("known_hosts"),
            format!("id:482913057 {}\n", "3f9a".repeat(16)),
        )
        .unwrap();
    });
    assert_eq!(
        peer.backend.pinned_fingerprint(DeviceId(482_913_057)),
        Some("3f9a 3f9a 3f9a 3f9a".into())
    );
    assert_eq!(peer.backend.pinned_fingerprint(DeviceId(123_456_789)), None);
    // Loopback never has a neighbour-cache entry; the lookup is read-only.
    assert_eq!(
        peer.backend.neighbor_mac("127.0.0.1".parse().unwrap()),
        None
    );
}

/// TLP-recording: the viewer records the live H.264 stream to an MP4 that plays back.
#[test]
fn viewer_records_a_session_to_mp4() {
    use tlp_desk_viewer::recording::{list_recordings, Player};

    let rt = Runtime::new().unwrap();
    let host = start(&rt, Arc::default(), 0, |_| {});
    let mut viewer = start(&rt, Arc::default(), 0, |_| {});
    let folder = tempfile::tempdir().unwrap();
    viewer
        .backend
        .set_recordings_folder(folder.path().to_owned());
    let code = host.backend.local_info().one_time_code;
    let attempt = viewer.backend.connect(
        ConnectTarget::Address(host.backend.local_addr().unwrap()),
        Some(code),
    );
    let (session, frames) = viewer.events.wait(&rt, "session", |e| match e {
        BackendEvent::SessionStarted {
            attempt: a,
            session,
            frames,
            ..
        } if *a == attempt => Some((*session, frames.clone())),
        _ => None,
    });
    wait_frame(&frames);

    viewer.backend.start_recording(session);
    let started_in = viewer.events.wait(&rt, "recording started", |e| match e {
        BackendEvent::RecordingStarted { session: s, folder } if *s == session => {
            Some(folder.clone())
        }
        _ => None,
    });
    assert_eq!(started_in, folder.path());
    // About a second of video (the fake screen runs at ~30 fps).
    for _ in 0..25 {
        wait_frame(&frames);
    }
    viewer.backend.stop_recording(session);
    let (files, problem) = viewer.events.wait(&rt, "recording stopped", |e| match e {
        BackendEvent::RecordingStopped {
            session: s,
            files,
            problem,
        } if *s == session => Some((files.clone(), problem.clone())),
        _ => None,
    });
    assert_eq!(problem, None);
    assert_eq!(files.len(), 1, "{files:?}");
    let name = files[0].file_name().unwrap().to_str().unwrap().to_owned();
    assert!(name.starts_with("TLP-Desk اختبار-pc "), "{name}");

    let list = list_recordings(folder.path()).unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].complete);
    assert!(list[0]
        .duration
        .is_some_and(|d| d >= Duration::from_millis(300)));

    let player = Player::open(&files[0], tlp_desk_viewer::PixelFormat::Rgba, None).unwrap();
    let deadline = Instant::now() + WAIT;
    let frame = loop {
        if let Some(f) = player.take_frame() {
            break f;
        }
        assert!(Instant::now() < deadline, "the recording does not decode");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!((frame.width, frame.height), (W, H));
    let _ = &host;
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

// --- TLP-settings-tray: connection log + live host settings ---

/// A peer whose connection log goes to its own temp dir.
fn start_logged(rt: &Runtime) -> (Peer, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let log_dir = dir.path().join("log");
    let mut cfg = config(dir.path(), Arc::default(), 0);
    cfg.connection_log = Some(log_dir.clone());
    let (backend, rx) = RealBackend::start_concrete(rt.handle(), Arc::new(|| {}), cfg);
    let peer = Peer {
        backend,
        events: Events::new(rx),
        _dir: dir,
    };
    (peer, log_dir)
}

#[test]
fn connection_log_records_sessions_refusals_and_pause() {
    use tlp_desk_app::connection_log::{Direction, LogAuth, LogEnd, LogStore};
    use tlp_desk_app::settings::HostSettings;

    let rt = Runtime::new().unwrap();
    let (mut host, host_log) = start_logged(&rt);
    let (mut viewer, viewer_log) = start_logged(&rt);
    let target = ConnectTarget::Address(host.backend.local_addr().unwrap());

    // A session by one-time code, ended by the viewer.
    let code = host.backend.local_info().one_time_code;
    let attempt = viewer.backend.connect(target.clone(), Some(code));
    let session = viewer.events.wait(&rt, "code session", |e| match e {
        BackendEvent::SessionStarted {
            attempt: a,
            session,
            ..
        } if *a == attempt => Some(*session),
        _ => None,
    });
    host.events.wait(&rt, "host session", |e| {
        matches!(e, BackendEvent::HostSessionStarted { .. }).then_some(())
    });
    viewer.backend.disconnect(session);
    viewer.events.wait(&rt, "viewer end", |e| {
        matches!(e, BackendEvent::SessionEnded { session: s, .. } if *s == session).then_some(())
    });
    host.events.wait(&rt, "host end", |e| {
        matches!(e, BackendEvent::SessionEnded { .. }).then_some(())
    });

    // A wrong code is logged as a refusal on the host.
    let current = host.backend.local_info().one_time_code;
    let first = (current.as_bytes()[0] - b'0' + 1) % 10;
    let attempt = viewer
        .backend
        .connect(target.clone(), Some(format!("{first}{}", &current[1..])));
    viewer.events.wait(&rt, "wrong code", |e| {
        matches!(e, BackendEvent::ConnectFailed { attempt: a, .. } if *a == attempt).then_some(())
    });

    // Pausing incoming connections refuses everyone before the handshake...
    let paused = HostSettings {
        incoming_enabled: false,
        ..HostSettings::default()
    };
    host.backend.apply_host_settings(&paused);
    let attempt = viewer.backend.connect(target.clone(), None);
    viewer.events.wait(&rt, "refused while paused", |e| {
        matches!(e, BackendEvent::ConnectFailed { attempt: a, .. } if *a == attempt).then_some(())
    });
    assert!(
        !host
            .events
            .seen
            .iter()
            .any(|e| matches!(e, BackendEvent::IncomingRequest { .. })),
        "a paused host never prompts"
    );
    // ...and resuming lets them in again.
    host.backend.apply_host_settings(&HostSettings::default());
    let attempt = viewer.backend.connect(target, None);
    let request = host.events.wait(&rt, "prompt after resume", |e| match e {
        BackendEvent::IncomingRequest { request, .. } => Some(*request),
        _ => None,
    });
    host.backend.respond_to_request(request, false);
    viewer.events.wait(&rt, "declined", |e| {
        matches!(e, BackendEvent::ConnectFailed { attempt: a, .. } if *a == attempt).then_some(())
    });

    // The host writes its lines from its event pump, which may lag the UI events a little
    // (the banner reconciler can report a session end first): poll with a deadline.
    let deadline = Instant::now() + WAIT;
    let host_entries = loop {
        let entries = LogStore::new(&host_log).read_all();
        if entries.len() >= 3 || Instant::now() > deadline {
            break entries;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut ends: Vec<LogEnd> = host_entries.iter().map(|e| e.end).collect();
    ends.sort_by_key(|e| format!("{e:?}"));
    assert_eq!(
        ends,
        [
            LogEnd::ClosedRemotely,
            LogEnd::Declined,
            LogEnd::WrongCredentials
        ],
        "{host_entries:?}"
    );
    let first = host_entries
        .iter()
        .find(|e| e.end == LogEnd::ClosedRemotely)
        .unwrap();
    assert_eq!(first.direction, Direction::Incoming);
    assert_eq!(first.auth, LogAuth::OneTimeCode);
    assert_eq!(first.name, "اختبار-pc");
    assert_eq!(first.address, "127.0.0.1");
    assert_eq!(first.fingerprint.len(), 64);
    assert_eq!(
        first.device_id,
        viewer.backend.local_info().device_id.0,
        "the log names the viewer"
    );

    let viewer_entries = LogStore::new(&viewer_log).read_all();
    assert_eq!(viewer_entries.len(), 1, "{viewer_entries:?}");
    assert_eq!(viewer_entries[0].direction, Direction::Outgoing);
    assert_eq!(viewer_entries[0].end, LogEnd::ClosedHere);
    assert_eq!(viewer_entries[0].auth, LogAuth::OneTimeCode);
    assert!(viewer_entries[0]
        .permissions
        .0
        .contains(tlp_desk_proto::Permissions::VIEW));
}

/// Clipboard sync through the real backends, with in-memory clipboards on both sides (never the
/// user's real clipboard): Arabic text copied on the viewer lands byte-exact on the host, both
/// UIs get their events, and the viewer's "Sync clipboard" switch stops it.
#[test]
fn clipboard_syncs_through_the_backends() {
    use tlp_desk_files::{ClipContent, ClipboardEvent, Direction, MemoryClipboard};
    let rt = Runtime::new().unwrap();
    let peer = |clip: Arc<MemoryClipboard>| {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path(), Arc::default(), 0);
        cfg.clipboard = Some(clip);
        cfg.host.default_permissions =
            tlp_desk_host::DEFAULT_PERMISSIONS.union(tlp_desk_proto::Permissions::CLIPBOARD);
        let (backend, rx) = RealBackend::start_concrete(rt.handle(), Arc::new(|| {}), cfg);
        Peer {
            backend,
            events: Events::new(rx),
            _dir: dir,
        }
    };
    let host_clip = Arc::new(MemoryClipboard::new());
    let viewer_clip = Arc::new(MemoryClipboard::new());
    let mut host = peer(host_clip.clone());
    let mut viewer = peer(viewer_clip.clone());
    let code = host.backend.local_info().one_time_code;
    let target = ConnectTarget::Address(host.backend.local_addr().unwrap());
    viewer.backend.connect(target, Some(code));
    let (session, _frames) = viewer.events.wait(&rt, "session started", started);
    let host_session = host.events.wait(&rt, "host session", |e| match e {
        BackendEvent::HostSessionStarted { session, .. } => Some(*session),
        _ => None,
    });

    let text = "نسخ\u{200F} من العارض\r\nلا\u{200C}مي 👋";
    viewer_clip.copy(ClipContent::Text(text.into()));
    let ev = viewer.events.wait(&rt, "clipboard sent", |e| match e {
        BackendEvent::Clipboard { session: s, event } if *s == session => Some(event.clone()),
        _ => None,
    });
    assert!(matches!(
        ev,
        ClipboardEvent::Synced {
            direction: Direction::Sent,
            ..
        }
    ));
    let ev = host.events.wait(&rt, "clipboard received", |e| match e {
        BackendEvent::Clipboard { session: s, event } if *s == host_session => Some(event.clone()),
        _ => None,
    });
    assert!(matches!(
        ev,
        ClipboardEvent::Synced {
            direction: Direction::Received,
            ..
        }
    ));
    assert_eq!(host_clip.content(), Some(ClipContent::Text(text.into())));

    // "Sync clipboard" off on the viewer: nothing moves any more.
    viewer.backend.set_clipboard_sync(session, false);
    std::thread::sleep(Duration::from_millis(200));
    viewer_clip.copy(ClipContent::Text("لا يُرسل".into()));
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(host_clip.content(), Some(ClipContent::Text(text.into())));
    assert_eq!(host_clip.write_count(), 1);
}

fn find_file(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|n| n == name) {
            return Some(path);
        }
    }
    None
}

#[test]
fn files_are_sent_through_the_backend_with_progress() {
    use tlp_desk_app::backend::files::TransferStatus;

    let rt = Runtime::new().unwrap();
    let host = start(&rt, Arc::default(), 0, |d| {
        std::fs::create_dir_all(d.join("received")).unwrap();
    });
    let mut viewer = start(&rt, Arc::default(), 0, |_| {});
    let code = host.backend.local_info().one_time_code;
    let attempt = viewer.backend.connect(
        ConnectTarget::Address(host.backend.local_addr().unwrap()),
        Some(code),
    );
    let (session, _frames) = viewer.events.wait(&rt, "session", |e| match e {
        BackendEvent::SessionStarted {
            attempt: a,
            session,
            frames,
            ..
        } if *a == attempt => Some((*session, frames.clone())),
        _ => None,
    });

    // A nested folder with Arabic names plus a larger file.
    let src = tempfile::tempdir().unwrap();
    let folder = src.path().join("مجلد المشروع");
    std::fs::create_dir_all(folder.join("فرعي")).unwrap();
    std::fs::write(folder.join("فرعي").join("ملف.txt"), "مرحبا بالعالم").unwrap();
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(folder.join("big.bin"), &big).unwrap();

    viewer.backend.send_files(session, vec![folder]);
    let view = viewer.events.wait(&rt, "transfer completed", |e| match e {
        BackendEvent::Transfer { session: s, view }
            if *s == session && view.status == TransferStatus::Completed =>
        {
            Some(view.clone())
        }
        _ => None,
    });
    assert_eq!(view.total_files, 2);
    assert_eq!(view.failed_files, 0);
    assert_eq!(view.bytes_done, view.total_bytes);

    let received = host._dir.path().join("received");
    let text = find_file(&received, "ملف.txt").expect("Arabic file arrived");
    assert_eq!(std::fs::read_to_string(text).unwrap(), "مرحبا بالعالم");
    let bin = find_file(&received, "big.bin").expect("big file arrived");
    assert_eq!(std::fs::read(bin).unwrap(), big);

    // A missing path is reported instead of silently ignored.
    viewer
        .backend
        .send_files(session, vec![src.path().join("does-not-exist")]);
    viewer.events.wait(&rt, "not started", |e| match e {
        BackendEvent::TransferNotStarted { session: s, .. } if *s == session => Some(()),
        _ => None,
    });
    viewer.backend.disconnect(session);
}

// --- audio (TLP-audio) ---

fn audio_state(
    session: SessionId,
    want: impl Fn(&tlp_desk_app::backend::audio::AudioState) -> bool,
) -> impl FnMut(&BackendEvent) -> Option<()> {
    move |e| match e {
        BackendEvent::AudioState { session: s, state } if *s == session && want(state) => Some(()),
        _ => None,
    }
}

/// Fake sound devices: the host "plays" a tone, the viewer's speaker records it.
#[test]
fn remote_sound_follows_the_permission_and_the_toolbar_toggle() {
    use tlp_desk_audio::device::fake::{FakeCapture, FakePlayback, FakeSignal};
    use tlp_desk_proto::Permissions;

    let rt = Runtime::new().unwrap();
    let capture = FakeCapture::new(
        48_000,
        2,
        FakeSignal::Tone {
            freq: 440.0,
            amplitude: 0.5,
        },
    );
    let speaker = FakePlayback::new(48_000, 2);
    let start_with = |grant_audio: bool| {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path(), Arc::default(), 0);
        if grant_audio {
            cfg.host.default_permissions = tlp_desk_host::DEFAULT_PERMISSIONS | Permissions::AUDIO;
        }
        cfg.audio = AudioDevices {
            capture: Arc::new(capture.clone()),
            playback: Arc::new(speaker.clone()),
        };
        let (backend, rx) = RealBackend::start_concrete(rt.handle(), Arc::new(|| {}), cfg);
        Peer {
            backend,
            events: Events::new(rx),
            _dir: dir,
        }
    };

    // Default host permissions do not include AUDIO: the toolbar shows it as not allowed and
    // nothing is captured.
    let strict = start_with(false);
    let mut viewer = start_with(false);
    let code = strict.backend.local_info().one_time_code;
    viewer.backend.connect(
        ConnectTarget::Address(strict.backend.local_addr().unwrap()),
        Some(code),
    );
    let (session, _) = viewer.events.wait(&rt, "session", started);
    viewer.events.wait(
        &rt,
        "audio not permitted",
        audio_state(session, |s| !s.permitted && s.enabled && !s.playing),
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(capture.starts(), 0);
    viewer.backend.disconnect(session);

    // A host that grants AUDIO: sound plays, the toggle stops and restarts it.
    let open = start_with(true);
    let code = open.backend.local_info().one_time_code;
    viewer.backend.connect(
        ConnectTarget::Address(open.backend.local_addr().unwrap()),
        Some(code),
    );
    let (session, _) = viewer.events.wait(&rt, "second session", started);
    viewer.events.wait(
        &rt,
        "audio playing",
        audio_state(session, |s| s.permitted && s.playing),
    );
    let deadline = Instant::now() + WAIT;
    while speaker.recorded().iter().filter(|s| s.abs() > 0.1).count() < 9_600 {
        assert!(Instant::now() < deadline, "no sound reached the speaker");
        std::thread::sleep(Duration::from_millis(20));
    }
    viewer.backend.set_audio_volume(session, 0.5);
    viewer.events.wait(
        &rt,
        "volume",
        audio_state(session, |s| (s.volume - 0.5).abs() < 1e-6),
    );
    viewer.backend.set_audio_enabled(session, false);
    viewer.events.wait(
        &rt,
        "audio off",
        audio_state(session, |s| !s.enabled && !s.playing),
    );
    let deadline = Instant::now() + WAIT;
    while capture.is_running() {
        assert!(Instant::now() < deadline, "host kept capturing");
        std::thread::sleep(Duration::from_millis(20));
    }
    viewer.backend.set_audio_enabled(session, true);
    viewer.events.wait(
        &rt,
        "audio back on",
        audio_state(session, |s| s.enabled && s.playing),
    );
    assert!(capture.is_running());
}
