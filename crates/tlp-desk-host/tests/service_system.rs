//! The service's `system` feature end to end on loopback: Ctrl+Alt+Del and lock workstation
//! are gated by the `SYSTEM` permission (also when revoked live), and viewers receive
//! `DesktopState` on connect and on every change. System actions are fakes: nothing here
//! calls `SendSAS` or locks the developer's PC.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tlp_desk_capture::{BgraFrame, CaptureError, CursorState, MonitorInfo, ScreenCapturer};
use tlp_desk_crypto::Identity;
use tlp_desk_host::service::{
    DesktopStatus, SystemAction, SystemActions, SystemControl, SystemEvent, SYSTEM_FEATURE,
};
use tlp_desk_host::{Host, HostConfig, HostError, HostEvent, HostEvents};
use tlp_desk_input::{InputError, InputEvent, InputInjector, LocalActivity, MonitorRect};
use tlp_desk_net::{Endpoint, EndpointConfig, Feature, FeatureCtx};
use tlp_desk_proto::{Message, Permissions};
use tlp_desk_viewer::{
    AuthMethod, ConnectTarget, KnownHostsStore, Viewer, ViewerConfig, ViewerEvent, ViewerEvents,
    ViewerSession,
};
use tokio::sync::{mpsc, watch};

const WAIT: Duration = Duration::from_secs(15);

struct StillCapturer(MonitorInfo);

impl ScreenCapturer for StillCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<BgraFrame>, CaptureError> {
        std::thread::sleep(timeout.min(Duration::from_millis(30)));
        Ok(Some(BgraFrame::new_tight(self.0.width, self.0.height)))
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
        width: 64,
        height: 48,
        primary: true,
        x: 0,
        y: 0,
    }
}

struct NullInjector;

impl InputInjector for NullInjector {
    fn inject(&mut self, _event: &InputEvent) -> Result<(), InputError> {
        Ok(())
    }
}

struct NoLocal;

impl LocalActivity for NoLocal {
    fn last_physical_input(&self) -> Option<Instant> {
        None
    }
}

#[derive(Default)]
struct FakeActions {
    cad: AtomicU32,
    lock: AtomicU32,
}

impl SystemActions for FakeActions {
    fn send_ctrl_alt_del(&self) -> Result<(), HostError> {
        self.cad.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn lock_workstation(&self) -> Result<(), HostError> {
        self.lock.fetch_add(1, Ordering::SeqCst);
        Err(HostError::NoAgent)
    }
}

/// Viewer-side probe: forwards every `DesktopState` to the test as a feature event.
struct DesktopProbe;

impl Feature for DesktopProbe {
    fn name(&self) -> &'static str {
        "desktop-probe"
    }
    fn handles(&self, msg: &Message) -> bool {
        matches!(msg, Message::DesktopState { .. })
    }
    fn start(&self, ctx: FeatureCtx, mut inbox: mpsc::UnboundedReceiver<Message>) {
        tokio::spawn(async move {
            while let Some(Message::DesktopState {
                secure_desktop,
                locked,
            }) = inbox.recv().await
            {
                ctx.emit(
                    "desktop-probe",
                    DesktopStatus {
                        secure_desktop,
                        locked,
                    },
                );
            }
        });
    }
}

fn endpoint(listen: bool) -> Arc<Endpoint> {
    Arc::new(
        Endpoint::bind(
            Arc::new(Identity::generate().unwrap()),
            EndpointConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                listen,
                device_name: if listen { "مضيف" } else { "عارض" }.into(),
                ..EndpointConfig::default()
            },
        )
        .unwrap(),
    )
}

struct Setup {
    host: Host,
    events: HostEvents,
    actions: Arc<FakeActions>,
    desktop: watch::Sender<DesktopStatus>,
    session: ViewerSession,
    vevents: ViewerEvents,
}

async fn setup(perms: Permissions) -> Setup {
    let actions = Arc::new(FakeActions::default());
    let (desktop, desktop_rx) = watch::channel(DesktopStatus {
        secure_desktop: false,
        locked: true,
    });
    let system = SystemControl::new(actions.clone(), desktop_rx);
    let mon = monitor();
    let cfg = HostConfig {
        advertise: false,
        default_permissions: perms,
        capturer_factory: Arc::new(move |_, _| {
            Ok(Box::new(StillCapturer(mon.clone())) as Box<dyn ScreenCapturer>)
        }),
        monitor_lister: Arc::new(|| Ok(vec![monitor()])),
        injector_factory: Arc::new(|_: MonitorRect| {
            Ok(Box::new(NullInjector) as Box<dyn InputInjector>)
        }),
        local_activity_factory: Arc::new(|| Ok(Arc::new(NoLocal) as Arc<dyn LocalActivity>)),
        remote_pointer_marker: false,
        features: tlp_desk_net::FeatureSet::new().with(Arc::new(system)),
        ..HostConfig::default()
    };
    let host_ep = endpoint(true);
    let target = ConnectTarget::IdAt(host_ep.device_id(), host_ep.local_addr().unwrap());
    let (host, events) = Host::start(host_ep, cfg).await.unwrap();
    let viewer = Viewer::new(
        endpoint(false),
        ViewerConfig {
            known_hosts: KnownHostsStore::InMemory,
            features: tlp_desk_net::FeatureSet::new().with(Arc::new(DesktopProbe)),
            ..ViewerConfig::default()
        },
    )
    .unwrap();
    let code = host.one_time_code().current();
    let (session, vevents) =
        tokio::time::timeout(WAIT, viewer.connect(target, AuthMethod::OneTimeCode(code)))
            .await
            .expect("connect timed out")
            .expect("connect");
    Setup {
        host,
        events,
        actions,
        desktop,
        session,
        vevents,
    }
}

async fn next_desktop(ev: &mut ViewerEvents) -> DesktopStatus {
    tokio::time::timeout(WAIT, async {
        loop {
            match ev.recv().await.expect("viewer events ended") {
                ViewerEvent::Feature(f) => {
                    if let Some(s) = f.payload::<DesktopStatus>() {
                        return *s;
                    }
                }
                ViewerEvent::Ended(r) => panic!("session ended: {r:?}"),
                _ => {}
            }
        }
    })
    .await
    .expect("timed out waiting for DesktopState")
}

async fn next_system_event(ev: &mut HostEvents) -> SystemEvent {
    tokio::time::timeout(WAIT, async {
        loop {
            if let HostEvent::Feature { event, .. } = ev.recv().await.expect("host events") {
                if event.feature == SYSTEM_FEATURE {
                    return event.payload::<SystemEvent>().expect("payload").clone();
                }
            }
        }
    })
    .await
    .expect("timed out waiting for a system event")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ctrl_alt_del_and_lock_need_system_permission() {
    let mut s = setup(Permissions::VIEW | Permissions::CONTROL | Permissions::SYSTEM).await;
    // DesktopState arrives right after connecting, then on every change.
    assert_eq!(
        next_desktop(&mut s.vevents).await,
        DesktopStatus {
            secure_desktop: false,
            locked: true
        }
    );
    s.desktop.send_replace(DesktopStatus {
        secure_desktop: true,
        locked: true,
    });
    assert!(next_desktop(&mut s.vevents).await.secure_desktop);

    assert!(s.session.send_message(Message::SendCtrlAltDel));
    let ev = next_system_event(&mut s.events).await;
    assert_eq!(ev.action, SystemAction::CtrlAltDel);
    assert_eq!(ev.result, Ok(()));
    assert_eq!(s.actions.cad.load(Ordering::SeqCst), 1);

    // A burst is throttled to one action per second.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    for _ in 0..5 {
        assert!(s.session.send_message(Message::LockWorkstation));
    }
    let ev = next_system_event(&mut s.events).await;
    assert_eq!(ev.action, SystemAction::LockWorkstation);
    assert!(ev.result.is_err(), "fake lock reports no agent");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.actions.lock.load(Ordering::SeqCst), 1);

    // Revoking SYSTEM live stops further actions.
    let id = s.host.sessions()[0].id;
    assert!(s
        .host
        .set_permissions(id, Permissions::VIEW | Permissions::CONTROL));
    tokio::time::timeout(WAIT, async {
        while s.session.permissions().contains(Permissions::SYSTEM) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(s.session.send_message(Message::SendCtrlAltDel));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(s.actions.cad.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_system_permission_nothing_happens() {
    let mut s = setup(Permissions::VIEW | Permissions::CONTROL | Permissions::KEYBOARD).await;
    // The desktop state is informational and still reaches every viewer.
    assert!(next_desktop(&mut s.vevents).await.locked);
    assert!(s.session.send_message(Message::SendCtrlAltDel));
    assert!(s.session.send_message(Message::LockWorkstation));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(s.actions.cad.load(Ordering::SeqCst), 0);
    assert_eq!(s.actions.lock.load(Ordering::SeqCst), 0);
}
