//! Service-side proxies for the session agent: a capturer, an injector and a monitor lister
//! that forward to the agent over its pipe, packaged as the factories of [`HostConfig`].
//!
//! Agent restarts are expected (logon, logoff, user switch, crash). The proxies therefore
//! never hold the session hostage:
//! * a capture connection that breaks reports [`CaptureError::SurfaceLost`], so the host's
//!   video pipeline re-opens through the factory (retrying until the new agent is up);
//! * the injector reconnects lazily on the next event;
//! * the monitor lister falls back to "unknown" (the pipeline keeps the last list).
//!
//! [`HostConfig`]: crate::HostConfig

use std::io::{Read, Write};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::Duration;

use tlp_desk_capture::{
    BgraFrame, CaptureError, CaptureOptions, CursorShape, CursorState, MonitorInfo, ScreenCapturer,
};
use tlp_desk_input::{InputError, InputEvent, InputInjector, MonitorRect};

use super::ipc::{
    self, AgentChannel, AgentCommand, AgentHello, AgentWelcome, CaptureReply, CaptureRequest,
    CommandReply, InputCommand, WireCursor, WireError, WireMonitor, WireRect, IPC_VERSION,
};
use crate::{CapturerFactory, HostError, InjectorFactory, MonitorLister};

/// A bidirectional byte stream to the agent (a pipe client handle, or an in-memory pipe in
/// tests).
pub trait AgentConn: Read + Write + Send {}

impl<T: Read + Write + Send> AgentConn for T {}

/// Opens new connections to the running agent's pipe.
pub trait AgentConnector: Send + Sync + 'static {
    /// One new connection (the caller sends the [`AgentHello`]).
    fn connect(&self) -> Result<Box<dyn AgentConn>, HostError>;
}

/// Largest frame dimension accepted from the agent (a corrupt header must not make the
/// service allocate or encode something absurd).
const MAX_DIMENSION: u32 = 16_384;

/// Buffers kept for reuse between frames.
const FRAME_POOL: usize = 3;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The service's handle on "the current agent". Cheap to clone through `Arc`; the factories
/// it produces always talk to whichever agent is current.
#[derive(Default)]
pub struct AgentLink {
    connector: RwLock<Option<Arc<dyn AgentConnector>>>,
    command: Mutex<Option<Box<dyn AgentConn>>>,
}

impl std::fmt::Debug for AgentLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentLink")
            .field("connected", &self.is_connected())
            .finish_non_exhaustive()
    }
}

impl AgentLink {
    /// A link with no agent yet.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Points the link at a new agent (`None`: the agent is gone).
    pub fn set(&self, connector: Option<Arc<dyn AgentConnector>>) {
        *self.connector.write().unwrap_or_else(|p| p.into_inner()) = connector;
        // The cached command connection belongs to the old agent.
        lock(&self.command).take();
    }

    /// Whether an agent is set.
    pub fn is_connected(&self) -> bool {
        self.connector
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
    }

    /// Opens a channel: connect, hello, welcome. Returns the connection and the agent's
    /// session id.
    pub fn open(&self, channel: AgentChannel) -> Result<(Box<dyn AgentConn>, u32), HostError> {
        let connector = self
            .connector
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or(HostError::NoAgent)?;
        let mut conn = connector.connect()?;
        ipc::write_msg(
            &mut conn,
            &AgentHello {
                version: IPC_VERSION,
                channel,
            },
        )?;
        match ipc::read_msg::<_, AgentWelcome>(&mut conn)? {
            AgentWelcome::Ok {
                version,
                session_id,
            } if version == IPC_VERSION => Ok((conn, session_id)),
            AgentWelcome::Ok { version, .. } => Err(HostError::IpcVersion {
                expected: IPC_VERSION,
                got: version,
            }),
            AgentWelcome::Refused { error, .. } => Err(refused(error)),
        }
    }

    /// Sends one command on the (cached) command connection; reconnects once if the cached
    /// connection broke.
    pub fn command(&self, cmd: &AgentCommand) -> Result<CommandReply, HostError> {
        let mut cached = lock(&self.command);
        for attempt in 0..2 {
            if cached.is_none() {
                cached.replace(self.open(AgentChannel::Command)?.0);
            }
            let Some(conn) = cached.as_mut() else {
                return Err(HostError::NoAgent);
            };
            let res = ipc::write_msg(conn, cmd).and_then(|()| ipc::read_msg(conn));
            match res {
                Ok(reply) => return Ok(reply),
                Err(e) => {
                    cached.take();
                    if attempt == 1 {
                        return Err(e);
                    }
                }
            }
        }
        Err(HostError::NoAgent)
    }

    /// Locks the console session through the agent.
    pub fn lock_workstation(&self) -> Result<(), HostError> {
        match self.command(&AgentCommand::LockWorkstation)? {
            CommandReply::Ok => Ok(()),
            CommandReply::Error(e) => Err(refused(e)),
            CommandReply::Monitors(_) => Err(HostError::IpcRefused("unexpected reply".into())),
        }
    }

    /// Asks the agent process to exit (before the service stops or retargets it).
    pub fn request_exit(&self) {
        if let Err(e) = self.command(&AgentCommand::Exit) {
            tracing::debug!(%e, "agent exit request failed");
        }
    }

    /// The monitors of the console session.
    pub fn list_monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
        match self.command(&AgentCommand::ListMonitors) {
            Ok(CommandReply::Monitors(list)) => Ok(list.into_iter().map(monitor_info).collect()),
            Ok(CommandReply::Error(e)) => Err(wire_to_capture(e)),
            Ok(CommandReply::Ok) => Err(CaptureError::Platform("unexpected reply".into())),
            Err(e) => Err(CaptureError::Platform(e.to_string())),
        }
    }

    /// [`HostConfig::capturer_factory`](crate::HostConfig::capturer_factory) that captures in
    /// the agent, frames capped at `max_size`.
    pub fn capturer_factory(self: &Arc<Self>, max_size: Option<(u32, u32)>) -> CapturerFactory {
        let link = self.clone();
        Arc::new(move |monitor, options: CaptureOptions| {
            // The quality preset's cap wins over the link default (adaptive resolution steps).
            RemoteCapturer::open(&link, monitor, options.max_size.or(max_size))
                .map(|c| Box::new(c) as Box<dyn ScreenCapturer>)
        })
    }

    /// [`HostConfig::injector_factory`](crate::HostConfig::injector_factory) that injects in
    /// the agent. Never fails: without an agent the injector connects on its first event.
    pub fn injector_factory(self: &Arc<Self>) -> InjectorFactory {
        let link = self.clone();
        Arc::new(move |rect| {
            Ok(Box::new(RemoteInjector::new(link.clone(), rect)) as Box<dyn InputInjector>)
        })
    }

    /// [`HostConfig::monitor_lister`](crate::HostConfig::monitor_lister) through the agent.
    pub fn monitor_lister(self: &Arc<Self>) -> MonitorLister {
        let link = self.clone();
        Arc::new(move || link.list_monitors())
    }
}

fn refused(e: WireError) -> HostError {
    match e {
        WireError::VersionMismatch { expected, got } => HostError::IpcVersion { expected, got },
        other => HostError::IpcRefused(format!("{other:?}")),
    }
}

fn wire_to_capture(e: WireError) -> CaptureError {
    match e {
        WireError::SurfaceLost => CaptureError::SurfaceLost,
        WireError::NoSuchMonitor(m) => CaptureError::NoSuchMonitor(m),
        WireError::Unsupported => CaptureError::Unsupported,
        other => CaptureError::Platform(format!("agent: {other:?}")),
    }
}

pub(crate) fn monitor_info(m: WireMonitor) -> MonitorInfo {
    MonitorInfo {
        index: m.index,
        name: m.name,
        width: m.width,
        height: m.height,
        primary: m.primary,
        x: m.x,
        y: m.y,
    }
}

/// Captures in the agent; one connection per capturer.
pub struct RemoteCapturer {
    conn: Box<dyn AgentConn>,
    monitor: MonitorInfo,
    frame_size: (u32, u32),
    cursor: CursorState,
    pool: Vec<Vec<u8>>,
    broken: bool,
}

impl std::fmt::Debug for RemoteCapturer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteCapturer")
            .field("monitor", &self.monitor)
            .field("frame_size", &self.frame_size)
            .field("broken", &self.broken)
            .finish_non_exhaustive()
    }
}

impl RemoteCapturer {
    /// Opens a capture connection for `monitor`.
    pub fn open(
        link: &AgentLink,
        monitor: u32,
        max_size: Option<(u32, u32)>,
    ) -> Result<Self, CaptureError> {
        let (max_width, max_height) = max_size.unwrap_or((0, 0));
        let (mut conn, _) = link
            .open(AgentChannel::Capture {
                monitor,
                max_width,
                max_height,
            })
            .map_err(|e| match e {
                HostError::IpcRefused(msg) if msg.contains("NoSuchMonitor") => {
                    CaptureError::NoSuchMonitor(monitor)
                }
                e => CaptureError::Platform(e.to_string()),
            })?;
        let opened: CaptureReply =
            ipc::read_msg(&mut conn).map_err(|e| CaptureError::Platform(e.to_string()))?;
        let CaptureReply::Opened {
            monitor: m,
            frame_width,
            frame_height,
        } = opened
        else {
            return Err(CaptureError::Platform(
                "agent sent no capture header".into(),
            ));
        };
        if m.width == 0
            || m.height == 0
            || m.width > MAX_DIMENSION
            || m.height > MAX_DIMENSION
            || frame_width > MAX_DIMENSION
            || frame_height > MAX_DIMENSION
        {
            return Err(CaptureError::Platform(
                "agent reported an invalid monitor".into(),
            ));
        }
        Ok(Self {
            conn,
            monitor: monitor_info(m),
            frame_size: (frame_width, frame_height),
            cursor: CursorState {
                x: 0,
                y: 0,
                visible: false,
                shape: None,
            },
            pool: Vec::new(),
            broken: false,
        })
    }

    fn set_cursor(&mut self, c: WireCursor) {
        let shape = c.shape.and_then(|s| {
            // Bound the cursor image: a corrupt size must not reach the compositor.
            let ok = s.width <= 256
                && s.height <= 256
                && u64::from(s.width) * u64::from(s.height) * 4 == s.rgba.len() as u64;
            ok.then_some(CursorShape {
                width: s.width,
                height: s.height,
                hotspot_x: s.hotspot_x.min(s.width),
                hotspot_y: s.hotspot_y.min(s.height),
                rgba: s.rgba,
            })
        });
        self.cursor.x = c.x;
        self.cursor.y = c.y;
        self.cursor.visible = c.visible;
        if shape.is_some() {
            self.cursor.shape = shape;
        }
    }

    fn lost(&mut self, why: &dyn std::fmt::Display) -> CaptureError {
        if !self.broken {
            tracing::info!(%why, "agent capture connection lost; the pipeline will reopen");
        }
        self.broken = true;
        CaptureError::SurfaceLost
    }
}

impl ScreenCapturer for RemoteCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<BgraFrame>, CaptureError> {
        if self.broken {
            return Err(CaptureError::SurfaceLost);
        }
        let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        if let Err(e) = ipc::write_msg(&mut self.conn, &CaptureRequest::NextFrame { timeout_ms }) {
            return Err(self.lost(&e));
        }
        let reply: CaptureReply = match ipc::read_msg(&mut self.conn) {
            Ok(r) => r,
            Err(e) => return Err(self.lost(&e)),
        };
        match reply {
            CaptureReply::Frame {
                width,
                height,
                stride,
                timestamp_us,
                payload_len,
                cursor,
            } => {
                let mut data = self.pool.pop().unwrap_or_default();
                if let Err(e) = ipc::read_payload_into(&mut self.conn, payload_len, &mut data) {
                    return Err(self.lost(&e));
                }
                self.set_cursor(cursor);
                let frame = BgraFrame {
                    width,
                    height,
                    stride,
                    data,
                    timestamp_us,
                };
                if width > MAX_DIMENSION || height > MAX_DIMENSION || !frame.is_well_formed() {
                    return Err(self.lost(&"agent sent a malformed frame"));
                }
                Ok(Some(frame))
            }
            CaptureReply::NoChange { cursor } => {
                self.set_cursor(cursor);
                Ok(None)
            }
            CaptureReply::Error(WireError::SurfaceLost) => Err(self.lost(&"desktop switched")),
            CaptureReply::Error(e) => Err(wire_to_capture(e)),
            CaptureReply::Opened { .. } => Err(self.lost(&"unexpected capture header")),
        }
    }

    fn cursor(&mut self) -> Result<CursorState, CaptureError> {
        // The shape is reported once, like the local capturers do.
        let shape = self.cursor.shape.take();
        Ok(CursorState {
            shape,
            ..self.cursor.clone()
        })
    }

    fn monitor(&self) -> &MonitorInfo {
        &self.monitor
    }

    fn recycle(&mut self, frame: BgraFrame) {
        if self.pool.len() < FRAME_POOL {
            self.pool.push(frame.data);
        }
    }

    fn frame_size(&self) -> (u32, u32) {
        self.frame_size
    }
}

/// Injects in the agent; reconnects lazily after the agent restarted.
pub struct RemoteInjector {
    link: Arc<AgentLink>,
    rect: MonitorRect,
    conn: Option<Box<dyn AgentConn>>,
}

impl std::fmt::Debug for RemoteInjector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteInjector")
            .field("rect", &self.rect)
            .field("connected", &self.conn.is_some())
            .finish_non_exhaustive()
    }
}

impl RemoteInjector {
    /// An injector for `rect` (connects on first use).
    pub fn new(link: Arc<AgentLink>, rect: MonitorRect) -> Self {
        Self {
            link,
            rect,
            conn: None,
        }
    }

    fn send(&mut self, cmd: &InputCommand) -> Result<(), InputError> {
        for attempt in 0..2 {
            if self.conn.is_none() {
                let r = WireRect {
                    x: self.rect.x,
                    y: self.rect.y,
                    width: self.rect.width,
                    height: self.rect.height,
                };
                match self.link.open(AgentChannel::Input { rect: r }) {
                    Ok((c, _)) => self.conn = Some(c),
                    Err(e) => return Err(InputError::Platform(e.to_string())),
                }
            }
            let Some(conn) = self.conn.as_mut() else {
                break;
            };
            match ipc::write_msg(conn, cmd) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    self.conn = None;
                    if attempt == 1 {
                        return Err(InputError::Platform(e.to_string()));
                    }
                }
            }
        }
        Err(InputError::Platform("agent unavailable".into()))
    }
}

impl InputInjector for RemoteInjector {
    fn inject(&mut self, event: &InputEvent) -> Result<(), InputError> {
        self.send(&InputCommand::Event(event.clone()))
    }

    fn set_monitor(&mut self, monitor: MonitorRect) -> bool {
        self.rect = monitor;
        if self.conn.is_none() {
            return true;
        }
        self.send(&InputCommand::SetMonitor(WireRect {
            x: monitor.x,
            y: monitor.y,
            width: monitor.width,
            height: monitor.height,
        }))
        .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::Instant;

    use super::super::agent::{serve_connection, AgentPlatform};
    use super::super::desktop::DesktopSource;
    use super::super::testutil::duplex;
    use super::*;

    /// What the fake OS looks like; shared by every agent thread.
    #[derive(Default)]
    struct Os {
        desktop: Mutex<String>,
        /// Desktop each capturer was opened on, and whether "secure" was requested.
        opened: Mutex<Vec<(String, bool)>>,
        injected: Mutex<Vec<(MonitorRect, InputEvent)>>,
        injectors: AtomicU32,
        locks: AtomicU32,
        frames: AtomicU32,
    }

    struct Platform(Arc<Os>);

    thread_local! {
        static THREAD_DESKTOP: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    }

    struct FakeDesktop(Arc<Os>);

    impl DesktopSource for FakeDesktop {
        fn input_desktop(&mut self) -> Result<String, HostError> {
            Ok(lock(&self.0.desktop).clone())
        }
        fn attach_thread(&mut self) -> Result<String, HostError> {
            let d = lock(&self.0.desktop).clone();
            THREAD_DESKTOP.with(|t| *t.borrow_mut() = d.clone());
            Ok(d)
        }
    }

    struct FakeCap {
        os: Arc<Os>,
        mon: MonitorInfo,
        size: (u32, u32),
        n: u32,
    }

    impl ScreenCapturer for FakeCap {
        fn next_frame(&mut self, _timeout: Duration) -> Result<Option<BgraFrame>, CaptureError> {
            self.n += 1;
            if self.n % 3 == 0 {
                return Ok(None);
            }
            self.os.frames.fetch_add(1, Ordering::SeqCst);
            let mut f = BgraFrame::new_tight(self.size.0, self.size.1);
            f.data.fill((self.n & 0xff) as u8);
            f.timestamp_us = u64::from(self.n);
            Ok(Some(f))
        }
        fn cursor(&mut self) -> Result<CursorState, CaptureError> {
            Ok(CursorState {
                x: 5,
                y: 6,
                visible: true,
                shape: (self.n == 1).then(|| CursorShape {
                    width: 2,
                    height: 2,
                    hotspot_x: 1,
                    hotspot_y: 1,
                    rgba: vec![9; 16],
                }),
            })
        }
        fn monitor(&self) -> &MonitorInfo {
            &self.mon
        }
        fn frame_size(&self) -> (u32, u32) {
            self.size
        }
    }

    struct FakeInj(Arc<Os>, MonitorRect);

    impl InputInjector for FakeInj {
        fn inject(&mut self, event: &InputEvent) -> Result<(), InputError> {
            lock(&self.0.injected).push((self.1, event.clone()));
            Ok(())
        }
    }

    fn mon(index: u32) -> MonitorInfo {
        MonitorInfo {
            index,
            name: format!(r"\\.\DISPLAY{}", index + 1),
            width: 64,
            height: 32,
            primary: index == 0,
            x: (index * 64) as i32,
            y: 0,
        }
    }

    impl AgentPlatform for Platform {
        fn session_id(&self) -> u32 {
            1
        }
        fn open_capturer(
            &self,
            monitor: u32,
            max_size: Option<(u32, u32)>,
            secure_desktop: bool,
        ) -> Result<Box<dyn ScreenCapturer>, CaptureError> {
            if monitor > 1 {
                return Err(CaptureError::NoSuchMonitor(monitor));
            }
            let desk = THREAD_DESKTOP.with(|t| t.borrow().clone());
            lock(&self.0.opened).push((desk, secure_desktop));
            let size = max_size.map_or((64, 32), |(w, h)| (w.min(64), h.min(32)));
            Ok(Box::new(FakeCap {
                os: self.0.clone(),
                mon: mon(monitor),
                size,
                n: 0,
            }))
        }
        fn list_monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
            Ok(vec![mon(0), mon(1)])
        }
        fn open_injector(&self, rect: MonitorRect) -> Result<Box<dyn InputInjector>, InputError> {
            self.0.injectors.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakeInj(self.0.clone(), rect)))
        }
        fn lock_workstation(&self) -> Result<(), HostError> {
            self.0.locks.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn desktop_source(&self) -> Box<dyn DesktopSource> {
            Box::new(FakeDesktop(self.0.clone()))
        }
    }

    /// Each `connect` starts an agent thread on the other end of a fresh in-memory pipe.
    struct Agent {
        os: Arc<Os>,
        stop: Arc<AtomicBool>,
        dead: AtomicBool,
    }

    impl AgentConnector for Agent {
        fn connect(&self) -> Result<Box<dyn AgentConn>, HostError> {
            if self.dead.load(Ordering::SeqCst) {
                return Err(HostError::NoAgent);
            }
            let (a, mut b) = duplex();
            let os = self.os.clone();
            let stop = self.stop.clone();
            std::thread::spawn(move || {
                let platform = Platform(os);
                let _ = serve_connection(&mut b, &platform, &stop);
            });
            Ok(Box::new(a))
        }
    }

    fn setup() -> (Arc<AgentLink>, Arc<Agent>) {
        let os = Arc::new(Os::default());
        *lock(&os.desktop) = "Default".into();
        let agent = Arc::new(Agent {
            os,
            stop: Arc::default(),
            dead: AtomicBool::new(false),
        });
        let link = AgentLink::new();
        link.set(Some(agent.clone()));
        (link, agent)
    }

    fn wait_until(what: &str, f: impl Fn() -> bool) {
        let t0 = Instant::now();
        while !f() {
            assert!(t0.elapsed() < Duration::from_secs(10), "timed out: {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn frames_and_cursor_cross_the_pipe() {
        let (link, _agent) = setup();
        let factory = link.capturer_factory(Some((32, 16)));
        let mut cap = factory(1, CaptureOptions::default()).unwrap();
        assert_eq!(cap.monitor().index, 1);
        assert_eq!(cap.monitor().x, 64);
        assert_eq!(cap.frame_size(), (32, 16));
        let f = cap.next_frame(Duration::from_millis(5)).unwrap().unwrap();
        assert_eq!((f.width, f.height, f.stride), (32, 16, 128));
        assert!(f.data.iter().all(|&b| b == 1));
        let c = cap.cursor().unwrap();
        assert_eq!((c.x, c.y, c.visible), (5, 6, true));
        assert_eq!(c.shape.as_ref().map(|s| s.rgba.len()), Some(16));
        // The shape is reported once.
        assert!(cap.cursor().unwrap().shape.is_none());
        cap.recycle(f);
        let f2 = cap.next_frame(Duration::from_millis(5)).unwrap().unwrap();
        assert!(f2.data.iter().all(|&b| b == 2));
        // Third request: no change.
        assert!(cap.next_frame(Duration::from_millis(5)).unwrap().is_none());
        // Unknown monitor.
        assert!(matches!(factory(7, CaptureOptions::default()), Err(CaptureError::NoSuchMonitor(7))));
    }

    #[test]
    fn desktop_switch_reopens_capture_on_the_secure_desktop() {
        let (link, agent) = setup();
        let factory = link.capturer_factory(None);
        let mut cap = factory(0, CaptureOptions::default()).unwrap();
        assert!(cap.next_frame(Duration::ZERO).unwrap().is_some());
        // Lock screen / UAC: the input desktop becomes Winlogon.
        *lock(&agent.os.desktop) = "Winlogon".into();
        std::thread::sleep(super::super::agent::DESKTOP_CHECK_EVERY + Duration::from_millis(20));
        assert!(matches!(
            cap.next_frame(Duration::ZERO),
            Err(CaptureError::SurfaceLost)
        ));
        // It stays lost; the pipeline reopens through the factory.
        assert!(matches!(
            cap.next_frame(Duration::ZERO),
            Err(CaptureError::SurfaceLost)
        ));
        let mut cap = factory(0, CaptureOptions::default()).unwrap();
        assert!(cap.next_frame(Duration::ZERO).unwrap().is_some());
        let opened = lock(&agent.os.opened).clone();
        assert_eq!(
            opened,
            vec![("Default".into(), false), ("Winlogon".into(), true)]
        );
    }

    #[test]
    fn agent_restart_is_survived() {
        let (link, agent) = setup();
        let factory = link.capturer_factory(None);
        let mut cap = factory(0, CaptureOptions::default()).unwrap();
        assert!(cap.next_frame(Duration::ZERO).is_ok());
        let mut inj = link.injector_factory()(MonitorRect::new(0, 0, 64, 32)).unwrap();
        inj.inject(&InputEvent::MouseMove { x: 1, y: 2 }).unwrap();
        // The agent goes away (logoff): the link is cleared and connections break.
        agent.dead.store(true, Ordering::SeqCst);
        link.set(None);
        assert!(matches!(factory(0, CaptureOptions::default()), Err(CaptureError::Platform(_))));
        assert!(link.list_monitors().is_err());
        // A new agent comes up.
        let os = agent.os.clone();
        let agent2 = Arc::new(Agent {
            os,
            stop: Arc::default(),
            dead: AtomicBool::new(false),
        });
        link.set(Some(agent2.clone()));
        assert!(factory(0, CaptureOptions::default()).unwrap().next_frame(Duration::ZERO).is_ok());
        assert_eq!(link.list_monitors().unwrap().len(), 2);
        drop(cap);
    }

    #[test]
    fn input_reaches_the_agent_and_follows_monitor_changes() {
        let (link, agent) = setup();
        let r0 = MonitorRect::new(0, 0, 64, 32);
        let r1 = MonitorRect::new(64, 0, 64, 32);
        let mut inj = link.injector_factory()(r0).unwrap();
        let key = InputEvent::Key {
            scancode: 0x1e,
            extended: false,
            down: true,
        };
        inj.inject(&InputEvent::MouseMove { x: 10, y: 20 }).unwrap();
        assert!(inj.set_monitor(r1));
        inj.inject(&key).unwrap();
        inj.release_all().unwrap();
        wait_until("4 events", || lock(&agent.os.injected).len() >= 4);
        let got = lock(&agent.os.injected).clone();
        assert_eq!(got[0], (r0, InputEvent::MouseMove { x: 10, y: 20 }));
        // The fake injector cannot retarget: the old one releases, a new one is created.
        assert_eq!(got[1], (r0, InputEvent::ReleaseAll));
        assert_eq!(got[2], (r1, key));
        assert_eq!(got[3], (r1, InputEvent::ReleaseAll));
        // Dropping the injector closes the connection: the agent releases everything.
        drop(inj);
        wait_until("release on close", || lock(&agent.os.injected).len() >= 5);
    }

    #[test]
    fn input_injector_is_recreated_after_a_desktop_switch() {
        let (link, agent) = setup();
        let mut inj = link.injector_factory()(MonitorRect::new(0, 0, 64, 32)).unwrap();
        inj.inject(&InputEvent::MouseMove { x: 1, y: 1 }).unwrap();
        wait_until("first", || lock(&agent.os.injected).len() == 1);
        *lock(&agent.os.desktop) = "Winlogon".into();
        std::thread::sleep(super::super::agent::DESKTOP_CHECK_EVERY + Duration::from_millis(20));
        inj.inject(&InputEvent::MouseMove { x: 2, y: 2 }).unwrap();
        wait_until("second", || lock(&agent.os.injected).len() == 2);
        assert_eq!(agent.os.injectors.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn commands_lock_list_and_exit() {
        let (link, agent) = setup();
        link.lock_workstation().unwrap();
        assert_eq!(agent.os.locks.load(Ordering::SeqCst), 1);
        let mons = link.monitor_lister()().unwrap();
        assert_eq!(mons.len(), 2);
        assert_eq!(mons[1].name, r"\\.\DISPLAY2");
        link.request_exit();
        assert!(agent.stop.load(Ordering::SeqCst));
    }

    #[test]
    fn no_agent_means_errors_not_panics() {
        let link = AgentLink::new();
        assert!(matches!(
            link.open(AgentChannel::Command),
            Err(HostError::NoAgent)
        ));
        assert!(link.capturer_factory(None)(0, CaptureOptions::default()).is_err());
        assert!(link.lock_workstation().is_err());
        let mut inj = link.injector_factory()(MonitorRect::new(0, 0, 1, 1)).unwrap();
        assert!(inj.inject(&InputEvent::ReleaseAll).is_err());
    }

    #[test]
    fn version_mismatch_is_refused() {
        let os = Arc::new(Os::default());
        let (mut a, mut b) = duplex();
        let t = std::thread::spawn(move || {
            serve_connection(&mut b, &Platform(os), &AtomicBool::new(false))
        });
        ipc::write_msg(
            &mut a,
            &AgentHello {
                version: IPC_VERSION + 1,
                channel: AgentChannel::Command,
            },
        )
        .unwrap();
        let w: AgentWelcome = ipc::read_msg(&mut a).unwrap();
        assert!(matches!(w, AgentWelcome::Refused { .. }));
        assert!(matches!(
            t.join().unwrap(),
            Err(HostError::IpcVersion { .. })
        ));
    }

    #[test]
    fn malformed_frames_from_the_agent_are_refused() {
        // A fake "agent" that announces a frame larger than its payload geometry allows.
        struct Liar;
        impl AgentConnector for Liar {
            fn connect(&self) -> Result<Box<dyn AgentConn>, HostError> {
                let (a, mut b) = duplex();
                std::thread::spawn(move || {
                    let _: AgentHello = ipc::read_msg(&mut b).unwrap();
                    ipc::write_msg(
                        &mut b,
                        &AgentWelcome::Ok {
                            version: IPC_VERSION,
                            session_id: 1,
                        },
                    )
                    .unwrap();
                    ipc::write_msg(
                        &mut b,
                        &CaptureReply::Opened {
                            monitor: wire(mon(0)),
                            frame_width: 64,
                            frame_height: 32,
                        },
                    )
                    .unwrap();
                    let _: CaptureRequest = ipc::read_msg(&mut b).unwrap();
                    let hdr = CaptureReply::Frame {
                        width: 64,
                        height: 32,
                        stride: 256,
                        timestamp_us: 0,
                        payload_len: 10,
                        cursor: WireCursor {
                            x: 0,
                            y: 0,
                            visible: false,
                            shape: None,
                        },
                    };
                    ipc::write_msg_with_payload(&mut b, &hdr, &[0; 10]).unwrap();
                    std::thread::sleep(Duration::from_millis(100));
                });
                Ok(Box::new(a))
            }
        }
        fn wire(m: MonitorInfo) -> WireMonitor {
            super::super::agent::wire_monitor(&m)
        }
        let link = AgentLink::new();
        link.set(Some(Arc::new(Liar)));
        let mut cap = link.capturer_factory(None)(0, CaptureOptions::default()).unwrap();
        assert!(matches!(
            cap.next_frame(Duration::ZERO),
            Err(CaptureError::SurfaceLost)
        ));
    }
}
