//! Chat through the `Backend` trait: two real backends on 127.0.0.1 (real QUIC, a still
//! synthetic screen, no input injection), Arabic both ways, the host user turning chat off and
//! on again.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tlp_desk_app::backend::chat::ChatSendError;
use tlp_desk_app::backend::real::{RealBackend, RealConfig};
use tlp_desk_app::backend::{Backend, BackendEvent, ConnectTarget, EventReceiver, SessionId};
use tlp_desk_capture::{BgraFrame, CaptureError, CursorState, MonitorInfo, ScreenCapturer};
use tlp_desk_host::HostConfig;
use tlp_desk_input::{InputError, InputEvent, InputInjector};
use tlp_desk_viewer::KnownHostsStore;
use tokio::runtime::Runtime;

const WAIT: Duration = Duration::from_secs(20);

fn monitor() -> MonitorInfo {
    MonitorInfo {
        index: 0,
        name: "FAKE".into(),
        width: 320,
        height: 240,
        primary: true,
        x: 0,
        y: 0,
    }
}

struct IdleCapturer(MonitorInfo);

impl ScreenCapturer for IdleCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<BgraFrame>, CaptureError> {
        std::thread::sleep(timeout.min(Duration::from_millis(50)));
        Ok(None)
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
        &self.0
    }
}

struct NoInput;

impl InputInjector for NoInput {
    fn inject(&mut self, _: &InputEvent) -> Result<(), InputError> {
        Ok(())
    }
}

struct Peer {
    backend: Arc<RealBackend>,
    rx: EventReceiver,
    _dir: tempfile::TempDir,
}

impl Peer {
    fn start(rt: &Runtime) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let defaults = RealConfig::default();
        let cfg = RealConfig {
            data_dir: dir.path().to_path_buf(),
            bind: "127.0.0.1:0".parse().unwrap(),
            device_name: Some("جهاز-اختبار".into()),
            known_hosts: KnownHostsStore::File(dir.path().join("known_hosts")),
            host: HostConfig {
                advertise: false,
                capturer_factory: Arc::new(|_| {
                    Ok(Box::new(IdleCapturer(monitor())) as Box<dyn ScreenCapturer>)
                }),
                injector_factory: Arc::new(|_| Ok(Box::new(NoInput) as Box<dyn InputInjector>)),
                monitor_lister: Arc::new(|| Ok(vec![monitor()])),
                local_input_grace: Duration::ZERO,
                remote_pointer_marker: false,
                // The app's default grant (includes CHAT).
                ..defaults.host
            },
            fallback_discovery: false,
        };
        let (backend, rx) = RealBackend::start_concrete(rt.handle(), Arc::new(|| {}), cfg);
        Self {
            backend,
            rx,
            _dir: dir,
        }
    }

    fn wait<T>(
        &mut self,
        rt: &Runtime,
        what: &str,
        mut pick: impl FnMut(&BackendEvent) -> Option<T>,
    ) -> T {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let ev = rt
                .block_on(async { tokio::time::timeout(left, self.rx.recv()).await })
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
                .expect("event channel closed");
            if let Some(v) = pick(&ev) {
                return v;
            }
        }
    }

    fn received(&mut self, rt: &Runtime, session: SessionId) -> String {
        self.wait(rt, "chat message", |e| match e {
            BackendEvent::ChatReceived { session: s, line } if *s == session => {
                assert!(!line.mine);
                Some(line.text.clone())
            }
            _ => None,
        })
    }

    fn availability(&mut self, rt: &Runtime, session: SessionId) -> bool {
        self.wait(rt, "chat availability", |e| match e {
            BackendEvent::ChatAvailability {
                session: s,
                allowed,
            } if *s == session => Some(*allowed),
            _ => None,
        })
    }
}

#[test]
fn chat_through_the_backend_trait() {
    let rt = Runtime::new().unwrap();
    let mut host = Peer::start(&rt);
    let mut viewer = Peer::start(&rt);
    let code = host.backend.local_info().one_time_code;
    let target = ConnectTarget::Address(host.backend.local_addr().unwrap());
    viewer.backend.connect(target, Some(code));
    let session = viewer.wait(&rt, "session", |e| match e {
        BackendEvent::SessionStarted { session, .. } => Some(*session),
        _ => None,
    });
    let host_session = host.wait(&rt, "host session", |e| match e {
        BackendEvent::HostSessionStarted { session, .. } => Some(*session),
        _ => None,
    });
    assert!(host.availability(&rt, host_session));
    assert!(viewer.availability(&rt, session));

    let line = viewer
        .backend
        .send_chat(session, "مرحباً، هل تسمعني؟ (PC-01)")
        .unwrap();
    assert!(line.mine);
    assert_eq!(
        host.received(&rt, host_session),
        "مرحباً، هل تسمعني؟ (PC-01)"
    );
    host.backend
        .send_chat(host_session, "نعم — yes 👍")
        .unwrap();
    assert_eq!(viewer.received(&rt, session), "نعم — yes 👍");

    // The host user turns chat off: both sides learn it and neither can send.
    host.backend.set_chat_allowed(host_session, false);
    assert!(!host.availability(&rt, host_session));
    assert!(!viewer.availability(&rt, session));
    assert_eq!(
        viewer.backend.send_chat(session, "هل ما زلت هنا؟"),
        Err(ChatSendError::NotAllowed)
    );
    assert_eq!(
        host.backend.send_chat(host_session, "لا"),
        Err(ChatSendError::NotAllowed)
    );
    host.backend.set_chat_allowed(host_session, true);
    assert!(viewer.availability(&rt, session));
    viewer.backend.send_chat(session, "شكراً").unwrap();
    assert_eq!(host.received(&rt, host_session), "شكراً");

    // Unknown sessions have no chat.
    assert_eq!(
        viewer.backend.send_chat(SessionId(9999), "x"),
        Err(ChatSendError::Unavailable)
    );
    viewer.backend.disconnect(session);
}
