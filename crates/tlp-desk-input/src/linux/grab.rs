//! Viewer keyboard grab on X11 (`XGrabKeyboard`).
//!
//! While active, every key (including the ones the window manager binds: Super, Alt+Tab, ...)
//! is delivered to this grab instead of any local window and forwarded as
//! [`InputEvent::Key`]. Ctrl+Alt+Del has no special meaning on X11 and is forwarded like any
//! other combination. Under Wayland there is no global grab; [`KeyboardGrab::start`] fails and
//! the app falls back to the keys the GUI toolkit delivers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::xkb::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{ConnectionExt as _, GrabMode, GrabStatus, Window};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::CURRENT_TIME;

use super::{conn_err, no_display_error, x11_display_available};
use crate::grab_logic::GrabLogic;
use crate::{InputError, InputEvent};

/// How long the grab thread sleeps between polls of the X connection and its command queue.
const POLL: Duration = Duration::from_millis(4);

enum Command {
    /// Grab on this window (0 = the root window).
    Activate(Window),
    Deactivate,
    Quit,
}

/// Captures the local keyboard for a viewer session. See the module docs.
///
/// Dropping the grab releases it (emitting [`InputEvent::ReleaseAll`] if it was active) and
/// joins its thread.
pub struct KeyboardGrab {
    cmd: Sender<Command>,
    active: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl KeyboardGrab {
    /// Connects to the X server (inactive grab). Returns the grab and the receiver of
    /// captured events.
    pub fn start() -> Result<(KeyboardGrab, Receiver<InputEvent>), InputError> {
        if !x11_display_available() || super::wayland_session() {
            return Err(no_display_error());
        }
        let (conn, screen) =
            RustConnection::connect(None).map_err(|e| conn_err("connect to the X server", e))?;
        let root = conn
            .setup()
            .roots
            .get(screen)
            .map(|s| s.root)
            .ok_or_else(|| InputError::Platform("X server reported no screen".into()))?;
        // Without detectable auto-repeat the server reports a key held down as alternating
        // release/press pairs, which would be forwarded as real releases. Best effort.
        enable_detectable_autorepeat(&conn);

        let (tx, rx) = mpsc::channel();
        let (cmd, cmd_rx) = mpsc::channel();
        let active = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new()
            .name("tlp-desk-keyboard-grab".into())
            .spawn({
                let active = Arc::clone(&active);
                move || grab_thread(conn, root, cmd_rx, tx, active)
            })
            .map_err(|e| InputError::Platform(format!("spawn keyboard grab thread: {e}")))?;
        Ok((
            KeyboardGrab {
                cmd,
                active,
                thread: Some(thread),
            },
            rx,
        ))
    }

    /// Turns the grab on or off. Turning it off emits [`InputEvent::ReleaseAll`]. The app only
    /// turns it on while the session window has focus.
    pub fn set_active(&self, active: bool) {
        let _ = self.cmd.send(if active {
            Command::Activate(0)
        } else {
            Command::Deactivate
        });
    }

    /// Like `set_active(true)` but grabs on the given X window id.
    pub fn set_active_for_window(&self, window: isize) {
        let id = u32::try_from(window).unwrap_or(0);
        let _ = self.cmd.send(Command::Activate(id));
    }

    /// Whether the grab currently holds the keyboard.
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
}

impl Drop for KeyboardGrab {
    fn drop(&mut self) {
        let _ = self.cmd.send(Command::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn enable_detectable_autorepeat(conn: &RustConnection) {
    let flag = xkb::PerClientFlag::DETECTABLE_AUTO_REPEAT;
    let supported = conn
        .xkb_use_extension(1, 0)
        .ok()
        .and_then(|c| c.reply().ok())
        .is_some_and(|r| r.supported);
    if supported {
        let _ = conn.xkb_per_client_flags(
            xkb::ID::USE_CORE_KBD.into(),
            flag,
            flag,
            xkb::BoolCtrl::from(0u8),
            xkb::BoolCtrl::from(0u8),
            xkb::BoolCtrl::from(0u8),
        );
        let _ = conn.flush();
    }
}

fn grab_thread(
    conn: RustConnection,
    root: Window,
    cmd: Receiver<Command>,
    tx: Sender<InputEvent>,
    active: Arc<AtomicBool>,
) {
    let mut logic = GrabLogic::default();
    loop {
        // Commands first so a deactivate is never starved by a burst of key events.
        while let Ok(c) = cmd.try_recv() {
            match c {
                Command::Activate(w) => {
                    let target = if w == 0 { root } else { w };
                    logic.reset();
                    let granted = conn
                        .grab_keyboard(
                            false,
                            target,
                            CURRENT_TIME,
                            GrabMode::ASYNC,
                            GrabMode::ASYNC,
                        )
                        .ok()
                        .and_then(|c| c.reply().ok())
                        .is_some_and(|r| r.status == GrabStatus::SUCCESS);
                    active.store(granted, Ordering::SeqCst);
                }
                Command::Deactivate | Command::Quit => {
                    if active.swap(false, Ordering::SeqCst) {
                        let _ = conn.ungrab_keyboard(CURRENT_TIME);
                        let _ = conn.flush();
                        logic.reset();
                        let _ = tx.send(InputEvent::ReleaseAll);
                    }
                    if matches!(c, Command::Quit) {
                        return;
                    }
                }
            }
        }
        match conn.poll_for_event() {
            Ok(Some(Event::KeyPress(e))) => forward(&mut logic, &tx, e.detail, true),
            Ok(Some(Event::KeyRelease(e))) => forward(&mut logic, &tx, e.detail, false),
            Ok(Some(_)) => {}
            Ok(None) => std::thread::sleep(POLL),
            // The connection died (X server gone): nothing more can be grabbed.
            Err(_) => {
                active.store(false, Ordering::SeqCst);
                return;
            }
        }
    }
}

fn forward(logic: &mut GrabLogic, tx: &Sender<InputEvent>, keycode: u8, down: bool) {
    let Some(key) = crate::evdev::x_keycode_to_scan(keycode) else {
        return;
    };
    let decision = logic.on_key(true, false, key.scancode, key.extended, down);
    for ev in decision.emit {
        let _ = tx.send(ev);
    }
}
