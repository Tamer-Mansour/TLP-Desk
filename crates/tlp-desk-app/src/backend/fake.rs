//! In-process fake backend so the GUI is fully runnable before the real network, capture and
//! codec lanes land. It never touches the network.
//!
//! * three fake LAN devices (one flips online/offline every few seconds);
//! * connecting to any of them (by ID, name or IP) or to any private IP succeeds after a short
//!   "waiting for accept" phase; unknown IDs/names fail with `NotFound`, offline devices with
//!   `Unreachable`, public IPs with `NotLocalNetwork`; `192.168.1.66` fails once with
//!   `HostKeyChanged` until [`Backend::forget_host_key`] is called;
//! * with a one-time code the "waiting for accept" phase is skipped;
//! * the "remote screen" is a synthetic animated 1280x720 test pattern at 30 fps that echoes the
//!   input it receives (crosshair at the pointer, indicators for buttons/keys/wheel);
//! * the fake host has two monitors (1280x720 primary, 1024x768 secondary with a warm-tinted
//!   pattern) that can be switched with [`Backend::select_monitor`];
//! * a simulated incoming connection request arrives ~6 s after start;
//! * unattended (TLP-unattended): password sign-in succeeds unless the password starts with
//!   'wrong' (wrong credentials) or is 'locked' (locked out); the permanent password can be set
//!   and removed; 'Restart remote PC' goes down after ~1.5 s, retries three times and comes back.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tlp_desk_proto::{InputEvent, Permissions};
use tokio::runtime::Handle;
use tokio::task::AbortHandle;

use super::{
    AttemptId, Backend, BackendEvent, ConnectError, ConnectTarget, DeviceId, DiscoveredDevice,
    EndReason, EventReceiver, EventSender, FrameSlot, LocalInfo, MonitorDesc, PeerInfo, RepaintFn,
    RequestId, SessionId, SessionStats, VideoImage,
};

const FPS: u64 = 30;

/// The fake host's monitors (also used by the README media generator).
pub fn fake_monitors() -> Vec<MonitorDesc> {
    vec![
        MonitorDesc {
            index: 0,
            name: "\\\\.\\DISPLAY1".into(),
            width: 1280,
            height: 720,
            primary: true,
            x: 0,
            y: 0,
        },
        MonitorDesc {
            index: 1,
            name: "\\\\.\\DISPLAY2".into(),
            width: 1024,
            height: 768,
            primary: false,
            x: 1280,
            y: 0,
        },
    ]
}

/// Fake implementation of [`Backend`].
pub struct FakeBackend {
    inner: Arc<Inner>,
}

struct Inner {
    rt: Handle,
    tx: EventSender,
    repaint: RepaintFn,
    next_id: AtomicU64,
    state: Mutex<State>,
}

struct State {
    local: LocalInfo,
    devices: Vec<DiscoveredDevice>,
    attempts: HashMap<AttemptId, AbortHandle>,
    sessions: HashMap<SessionId, SessionCtl>,
    requests: HashMap<RequestId, PeerInfo>,
    /// The simulated "reinstalled" host at 192.168.1.66 has a changed key until forgotten.
    key_changed: bool,
    /// Permanent password of the fake host (in memory only; TLP-unattended).
    password: Option<String>,
    rng: u64,
}

/// Address of the simulated host whose key changed.
const KEY_CHANGED_IP: [u8; 4] = [192, 168, 1, 66];

struct SessionCtl {
    task: Option<AbortHandle>,
    remote: Arc<Mutex<RemoteState>>,
}

/// What the fake "remote PC" has received, drawn into the test pattern.
#[derive(Default)]
struct RemoteState {
    cursor: (u16, u16),
    buttons: HashSet<u8>,
    keys: HashSet<(u16, bool)>,
    wheel: i32,
    input_events: u64,
    /// Streamed monitor index.
    monitor: u32,
}

impl FakeBackend {
    /// Start the fake backend on `rt`. Returns the command handle and the event stream.
    pub fn start(rt: &Handle, repaint: RepaintFn) -> (Arc<dyn Backend>, EventReceiver) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut rng = 0x9E37_79B9_7F4A_7C15_u64;
        let inner = Arc::new(Inner {
            rt: rt.clone(),
            tx,
            repaint,
            next_id: AtomicU64::new(1),
            state: Mutex::new(State {
                local: LocalInfo {
                    device_id: DeviceId(482_913_057),
                    device_name: "DESKTOP-TLP01".into(),
                    addresses: vec![
                        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 23)),
                        IpAddr::V4(Ipv4Addr::new(10, 0, 0, 8)),
                    ],
                    one_time_code: next_code(&mut rng),
                },
                devices: vec![
                    device(715_204_386, "Office-PC", [192, 168, 1, 40], true),
                    device(306_117_942, "حاسوب-المكتبة", [192, 168, 1, 57], true),
                    device(950_442_118, "Lab-Laptop", [192, 168, 1, 81], false),
                ],
                attempts: HashMap::new(),
                sessions: HashMap::new(),
                requests: HashMap::new(),
                key_changed: true,
                password: None,
                rng,
            }),
        });
        inner.spawn_discovery();
        inner.spawn_incoming_request();
        (Arc::new(FakeBackend { inner }), rx)
    }
}

fn device(id: u32, name: &str, ip: [u8; 4], online: bool) -> DiscoveredDevice {
    DiscoveredDevice {
        device_id: DeviceId(id),
        name: name.into(),
        address: IpAddr::V4(Ipv4Addr::from(ip)),
        online,
    }
}

/// xorshift64 — good enough for fake codes and jitter.
fn next_rand(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

fn next_code(rng: &mut u64) -> String {
    format!("{:06}", next_rand(rng) % 1_000_000)
}

fn is_lan(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local() || v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback() || (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

impl Inner {
    fn emit(&self, event: BackendEvent) {
        if self.tx.send(event).is_ok() {
            (self.repaint)();
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn spawn_discovery(self: &Arc<Self>) {
        let me = Arc::clone(self);
        self.rt.spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let mut tick = 0usize;
            loop {
                let devices = me.state().devices.clone();
                me.emit(BackendEvent::Devices(devices));
                tokio::time::sleep(Duration::from_secs(5)).await;
                tick += 1;
                let mut st = me.state();
                // Flip the last device every other tick to exercise online/offline states.
                if tick % 2 == 0 {
                    if let Some(d) = st.devices.last_mut() {
                        d.online = !d.online;
                    }
                }
            }
        });
    }

    fn spawn_incoming_request(self: &Arc<Self>) {
        let me = Arc::clone(self);
        self.rt.spawn(async move {
            tokio::time::sleep(Duration::from_secs(6)).await;
            let request = RequestId(me.next_id());
            let peer = PeerInfo {
                device_id: DeviceId(950_442_118),
                name: "Lab-Laptop".into(),
                address: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 81)),
            };
            me.state().requests.insert(request, peer.clone());
            me.emit(BackendEvent::IncomingRequest { request, peer });
        });
    }

    fn resolve(&self, target: &ConnectTarget) -> Result<PeerInfo, ConnectError> {
        let st = self.state();
        let found = match target {
            ConnectTarget::Id(id) => st.devices.iter().find(|d| d.device_id == *id),
            ConnectTarget::Name(n) => st.devices.iter().find(|d| d.name.eq_ignore_ascii_case(n)),
            ConnectTarget::Address(sa) => {
                if !is_lan(sa.ip()) {
                    return Err(ConnectError::NotLocalNetwork);
                }
                if sa.ip() == IpAddr::V4(Ipv4Addr::from(KEY_CHANGED_IP)) && st.key_changed {
                    return Err(ConnectError::HostKeyChanged {
                        previous: "3f9a 1c07 e2b4 58d1".into(),
                        presented: "b70e 44a2 9c13 0f6e".into(),
                    });
                }
                match st.devices.iter().find(|d| d.address == sa.ip()) {
                    Some(d) => Some(d),
                    None => {
                        return Ok(PeerInfo {
                            device_id: DeviceId(123_456_789),
                            name: sa.ip().to_string(),
                            address: sa.ip(),
                        })
                    }
                }
            }
        };
        match found {
            Some(d) if !d.online => Err(ConnectError::Unreachable),
            Some(d) => Ok(PeerInfo {
                device_id: d.device_id,
                name: d.name.clone(),
                address: d.address,
            }),
            None => Err(ConnectError::NotFound),
        }
    }

    async fn run_attempt(
        self: Arc<Self>,
        attempt: AttemptId,
        target: ConnectTarget,
        code: Option<String>,
    ) {
        tokio::time::sleep(Duration::from_millis(600)).await;
        let peer = match self.resolve(&target) {
            Ok(p) => p,
            Err(error) => {
                self.state().attempts.remove(&attempt);
                self.emit(BackendEvent::ConnectFailed { attempt, error });
                return;
            }
        };
        if code.is_none() {
            self.emit(BackendEvent::AwaitingAccept { attempt });
            tokio::time::sleep(Duration::from_millis(1200)).await;
        }

        let session = SessionId(self.next_id());
        let frames = FrameSlot::new();
        let remote = Arc::new(Mutex::new(RemoteState::default()));
        {
            let mut st = self.state();
            if st.attempts.remove(&attempt).is_none() {
                return; // cancelled
            }
            let me = Arc::clone(&self);
            let slot = Arc::clone(&frames);
            let rem = Arc::clone(&remote);
            let task = self
                .rt
                .spawn(async move { me.run_video(session, slot, rem).await })
                .abort_handle();
            st.sessions.insert(
                session,
                SessionCtl {
                    task: Some(task),
                    remote,
                },
            );
        }
        self.emit(BackendEvent::SessionStarted {
            attempt,
            session,
            peer,
            permissions: Permissions::ALL,
            frames,
        });
        self.emit(BackendEvent::MonitorsChanged {
            session,
            monitors: fake_monitors(),
            active: 0,
        });
    }

    async fn run_video(
        self: Arc<Self>,
        session: SessionId,
        slot: Arc<FrameSlot>,
        remote: Arc<Mutex<RemoteState>>,
    ) {
        let mut interval = tokio::time::interval(Duration::from_micros(1_000_000 / FPS));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let start = Instant::now();
        let mut seq = 0u64;
        let mut window_start = Instant::now();
        let mut window_frames = 0u32;
        loop {
            interval.tick().await;
            let t = start.elapsed().as_secs_f32();
            let (width, height, rgba) = {
                let r = remote.lock().unwrap_or_else(|e| e.into_inner());
                let mons = fake_monitors();
                let m = mons
                    .iter()
                    .find(|m| m.index == r.monitor)
                    .unwrap_or(&mons[0]);
                (
                    m.width,
                    m.height,
                    render_test_pattern(m.width, m.height, t, &r),
                )
            };
            slot.publish(VideoImage {
                width,
                height,
                rgba,
                seq,
            });
            (self.repaint)();
            seq += 1;
            window_frames += 1;

            let elapsed = window_start.elapsed();
            if elapsed >= Duration::from_secs(1) {
                let jitter = (next_rand(&mut self.state().rng) % 1000) as f32 / 1000.0;
                let stats = SessionStats {
                    fps: window_frames as f32 / elapsed.as_secs_f32(),
                    latency_ms: 14.0 + jitter * 9.0,
                    bitrate_kbps: 9_000 + (jitter * 4_000.0) as u32,
                };
                self.emit(BackendEvent::Stats { session, stats });
                window_start = Instant::now();
                window_frames = 0;
            }
        }
    }
}

impl Backend for FakeBackend {
    fn local_info(&self) -> LocalInfo {
        self.inner.state().local.clone()
    }

    fn set_device_name(&self, name: String) {
        let info = {
            let mut st = self.inner.state();
            st.local.device_name = name;
            st.local.clone()
        };
        self.inner.emit(BackendEvent::LocalInfo(info));
    }

    fn refresh_one_time_code(&self) {
        let info = {
            let mut st = self.inner.state();
            let code = next_code(&mut st.rng);
            st.local.one_time_code = code;
            st.local.clone()
        };
        self.inner.emit(BackendEvent::LocalInfo(info));
    }

    fn connect(&self, target: ConnectTarget, code: Option<String>) -> AttemptId {
        let attempt = AttemptId(self.inner.next_id());
        let me = Arc::clone(&self.inner);
        // Hold the lock while spawning so the task cannot observe the map before insertion.
        let mut st = self.inner.state();
        let handle = self
            .inner
            .rt
            .spawn(me.run_attempt(attempt, target, code))
            .abort_handle();
        st.attempts.insert(attempt, handle);
        attempt
    }

    fn cancel_connect(&self, attempt: AttemptId) {
        if let Some(h) = self.inner.state().attempts.remove(&attempt) {
            h.abort();
        }
    }

    fn forget_host_key(&self, _attempt: AttemptId) -> bool {
        std::mem::replace(&mut self.inner.state().key_changed, false)
    }

    fn respond_to_request(&self, request: RequestId, accept: bool) {
        let Some(peer) = self.inner.state().requests.remove(&request) else {
            return;
        };
        if accept {
            let session = SessionId(self.inner.next_id());
            self.inner.state().sessions.insert(
                session,
                SessionCtl {
                    task: None,
                    remote: Arc::default(),
                },
            );
            self.inner
                .emit(BackendEvent::HostSessionStarted { session, peer });
        }
    }

    fn send_input(&self, session: SessionId, event: InputEvent) {
        let st = self.inner.state();
        let Some(ctl) = st.sessions.get(&session) else {
            return;
        };
        let mut r = ctl.remote.lock().unwrap_or_else(|e| e.into_inner());
        r.input_events += 1;
        match event {
            InputEvent::MouseMove { x, y } => r.cursor = (x, y),
            InputEvent::MouseButton { button, down } => {
                let b = button as u8;
                if down {
                    r.buttons.insert(b);
                } else {
                    r.buttons.remove(&b);
                }
            }
            InputEvent::MouseWheel { dy, .. } => r.wheel += i32::from(dy),
            InputEvent::Key {
                scancode,
                extended,
                down,
            } => {
                if down {
                    r.keys.insert((scancode, extended));
                } else {
                    r.keys.remove(&(scancode, extended));
                }
            }
            InputEvent::ReleaseAll => {
                r.keys.clear();
                r.buttons.clear();
            }
        }
    }

    fn set_view_only(&self, _session: SessionId, _view_only: bool) {}

    fn select_monitor(&self, session: SessionId, index: u32) {
        if !fake_monitors().iter().any(|m| m.index == index) {
            return; // like the real host: unknown index ignored
        }
        {
            let st = self.inner.state();
            let Some(ctl) = st.sessions.get(&session) else {
                return;
            };
            let mut r = ctl.remote.lock().unwrap_or_else(|e| e.into_inner());
            r.monitor = index;
            r.cursor = (0, 0);
        }
        self.inner.emit(BackendEvent::MonitorsChanged {
            session,
            monitors: fake_monitors(),
            active: index,
        });
    }

    fn connect_with_password(&self, target: ConnectTarget, password: String) -> AttemptId {
        let refused = if password.starts_with("wrong") {
            Some(tlp_desk_proto::RejectReason::WrongCredentials)
        } else if password == "locked" {
            Some(tlp_desk_proto::RejectReason::RateLimited)
        } else {
            None
        };
        match refused {
            // Like a code login (no accept prompt), with a placeholder code.
            None => self.connect(target, Some("000000".into())),
            Some(reason) => {
                let attempt = AttemptId(self.inner.next_id());
                let me = Arc::clone(&self.inner);
                self.inner.rt.spawn(async move {
                    tokio::time::sleep(Duration::from_millis(600)).await;
                    me.emit(BackendEvent::ConnectFailed {
                        attempt,
                        error: ConnectError::Rejected(reason),
                    });
                });
                attempt
            }
        }
    }

    fn set_unattended_password(&self, current: Option<String>, new: Option<String>) {
        use super::unattended::{PasswordChange, PasswordError};
        let result = {
            let mut st = self.inner.state();
            if st.password.is_some() && st.password != current {
                Err(PasswordError::WrongCurrent)
            } else if new.as_ref().is_some_and(|n| n.chars().count() < 8) {
                Err(PasswordError::TooShort)
            } else {
                let set = new.is_some();
                st.password = new;
                Ok(if set {
                    PasswordChange::Set
                } else {
                    PasswordChange::Removed
                })
            }
        };
        self.inner.emit(BackendEvent::PasswordChanged(result));
        self.inner.emit(BackendEvent::PasswordState(
            self.unattended_password_state(),
        ));
    }

    fn unattended_password_state(&self) -> super::unattended::PasswordState {
        super::unattended::PasswordState {
            set: self.inner.state().password.is_some(),
            machine_wide: true,
            admin: false,
        }
    }

    fn system_action(&self, session: SessionId, action: super::unattended::SystemAction) {
        if action != super::unattended::SystemAction::Restart
            || !self.inner.state().sessions.contains_key(&session)
        {
            return;
        }
        let me = Arc::clone(&self.inner);
        self.inner.rt.spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            me.emit(BackendEvent::RemoteRestarting { session });
            for attempt in 1..=3 {
                tokio::time::sleep(Duration::from_secs(1)).await;
                if !me.state().sessions.contains_key(&session) {
                    return; // stopped waiting
                }
                me.emit(BackendEvent::ReconnectAttempt { session, attempt });
            }
            tokio::time::sleep(Duration::from_millis(800)).await;
            if me.state().sessions.contains_key(&session) {
                me.emit(BackendEvent::Reconnected {
                    session,
                    permissions: Permissions::ALL,
                });
            }
        });
    }

    fn disconnect(&self, session: SessionId) {
        let removed = self.inner.state().sessions.remove(&session);
        if let Some(ctl) = removed {
            if let Some(task) = ctl.task {
                task.abort();
            }
            self.inner.emit(BackendEvent::SessionEnded {
                session,
                reason: EndReason::Local,
            });
        }
    }
}

/// One frame of the fake remote screen at time `t` seconds, for monitor `monitor` (an index from
/// [`fake_monitors`], unknown = the primary) with the remote pointer at `cursor` (normalized
/// `0..=u16::MAX`). A pure function of its arguments, so the README media generator
/// (`examples/readme_media`) gets byte-identical frames on every run.
pub fn test_pattern_frame(monitor: u32, t: f32, cursor: (u16, u16)) -> VideoImage {
    let mons = fake_monitors();
    let m = mons
        .iter()
        .find(|m| m.index == monitor)
        .or(mons.first())
        .map_or((1280, 720, 0), |m| (m.width, m.height, m.index));
    let remote = RemoteState {
        cursor,
        monitor: m.2,
        ..RemoteState::default()
    };
    VideoImage {
        width: m.0,
        height: m.1,
        rgba: render_test_pattern(m.0, m.1, t, &remote),
        seq: 0,
    }
}

/// Synthetic "remote desktop": animated gradient, a grid, a sweeping bar, and indicators that
/// reflect the input received.
fn render_test_pattern(w: u32, h: u32, t: f32, remote: &RemoteState) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let mut px = vec![0u8; w * h * 4];
    let phase = (t * 40.0) as usize;
    let bar_x = ((t * 0.25).fract() * w as f32) as usize;
    let cx = usize::from(remote.cursor.0) * (w - 1) / usize::from(u16::MAX);
    let cy = usize::from(remote.cursor.1) * (h - 1) / usize::from(u16::MAX);
    let wheel_y = (remote.wheel / 12).rem_euclid(h as i32) as usize;

    for y in 0..h {
        let row = &mut px[y * w * 4..(y + 1) * w * 4];
        let gy = (y * 160 / h) as u8;
        for x in 0..w {
            let o = x * 4;
            let mut r = 20 + (x * 90 / w) as u8;
            let mut g = 30 + gy / 2;
            let mut b = 60 + (((x + phase) % 512) as i32 - 256).unsigned_abs() as u8 / 3;
            if x % 80 == 0 || y % 80 == 0 {
                (r, g, b) = (70, 90, 120);
            }
            if x.abs_diff(bar_x) < 6 {
                (r, g, b) = (40, 180, 170);
            }
            if y == wheel_y && x < 120 {
                (r, g, b) = (240, 200, 60);
            }
            if remote.monitor != 0 {
                // Second monitor: warm tint so the switch is obvious.
                (r, g, b) = (b.saturating_add(60), g, r / 2);
            }
            if (x == cx && y.abs_diff(cy) < 24) || (y == cy && x.abs_diff(cx) < 24) {
                (r, g, b) = (255, 255, 255);
            }
            row[o] = r;
            row[o + 1] = g;
            row[o + 2] = b;
            row[o + 3] = 255;
        }
    }

    // Indicator squares, top-left: one per mouse button, then one for "any key held".
    let mut fill = |x0: usize, color: [u8; 3]| {
        for y in 16..48.min(h) {
            for x in x0..(x0 + 32).min(w) {
                let o = (y * w + x) * 4;
                px[o..o + 3].copy_from_slice(&color);
            }
        }
    };
    for b in 0..5u8 {
        let on = remote.buttons.contains(&b);
        fill(
            16 + usize::from(b) * 40,
            if on { [250, 120, 60] } else { [45, 55, 75] },
        );
    }
    let key_on = !remote.keys.is_empty();
    fill(232, if key_on { [90, 220, 120] } else { [45, 55, 75] });
    px
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pattern_has_expected_size_and_is_opaque() {
        let px = render_test_pattern(64, 48, 0.5, &RemoteState::default());
        assert_eq!(px.len(), 64 * 48 * 4);
        assert!(px.chunks_exact(4).all(|p| p[3] == 255));
    }

    #[test]
    fn test_pattern_frame_is_deterministic_per_monitor() {
        let a = test_pattern_frame(1, 2.5, (100, 200));
        assert_eq!((a.width, a.height), (1024, 768));
        assert_eq!(a, test_pattern_frame(1, 2.5, (100, 200)));
        // Unknown monitor index falls back to the primary.
        let p = test_pattern_frame(9, 0.0, (0, 0));
        assert_eq!((p.width, p.height), (1280, 720));
    }

    #[test]
    fn fake_host_switches_monitors() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (backend, mut rx) = FakeBackend::start(rt.handle(), Arc::new(|| {}));
        let attempt = backend.connect(
            ConnectTarget::Address("192.168.1.30:47400".parse().unwrap()),
            Some("123456".into()),
        );
        let (session, frames) = rt.block_on(async {
            loop {
                match rx.recv().await.unwrap() {
                    BackendEvent::SessionStarted {
                        attempt: a,
                        session,
                        frames,
                        ..
                    } if a == attempt => return (session, frames),
                    _ => {}
                }
            }
        });
        let next_monitors = |rx: &mut EventReceiver| {
            rt.block_on(async {
                loop {
                    if let BackendEvent::MonitorsChanged {
                        session: s,
                        monitors,
                        active,
                    } = rx.recv().await.unwrap()
                    {
                        assert_eq!(s, session);
                        return (monitors, active);
                    }
                }
            })
        };
        let (mons, active) = next_monitors(&mut rx);
        assert_eq!((mons.len(), active), (2, 0));
        backend.select_monitor(session, 1);
        let (_, active) = next_monitors(&mut rx);
        assert_eq!(active, 1);
        let frame = rt.block_on(async {
            loop {
                tokio::time::sleep(Duration::from_millis(20)).await;
                if let Some(f) = frames.take() {
                    if f.width == 1024 {
                        return f;
                    }
                }
            }
        });
        assert_eq!((frame.width, frame.height), (1024, 768));
        // Unknown index: nothing happens.
        backend.select_monitor(session, 9);
        backend.disconnect(session);
    }

    #[test]
    fn lan_filter() {
        assert!(is_lan("192.168.0.1".parse().unwrap()));
        assert!(is_lan("169.254.3.3".parse().unwrap()));
        assert!(!is_lan("8.8.8.8".parse().unwrap()));
    }
}
