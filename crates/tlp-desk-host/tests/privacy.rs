//! Privacy mode end to end: a real host and viewer over QUIC on 127.0.0.1 with a **fake**
//! shield (nothing is ever blanked or blocked on the machine running the tests).

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tlp_desk_capture::{BgraFrame, CaptureError, CursorState, MonitorInfo, ScreenCapturer};
use tlp_desk_crypto::Identity;
use tlp_desk_host::{
    FeatureSet, Host, HostConfig, HostEvent, HostEvents, PrivacyCause, PrivacyFeature,
    PrivacyStatus, PrivacyTextFn, PrivacyViewer, SessionId, ShieldFactory, DEFAULT_PERMISSIONS,
    PRIVACY_FEATURE,
};
use tlp_desk_input::{
    InputError, InputEvent, InputInjector, LocalActivity, PrivacyScreenText, PrivacyShield,
    ScreenLine, ShieldSignal, ShieldSignalFn,
};
use tlp_desk_net::{Endpoint, EndpointConfig};
use tlp_desk_proto::Permissions;
use tlp_desk_viewer::{
    privacy_request, AuthMethod, ConnectTarget, KnownHostsStore, PixelFormat, PrivacyState,
    PrivacyViewerFeature, Viewer, ViewerConfig, ViewerEvent, ViewerEvents, ViewerSession,
};

const WAIT: Duration = Duration::from_secs(15);

/// A monitor that never produces frames (privacy mode does not need video).
struct IdleCapturer(MonitorInfo);

impl ScreenCapturer for IdleCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<BgraFrame>, CaptureError> {
        std::thread::sleep(timeout.min(Duration::from_millis(20)));
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

struct NoInjector;
impl InputInjector for NoInjector {
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

/// The fake shield's observable state, shared with the test.
#[derive(Default)]
struct ShieldState {
    engaged: bool,
    engages: u32,
    text: Option<PrivacyScreenText>,
    signal: Option<ShieldSignalFn>,
}

type Shared = Arc<Mutex<ShieldState>>;

struct FakeShield(Shared);

impl PrivacyShield for FakeShield {
    fn engage(&mut self, text: &PrivacyScreenText) -> Result<(), InputError> {
        let mut s = self.0.lock().unwrap();
        s.engaged = true;
        s.engages += 1;
        s.text = Some(text.clone());
        Ok(())
    }
    fn set_text(&mut self, text: &PrivacyScreenText) {
        self.0.lock().unwrap().text = Some(text.clone());
    }
    fn release(&mut self) {
        self.0.lock().unwrap().engaged = false;
    }
    fn is_engaged(&self) -> bool {
        self.0.lock().unwrap().engaged
    }
}

fn text_fn() -> PrivacyTextFn {
    Arc::new(|viewers: &[PrivacyViewer]| PrivacyScreenText {
        lines: std::iter::once(ScreenLine::title("هذا الحاسوب قيد الاستخدام عن بُعد"))
            .chain(
                viewers
                    .iter()
                    .map(|v| ScreenLine::body(format!("{} ({})", v.name, v.device_id))),
            )
            .collect(),
    })
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
    shield: Shared,
    feature: PrivacyFeature,
    session: ViewerSession,
    viewer_events: ViewerEvents,
    _viewer: Viewer,
}

async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, fut)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// Host granting `perms` by one-time code, with the privacy feature (fake shield) on both
/// sides.
async fn rig(perms: Permissions) -> Rig {
    rig_with(perms, None).await
}

/// Like [`rig`]; `factory` replaces the fake shield.
async fn rig_with(perms: Permissions, factory: Option<ShieldFactory>) -> Rig {
    let shield: Shared = Arc::default();
    let s = shield.clone();
    let factory: ShieldFactory = factory.unwrap_or_else(|| {
        Arc::new(move |on_signal: ShieldSignalFn| {
            s.lock().unwrap().signal = Some(on_signal);
            Ok(Box::new(FakeShield(s.clone())) as Box<dyn PrivacyShield>)
        })
    });
    let feature = PrivacyFeature::with_shield_factory(text_fn(), factory);
    let cfg = HostConfig {
        advertise: false,
        default_permissions: perms,
        capturer_factory: Arc::new(|_| {
            Ok(Box::new(IdleCapturer(monitor())) as Box<dyn ScreenCapturer>)
        }),
        monitor_lister: Arc::new(|| Ok(vec![monitor()])),
        injector_factory: Arc::new(|_| Ok(Box::new(NoInjector) as Box<dyn InputInjector>)),
        local_activity_factory: Arc::new(|| Ok(Arc::new(NoLocal) as Arc<dyn LocalActivity>)),
        remote_pointer_marker: false,
        features: FeatureSet::new().with(Arc::new(feature.clone())),
        ..HostConfig::default()
    };
    let host_ep = endpoint(true, "host-pc");
    let target = ConnectTarget::IdAt(host_ep.device_id(), host_ep.local_addr().unwrap());
    let (host, events) = Host::start(host_ep, cfg).await.unwrap();
    let code = host.one_time_code().current();
    let viewer = Viewer::new(
        endpoint(false, "مكتب-Office"),
        ViewerConfig {
            known_hosts: KnownHostsStore::InMemory,
            pixel_format: PixelFormat::Rgba,
            features: FeatureSet::new().with(Arc::new(PrivacyViewerFeature)),
            ..ViewerConfig::default()
        },
    )
    .unwrap();
    let (session, viewer_events) = within(
        "connect",
        viewer.connect(target, AuthMethod::OneTimeCode(code)),
    )
    .await
    .unwrap();
    Rig {
        host,
        events,
        shield,
        feature,
        session,
        viewer_events,
        _viewer: viewer,
    }
}

/// Next privacy state the viewer hears about.
async fn viewer_privacy(events: &mut ViewerEvents) -> bool {
    within("viewer privacy event", async {
        loop {
            match events.recv().await.expect("viewer events closed") {
                ViewerEvent::Feature(e) if e.feature == PRIVACY_FEATURE => {
                    return e.payload::<PrivacyState>().expect("PrivacyState").enabled;
                }
                _ => {}
            }
        }
    })
    .await
}

/// Next privacy status the host UI hears about.
async fn host_privacy(events: &mut HostEvents) -> (SessionId, PrivacyStatus) {
    within("host privacy event", async {
        loop {
            if let HostEvent::Feature { id, event } = events.recv().await.expect("host events") {
                if event.feature == PRIVACY_FEATURE {
                    return (
                        id,
                        *event.payload::<PrivacyStatus>().expect("PrivacyStatus"),
                    );
                }
            }
        }
    })
    .await
}

async fn wait_engaged(shield: &Shared, want: bool) {
    within("shield state", async {
        while shield.lock().unwrap().engaged != want {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
}

fn with_privacy() -> Permissions {
    DEFAULT_PERMISSIONS | Permissions::PRIVACY_MODE
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn viewer_turns_privacy_on_and_off() {
    let mut r = rig(with_privacy()).await;
    assert!(r.session.send_message(privacy_request(true)));
    assert!(viewer_privacy(&mut r.viewer_events).await);
    assert!(r.shield.lock().unwrap().engaged);
    assert!(r.feature.is_active());
    let (_, st) = host_privacy(&mut r.events).await;
    assert_eq!(
        st,
        PrivacyStatus {
            active: true,
            cause: PrivacyCause::Requested
        }
    );
    // The overlay names the connected viewer (Arabic name intact): who-is-connected rule.
    let text = r.shield.lock().unwrap().text.clone().unwrap();
    assert!(
        text.lines
            .iter()
            .any(|l| l.text.starts_with("مكتب-Office (")),
        "{text:?}"
    );

    assert!(r.session.send_message(privacy_request(false)));
    assert!(!viewer_privacy(&mut r.viewer_events).await);
    assert!(!r.shield.lock().unwrap().engaged);
    assert!(!r.feature.is_active());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_permission_the_request_is_refused() {
    let mut r = rig(DEFAULT_PERMISSIONS).await;
    r.session.send_message(privacy_request(true));
    // The host core drops the message (no PRIVACY_MODE): nothing engages, nothing answers.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(r.shield.lock().unwrap().engages, 0);
    assert!(!r.feature.is_active());
    // Granting it live makes the same request work.
    let id = r.host.sessions()[0].id;
    r.host.set_permissions(id, with_privacy());
    within("permission change", async {
        loop {
            if let Some(ViewerEvent::PermissionsChanged(p)) = r.viewer_events.recv().await {
                assert!(p.contains(Permissions::PRIVACY_MODE));
                return;
            }
        }
    })
    .await;
    r.session.send_message(privacy_request(true));
    assert!(viewer_privacy(&mut r.viewer_events).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoking_the_permission_turns_privacy_off() {
    let mut r = rig(with_privacy()).await;
    r.session.send_message(privacy_request(true));
    assert!(viewer_privacy(&mut r.viewer_events).await);
    let id = r.host.sessions()[0].id;
    r.host.set_permissions(id, DEFAULT_PERMISSIONS);
    assert!(!viewer_privacy(&mut r.viewer_events).await);
    wait_engaged(&r.shield, false).await;
    let (_, st) = host_privacy(&mut r.events).await; // the "on" event
    assert!(st.active);
    let (_, st) = host_privacy(&mut r.events).await;
    assert_eq!(st.cause, PrivacyCause::PermissionRevoked);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn viewer_disconnect_turns_privacy_off() {
    let mut r = rig(with_privacy()).await;
    r.session.send_message(privacy_request(true));
    assert!(viewer_privacy(&mut r.viewer_events).await);
    r.session.disconnect();
    wait_engaged(&r.shield, false).await;
    assert!(!r.feature.is_active());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_ending_the_session_turns_privacy_off() {
    let mut r = rig(with_privacy()).await;
    r.session.send_message(privacy_request(true));
    assert!(viewer_privacy(&mut r.viewer_events).await);
    let id = r.host.sessions()[0].id;
    r.host.disconnect(id);
    wait_engaged(&r.shield, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn viewer_app_exit_turns_privacy_off() {
    let r = rig(with_privacy()).await;
    let Rig {
        session,
        mut viewer_events,
        shield,
        host,
        _viewer,
        ..
    } = r;
    session.send_message(privacy_request(true));
    assert!(viewer_privacy(&mut viewer_events).await);
    // The viewer app exits: session, events and the viewer itself are dropped.
    drop(session);
    drop(viewer_events);
    drop(_viewer);
    wait_engaged(&shield, false).await;
    drop(host);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn emergency_exit_turns_privacy_off_and_tells_the_viewer() {
    let mut r = rig(with_privacy()).await;
    r.session.send_message(privacy_request(true));
    assert!(viewer_privacy(&mut r.viewer_events).await);
    // The host user held Ctrl+Alt+Shift+P: the shield reports it from its own thread.
    let signal = r.shield.lock().unwrap().signal.clone().unwrap();
    std::thread::spawn(move || signal(ShieldSignal::EmergencyExit))
        .join()
        .unwrap();
    assert!(!viewer_privacy(&mut r.viewer_events).await);
    wait_engaged(&r.shield, false).await;
    loop {
        let (_, st) = host_privacy(&mut r.events).await;
        if !st.active {
            assert_eq!(st.cause, PrivacyCause::EmergencyExit);
            break;
        }
    }
    // The viewer may ask again; the secure desktop also ends it.
    r.session.send_message(privacy_request(true));
    assert!(viewer_privacy(&mut r.viewer_events).await);
    let signal = r.shield.lock().unwrap().signal.clone().unwrap();
    signal(ShieldSignal::SecureDesktop);
    assert!(!viewer_privacy(&mut r.viewer_events).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsupported_shield_answers_off() {
    let factory: ShieldFactory = Arc::new(|_| Err(InputError::Unsupported));
    let mut r = rig_with(with_privacy(), Some(factory)).await;
    r.session.send_message(privacy_request(true));
    assert!(!viewer_privacy(&mut r.viewer_events).await);
    let (_, st) = host_privacy(&mut r.events).await;
    assert_eq!(
        st,
        PrivacyStatus {
            active: false,
            cause: PrivacyCause::Refused
        }
    );
    assert!(!r.feature.is_active());
}
