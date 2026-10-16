//! Unattended access through the real backend (`Backend` trait), two backends on 127.0.0.1:
//! set the permanent password, sign in with it (wrong, then right), restart the "remote PC"
//! (a recording fake, never the real machine) and get back in automatically after the host
//! backend was torn down and started again from the same data folder and port.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tlp_desk_app::backend::real::{RealBackend, RealConfig};
use tlp_desk_app::backend::unattended::{PasswordChange, PasswordError, SystemAction};
use tlp_desk_app::backend::{
    Backend, BackendEvent, ConnectError, ConnectTarget, EventReceiver, FrameSlot, SessionId,
};
use tlp_desk_capture::{BgraFrame, CaptureError, CursorState, MonitorInfo, ScreenCapturer};
use tlp_desk_host::{HostConfig, HostError, SystemPower};
use tlp_desk_input::{InputError, InputInjector, MonitorRect};
use tlp_desk_proto::{InputEvent, Permissions, RejectReason};
use tlp_desk_viewer::KnownHostsStore;
use tokio::runtime::Runtime;

const WAIT: Duration = Duration::from_secs(40);

fn monitor() -> MonitorInfo {
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

struct FakeCapturer(MonitorInfo);

impl ScreenCapturer for FakeCapturer {
    fn next_frame(&mut self, _timeout: Duration) -> Result<Option<BgraFrame>, CaptureError> {
        std::thread::sleep(Duration::from_millis(30));
        let mut f = BgraFrame::new_tight(self.0.width, self.0.height);
        for y in 0..self.0.height {
            for px in f.row_mut(y).chunks_exact_mut(4) {
                px.copy_from_slice(&[30, 90, 160, 255]);
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
        &self.0
    }
}

struct NoInput;

impl InputInjector for NoInput {
    fn inject(&mut self, _: &InputEvent) -> Result<(), InputError> {
        Ok(())
    }
}

/// Counts restart/lock requests; never touches the machine.
#[derive(Default)]
struct RecordingPower {
    restarts: AtomicU32,
}

impl SystemPower for RecordingPower {
    fn restart(&self) -> Result<(), HostError> {
        self.restarts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn lock_workstation(&self) -> Result<(), HostError> {
        Ok(())
    }
}

fn config(dir: &std::path::Path, port: u16, power: Arc<RecordingPower>) -> RealConfig {
    RealConfig {
        data_dir: dir.to_path_buf(),
        bind: format!("127.0.0.1:{port}").parse().unwrap(),
        device_name: Some("مكتب-pc".into()),
        known_hosts: KnownHostsStore::File(dir.join("known_hosts")),
        host: HostConfig {
            advertise: false,
            capturer_factory: Arc::new(|_, _| {
                Ok(Box::new(FakeCapturer(monitor())) as Box<dyn ScreenCapturer>)
            }),
            injector_factory: Arc::new(|_r: MonitorRect| {
                Ok(Box::new(NoInput) as Box<dyn InputInjector>)
            }),
            monitor_lister: Arc::new(|| Ok(vec![monitor()])),
            local_input_grace: Duration::ZERO,
            remote_pointer_marker: false,
            ..HostConfig::default()
        },
        fallback_discovery: false,
        power,
        system_permissions: true,
        // Never record or play real sound in tests.
        audio: tlp_desk_app::backend::audio::AudioDevices::fake(),
        // Behave the same whether or not the real TLP-Desk service is installed here.
        mirror_service: false,
        ..RealConfig::default()
    }
}

fn wait<T>(
    rt: &Runtime,
    rx: &mut EventReceiver,
    what: &str,
    mut pick: impl FnMut(&BackendEvent) -> Option<T>,
) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let ev = rt
            .block_on(async { tokio::time::timeout(left, rx.recv()).await })
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
            .expect("event channel closed");
        if let Some(v) = pick(&ev) {
            return v;
        }
    }
}

fn wait_frame(frames: &FrameSlot) {
    let deadline = Instant::now() + WAIT;
    while frames.take().is_none() {
        assert!(Instant::now() < deadline, "no video frame arrived");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn password_sign_in_restart_and_automatic_reconnect() {
    let rt = Runtime::new().unwrap();
    let host_dir = tempfile::tempdir().unwrap();
    let viewer_dir = tempfile::tempdir().unwrap();
    let power = Arc::new(RecordingPower::default());
    let (host, mut host_rx) = RealBackend::start_concrete(
        rt.handle(),
        Arc::new(|| {}),
        config(host_dir.path(), 0, power.clone()),
    );
    let (viewer, mut viewer_rx) = RealBackend::start_concrete(
        rt.handle(),
        Arc::new(|| {}),
        config(viewer_dir.path(), 0, Arc::default()),
    );
    let addr = host.local_addr().unwrap();
    let target = ConnectTarget::Address(addr);
    assert!(!host.unattended_password_state().set);

    // Set the permanent password (Arabic, with digits and Latin).
    let password = "كلمة المرور 2026 Desk";
    host.set_unattended_password(None, Some("قصيرة".into()));
    let r = wait(&rt, &mut host_rx, "too short", |e| match e {
        BackendEvent::PasswordChanged(r) => Some(r.clone()),
        _ => None,
    });
    assert_eq!(r, Err(PasswordError::TooShort));
    host.set_unattended_password(None, Some(password.into()));
    let r = wait(&rt, &mut host_rx, "password set", |e| match e {
        BackendEvent::PasswordChanged(r) => Some(r.clone()),
        _ => None,
    });
    assert_eq!(r, Ok(PasswordChange::Set));
    assert!(host.unattended_password_state().set);
    if !host.unattended_password_state().admin {
        host.set_unattended_password(Some("خطأ خطأ خطأ".into()), None);
        let r = wait(&rt, &mut host_rx, "wrong current", |e| match e {
            BackendEvent::PasswordChanged(r) => Some(r.clone()),
            _ => None,
        });
        assert_eq!(r, Err(PasswordError::WrongCurrent));
    }

    // Wrong password, then the right one.
    let attempt = viewer.connect_with_password(target.clone(), "كلمة خاطئة".into());
    let err = wait(&rt, &mut viewer_rx, "wrong password", |e| match e {
        BackendEvent::ConnectFailed { attempt: a, error } if *a == attempt => Some(error.clone()),
        _ => None,
    });
    assert_eq!(err, ConnectError::Rejected(RejectReason::WrongCredentials));
    let attempt = viewer.connect_with_password(target, password.into());
    let (session, frames, perms): (SessionId, Arc<FrameSlot>, Permissions) =
        wait(&rt, &mut viewer_rx, "password session", |e| match e {
            BackendEvent::SessionStarted {
                attempt: a,
                session,
                frames,
                permissions,
                ..
            } if *a == attempt => Some((*session, frames.clone(), *permissions)),
            _ => None,
        });
    assert!(perms.contains(Permissions::RESTART) && perms.contains(Permissions::SYSTEM));
    wait_frame(&frames);

    // Restart the "remote PC".
    viewer.system_action(session, SystemAction::Restart);
    let deadline = Instant::now() + WAIT;
    while power.restarts.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline, "restart never requested");
        std::thread::sleep(Duration::from_millis(20));
    }
    // The ticket left before the restart call; give loopback a moment to deliver it.
    std::thread::sleep(Duration::from_millis(300));

    // "Reboot": the host app goes away and starts again from the same folder and port.
    drop(host);
    drop(host_rx);
    wait(&rt, &mut viewer_rx, "restarting", |e| {
        matches!(e, BackendEvent::RemoteRestarting { session: s } if *s == session).then_some(())
    });
    let mut again = None;
    for _ in 0..40 {
        let (h, mut rx) = RealBackend::start_concrete(
            rt.handle(),
            Arc::new(|| {}),
            config(host_dir.path(), addr.port(), power.clone()),
        );
        let busy = std::iter::from_fn(|| rx.try_recv().ok())
            .any(|e| matches!(e, BackendEvent::HostUnavailable(_)));
        if !busy {
            again = Some((h, rx));
            break;
        }
        drop(h);
        std::thread::sleep(Duration::from_millis(250));
    }
    let (host, mut host_rx) = again.expect("port free again after the host stopped");
    assert!(
        host.unattended_password_state().set,
        "password survives the restart"
    );

    let permissions = wait(&rt, &mut viewer_rx, "reconnected", |e| match e {
        BackendEvent::Reconnected {
            session: s,
            permissions,
        } if *s == session => Some(*permissions),
        BackendEvent::SessionEnded { session: s, reason } if *s == session => {
            panic!("session ended instead of reconnecting: {reason:?}")
        }
        _ => None,
    });
    assert_eq!(permissions, perms);
    wait(&rt, &mut host_rx, "host sees the viewer again", |e| {
        matches!(e, BackendEvent::HostSessionStarted { .. }).then_some(())
    });
    // Video resumes in the very same frame slot.
    wait_frame(&frames);
    viewer.disconnect(session);
    drop(host);
}
