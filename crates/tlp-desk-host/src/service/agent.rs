//! The session agent (`TLP-Desk.exe --agent`): serves capture, input, commands and desktop
//! events to the service over its pipe.
//!
//! The agent runs as SYSTEM in the console session. Every service connection is handled on
//! its own thread by [`serve_connection`], which is platform-independent: the OS lives behind
//! [`AgentPlatform`] (Windows: capture crate, `SendInput`, `OpenInputDesktop`,
//! `LockWorkStation`), so the protocol is tested end to end with fakes over an in-memory pipe.
//!
//! Desktop switches: a capture or input connection keeps its thread attached to the input
//! desktop with a [`DesktopFollower`]. When the input desktop changes (lock screen, UAC), a
//! capture connection answers `SurfaceLost` and ends; the service's pipeline then opens a new
//! capture connection, whose fresh thread attaches to the new desktop and opens a capturer
//! there (DXGI duplication on the secure desktop). Input connections re-create their injector
//! in place.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tlp_desk_capture::{CaptureError, CursorState, MonitorInfo, ScreenCapturer};
use tlp_desk_input::{InputError, InputEvent, InputInjector, MonitorRect};

use super::desktop::{DesktopFollower, DesktopSource, DesktopWatcher};
use super::ipc::{
    self, AgentChannel, AgentCommand, AgentEvent, AgentHello, AgentWelcome, CaptureReply,
    CaptureRequest, CommandReply, InputCommand, WireCursor, WireCursorShape, WireError,
    WireMonitor, WireRect, IPC_VERSION,
};
use crate::HostError;

/// How often capture/input threads re-check the input desktop.
pub const DESKTOP_CHECK_EVERY: Duration = Duration::from_millis(250);

/// How often the events connection polls the input desktop.
pub const DESKTOP_POLL_EVERY: Duration = Duration::from_millis(250);

/// Longest `NextFrame` wait the agent honours (a hostile timeout cannot park the thread).
pub const MAX_FRAME_WAIT: Duration = Duration::from_millis(1000);

/// What the agent needs from the OS.
pub trait AgentPlatform: Send + Sync + 'static {
    /// The Windows session the agent runs in.
    fn session_id(&self) -> u32;
    /// Opens a capturer on the calling thread's desktop; `secure_desktop` asks for a backend
    /// that works there (DXGI duplication). `max_size` caps the frame size.
    fn open_capturer(
        &self,
        monitor: u32,
        max_size: Option<(u32, u32)>,
        secure_desktop: bool,
    ) -> Result<Box<dyn ScreenCapturer>, CaptureError>;
    fn list_monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError>;
    /// Opens an injector on the calling thread's desktop.
    fn open_injector(&self, rect: MonitorRect) -> Result<Box<dyn InputInjector>, InputError>;
    /// Locks the session (`LockWorkStation`).
    fn lock_workstation(&self) -> Result<(), HostError>;
    /// A fresh desktop source for one thread.
    fn desktop_source(&self) -> Box<dyn DesktopSource>;
}

impl DesktopSource for Box<dyn DesktopSource> {
    fn input_desktop(&mut self) -> Result<String, HostError> {
        (**self).input_desktop()
    }
    fn attach_thread(&mut self) -> Result<String, HostError> {
        (**self).attach_thread()
    }
}

/// Serves one service connection until it closes, the agent is told to exit, or `stop` is
/// set. Errors are I/O errors of this connection only.
pub fn serve_connection<C: Read + Write + ?Sized>(
    conn: &mut C,
    platform: &dyn AgentPlatform,
    stop: &AtomicBool,
) -> Result<(), HostError> {
    let hello: AgentHello = ipc::read_msg(conn)?;
    if hello.version != IPC_VERSION {
        ipc::write_msg(
            conn,
            &AgentWelcome::Refused {
                version: IPC_VERSION,
                error: WireError::VersionMismatch {
                    expected: IPC_VERSION,
                    got: hello.version,
                },
            },
        )?;
        return Err(HostError::IpcVersion {
            expected: IPC_VERSION,
            got: hello.version,
        });
    }
    match hello.channel {
        AgentChannel::Capture {
            monitor,
            max_width,
            max_height,
        } => {
            let max = (max_width > 0 && max_height > 0).then_some((max_width, max_height));
            serve_capture(conn, platform, monitor, max)
        }
        AgentChannel::Input { rect } => serve_input(conn, platform, rect),
        AgentChannel::Command => serve_commands(conn, platform, stop),
        AgentChannel::Events => serve_events(conn, platform, stop),
    }
}

fn welcome_ok<C: Write + ?Sized>(
    conn: &mut C,
    platform: &dyn AgentPlatform,
) -> Result<(), HostError> {
    ipc::write_msg(
        conn,
        &AgentWelcome::Ok {
            version: IPC_VERSION,
            session_id: platform.session_id(),
        },
    )
}

fn refuse<C: Write + ?Sized>(conn: &mut C, error: WireError) -> Result<(), HostError> {
    ipc::write_msg(
        conn,
        &AgentWelcome::Refused {
            version: IPC_VERSION,
            error,
        },
    )
}

fn serve_capture<C: Read + Write + ?Sized>(
    conn: &mut C,
    platform: &dyn AgentPlatform,
    monitor: u32,
    max: Option<(u32, u32)>,
) -> Result<(), HostError> {
    let mut follower = DesktopFollower::new(platform.desktop_source(), DESKTOP_CHECK_EVERY);
    if let Err(e) = follower.check(Instant::now()) {
        tracing::warn!(%e, "capture thread cannot attach to the input desktop");
        return refuse(conn, WireError::Other(e.to_string()));
    }
    let secure = follower.on_secure_desktop();
    let mut cap = match platform.open_capturer(monitor, max, secure) {
        Ok(c) => c,
        Err(e) => return refuse(conn, capture_error(&e)),
    };
    tracing::info!(
        monitor,
        desktop = follower.attached().unwrap_or_default(),
        backend = ?cap.backend(),
        "agent capture opened"
    );
    welcome_ok(conn, platform)?;
    let (fw, fh) = cap.frame_size();
    ipc::write_msg(
        conn,
        &CaptureReply::Opened {
            monitor: wire_monitor(cap.monitor()),
            frame_width: fw,
            frame_height: fh,
        },
    )?;
    loop {
        let req: CaptureRequest = match ipc::read_msg(conn) {
            Ok(r) => r,
            Err(HostError::IpcClosed) => return Ok(()),
            Err(e) => return Err(e),
        };
        let CaptureRequest::NextFrame { timeout_ms } = req;
        // A desktop switch invalidates this capturer: tell the service, which reopens on a
        // new connection (and thread) attached to the new desktop.
        match follower.check(Instant::now()) {
            Ok(None) => {}
            Ok(Some(name)) => {
                tracing::info!(desktop = %name, "input desktop changed; capture surface lost");
                return ipc::write_msg(conn, &CaptureReply::Error(WireError::SurfaceLost));
            }
            Err(e) => {
                tracing::debug!(%e, "re-attaching the capture thread failed");
                return ipc::write_msg(conn, &CaptureReply::Error(WireError::SurfaceLost));
            }
        }
        let wait = Duration::from_millis(u64::from(timeout_ms)).min(MAX_FRAME_WAIT);
        let frame = cap.next_frame(wait);
        let cursor = match cap.cursor() {
            Ok(c) => wire_cursor(c),
            Err(_) => WireCursor {
                x: 0,
                y: 0,
                visible: false,
                shape: None,
            },
        };
        match frame {
            Ok(Some(f)) => {
                let hdr = CaptureReply::Frame {
                    width: f.width,
                    height: f.height,
                    stride: f.stride,
                    timestamp_us: f.timestamp_us,
                    payload_len: f.data.len() as u64,
                    cursor,
                };
                ipc::write_msg_with_payload(conn, &hdr, &f.data)?;
                cap.recycle(f);
            }
            Ok(None) => ipc::write_msg(conn, &CaptureReply::NoChange { cursor })?,
            Err(CaptureError::SurfaceLost) => {
                return ipc::write_msg(conn, &CaptureReply::Error(WireError::SurfaceLost));
            }
            Err(e) => ipc::write_msg(conn, &CaptureReply::Error(capture_error(&e)))?,
        }
    }
}

fn serve_input<C: Read + Write + ?Sized>(
    conn: &mut C,
    platform: &dyn AgentPlatform,
    rect: WireRect,
) -> Result<(), HostError> {
    let mut follower = DesktopFollower::new(platform.desktop_source(), DESKTOP_CHECK_EVERY);
    if let Err(e) = follower.check(Instant::now()) {
        tracing::warn!(%e, "input thread cannot attach to the input desktop");
        return refuse(conn, WireError::Other(e.to_string()));
    }
    let mut rect = monitor_rect(rect);
    let mut inj = match platform.open_injector(rect) {
        Ok(i) => Some(i),
        Err(e) => return refuse(conn, input_error(&e)),
    };
    welcome_ok(conn, platform)?;
    loop {
        let cmd: InputCommand = match ipc::read_msg(conn) {
            Ok(c) => c,
            Err(e) => {
                // Service gone or session over: never leave keys or buttons pressed.
                if let Some(i) = inj.as_mut() {
                    let _ = i.release_all();
                }
                return match e {
                    HostError::IpcClosed => Ok(()),
                    e => Err(e),
                };
            }
        };
        match follower.check(Instant::now()) {
            Ok(Some(name)) => {
                tracing::info!(desktop = %name, "input desktop changed; re-creating the injector");
                // The old injector belongs to the old desktop: drop it before opening anew.
                drop(inj.take());
                inj = open_injector_logged(platform, rect);
            }
            Ok(None) => {}
            Err(e) => tracing::debug!(%e, "re-attaching the input thread failed"),
        }
        match cmd {
            InputCommand::Event(ev) => {
                if inj.is_none() {
                    inj = open_injector_logged(platform, rect);
                }
                if let Some(i) = inj.as_mut() {
                    if let Err(e) = i.inject(&ev) {
                        tracing::debug!(%e, "agent input injection failed");
                        if matches!(ev, InputEvent::ReleaseAll) {
                            continue;
                        }
                    }
                }
            }
            InputCommand::SetMonitor(r) => {
                rect = monitor_rect(r);
                let retargeted = inj.as_mut().is_some_and(|i| i.set_monitor(rect));
                if !retargeted {
                    if let Some(mut old) = inj.take() {
                        let _ = old.release_all();
                    }
                    inj = open_injector_logged(platform, rect);
                }
            }
        }
    }
}

fn open_injector_logged(
    platform: &dyn AgentPlatform,
    rect: MonitorRect,
) -> Option<Box<dyn InputInjector>> {
    match platform.open_injector(rect) {
        Ok(i) => Some(i),
        Err(e) => {
            tracing::warn!(%e, "agent cannot create the input injector");
            None
        }
    }
}

fn serve_commands<C: Read + Write + ?Sized>(
    conn: &mut C,
    platform: &dyn AgentPlatform,
    stop: &AtomicBool,
) -> Result<(), HostError> {
    welcome_ok(conn, platform)?;
    loop {
        let cmd: AgentCommand = match ipc::read_msg(conn) {
            Ok(c) => c,
            Err(HostError::IpcClosed) => return Ok(()),
            Err(e) => return Err(e),
        };
        let reply = match cmd {
            AgentCommand::ListMonitors => match platform.list_monitors() {
                Ok(list) => CommandReply::Monitors(list.iter().map(wire_monitor).collect()),
                Err(e) => CommandReply::Error(capture_error(&e)),
            },
            AgentCommand::LockWorkstation => match platform.lock_workstation() {
                Ok(()) => CommandReply::Ok,
                Err(e) => CommandReply::Error(WireError::Other(e.to_string())),
            },
            AgentCommand::Exit => {
                tracing::info!("service asked the agent to exit");
                stop.store(true, Ordering::Release);
                ipc::write_msg(conn, &CommandReply::Ok)?;
                return Ok(());
            }
        };
        ipc::write_msg(conn, &reply)?;
    }
}

fn serve_events<C: Read + Write + ?Sized>(
    conn: &mut C,
    platform: &dyn AgentPlatform,
    stop: &AtomicBool,
) -> Result<(), HostError> {
    welcome_ok(conn, platform)?;
    let mut watcher = DesktopWatcher::new(platform.desktop_source());
    while !stop.load(Ordering::Acquire) {
        if let Some(change) = watcher.poll() {
            tracing::info!(desktop = %change.name, secure = change.secure, "input desktop changed");
            ipc::write_msg(
                conn,
                &AgentEvent::Desktop {
                    name: change.name,
                    secure: change.secure,
                },
            )?;
        }
        std::thread::sleep(DESKTOP_POLL_EVERY);
    }
    Ok(())
}

pub(crate) fn wire_monitor(m: &MonitorInfo) -> WireMonitor {
    WireMonitor {
        index: m.index,
        name: m.name.clone(),
        width: m.width,
        height: m.height,
        primary: m.primary,
        x: m.x,
        y: m.y,
    }
}

pub(crate) fn monitor_rect(r: WireRect) -> MonitorRect {
    MonitorRect::new(r.x, r.y, r.width, r.height)
}

fn wire_cursor(c: CursorState) -> WireCursor {
    WireCursor {
        x: c.x,
        y: c.y,
        visible: c.visible,
        shape: c.shape.map(|s| WireCursorShape {
            width: s.width,
            height: s.height,
            hotspot_x: s.hotspot_x,
            hotspot_y: s.hotspot_y,
            rgba: s.rgba,
        }),
    }
}

pub(crate) fn capture_error(e: &CaptureError) -> WireError {
    match e {
        CaptureError::SurfaceLost => WireError::SurfaceLost,
        CaptureError::NoSuchMonitor(m) => WireError::NoSuchMonitor(*m),
        CaptureError::Unsupported => WireError::Unsupported,
        CaptureError::Platform(s) => WireError::Other(s.clone()),
    }
}

fn input_error(e: &InputError) -> WireError {
    match e {
        InputError::Unsupported => WireError::Unsupported,
        InputError::Blocked(s) => WireError::Blocked(s.clone()),
        InputError::Platform(s) => WireError::Other(s.clone()),
    }
}
