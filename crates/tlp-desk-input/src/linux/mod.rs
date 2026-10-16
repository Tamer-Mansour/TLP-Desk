//! Linux backends: X11 input injection through the XTest extension and the viewer keyboard
//! grab through `XGrabKeyboard`.
//!
//! Built from the X Window System protocol and XTest extension specifications only. Wayland
//! sessions do not allow a client to inject input or grab the keyboard globally; there the
//! constructors fail with an error that names the cause (see [`x11_display_available`]).
//! Injecting under Wayland needs the RemoteDesktop portal, which is not implemented yet
//! (see `docs/src/dev/linux.md`).

mod grab;

pub use grab::KeyboardGrab;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Window, BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, KEY_PRESS_EVENT, KEY_RELEASE_EVENT,
    MOTION_NOTIFY_EVENT,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use crate::keys::ScanKey;
use crate::mapping::MonitorRect;
use crate::tracker::{InputSink, RawInput, TrackingInjector};
use crate::{InputError, InputEvent, InputInjector};

/// Whether an X server can be reached (`DISPLAY` is set). Under a pure Wayland session
/// (`WAYLAND_DISPLAY` without `DISPLAY`) this is false and the Linux backends report why.
pub fn x11_display_available() -> bool {
    std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty())
}

/// The error returned when no X server is reachable, naming Wayland when that is the cause.
pub(crate) fn no_display_error() -> InputError {
    if std::env::var_os("WAYLAND_DISPLAY").is_some_and(|d| !d.is_empty()) {
        InputError::Platform(
            "this is a Wayland session without XWayland; input injection needs an X11 session \
             (the RemoteDesktop portal is not implemented yet)"
                .into(),
        )
    } else {
        InputError::Platform("DISPLAY is not set; no X server to inject input into".into())
    }
}

pub(crate) fn conn_err(context: &str, e: impl std::fmt::Display) -> InputError {
    InputError::Platform(format!("{context}: {e}"))
}

/// Clamps a root-window pixel to the `i16` the protocol carries.
pub(crate) fn clamp_i16(v: i32) -> i16 {
    v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

/// Upper bound of scroll clicks synthesized for one wheel event (a 16-bit delta is at most
/// 273 notches); keeps a hostile peer from making one event spin the host.
const MAX_CLICKS_PER_EVENT: u32 = 300;

/// XTest-backed sink.
struct XTestSink {
    conn: RustConnection,
    root: Window,
    /// Sub-notch wheel remainders (vertical, horizontal).
    carry: [i32; 2],
}

impl XTestSink {
    fn connect() -> Result<Self, InputError> {
        if !x11_display_available() {
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
        // Fails when the server lacks XTest (some hardened setups disable it).
        conn.xtest_get_version(2, 2)
            .map_err(|e| conn_err("query XTest", e))?
            .reply()
            .map_err(|e| conn_err("XTest extension is not available", e))?;
        Ok(Self {
            conn,
            root,
            carry: [0; 2],
        })
    }

    fn fake(&self, kind: u8, detail: u8, x: i16, y: i16) -> Result<(), InputError> {
        self.conn
            .xtest_fake_input(kind, detail, 0, self.root, x, y, 0)
            .map(drop)
            .map_err(|e| conn_err("XTestFakeInput", e))
    }

    fn click(&self, button: u8, count: u32) -> Result<(), InputError> {
        for _ in 0..count.min(MAX_CLICKS_PER_EVENT) {
            self.fake(BUTTON_PRESS_EVENT, button, 0, 0)?;
            self.fake(BUTTON_RELEASE_EVENT, button, 0, 0)?;
        }
        Ok(())
    }
}

impl InputSink for XTestSink {
    fn send(&mut self, batch: &[RawInput]) -> Result<(), InputError> {
        for raw in batch {
            match *raw {
                RawInput::MoveTo { x, y } => {
                    self.fake(MOTION_NOTIFY_EVENT, 0, clamp_i16(x), clamp_i16(y))?
                }
                RawInput::Button { button, down } => {
                    let kind = if down {
                        BUTTON_PRESS_EVENT
                    } else {
                        BUTTON_RELEASE_EVENT
                    };
                    self.fake(kind, crate::evdev::x_button(button), 0, 0)?;
                }
                RawInput::Wheel { horizontal, delta } => {
                    let carry = &mut self.carry[usize::from(horizontal)];
                    let (button, count) = crate::evdev::wheel_clicks(horizontal, delta, carry);
                    self.click(button, count)?;
                }
                RawInput::Key { key, down } => {
                    // Keys with no Linux equivalent are skipped, like Pause on Windows.
                    if let Some(code) = crate::evdev::scan_to_x_keycode(key) {
                        let kind = if down {
                            KEY_PRESS_EVENT
                        } else {
                            KEY_RELEASE_EVENT
                        };
                        self.fake(kind, code, 0, 0)?;
                    }
                }
            }
        }
        // One flush per batch so a press/release pair reaches the server together.
        self.conn
            .flush()
            .map_err(|e| conn_err("flush to the X server", e))
    }
}

/// Host-side injector for one captured monitor on X11.
///
/// Pointer events are mapped to `monitor` (root-window pixels, as reported by
/// `tlp_desk_capture::list_monitors`) and sent as absolute XTest motion. Keys go in by evdev
/// keycode, so the host's active XKB layout decides the character, exactly like the Windows
/// injector. Everything held is released on [`InputEvent::ReleaseAll`] and on drop.
pub struct LinuxInjector {
    inner: TrackingInjector<XTestSink>,
}

impl LinuxInjector {
    /// Connects to the X server named by `DISPLAY`.
    pub fn new(monitor: MonitorRect) -> Result<Self, InputError> {
        if monitor.width == 0 || monitor.height == 0 {
            return Err(InputError::Platform(format!(
                "empty target monitor {monitor:?}"
            )));
        }
        Ok(Self {
            inner: TrackingInjector::new(XTestSink::connect()?, monitor),
        })
    }

    /// The monitor pointer events are mapped to.
    pub fn monitor(&self) -> MonitorRect {
        self.inner.monitor()
    }
}

impl InputInjector for LinuxInjector {
    fn inject(&mut self, event: &InputEvent) -> Result<(), InputError> {
        self.inner.inject(event)
    }

    fn set_monitor(&mut self, monitor: MonitorRect) -> bool {
        if monitor.width == 0 || monitor.height == 0 {
            return false;
        }
        self.inner.set_monitor(monitor);
        true
    }
}

/// Bounding rectangle of the X screen (the virtual desktop), or `None` without a display.
pub fn virtual_desktop_rect() -> Option<MonitorRect> {
    if !x11_display_available() {
        return None;
    }
    let (conn, screen) = RustConnection::connect(None).ok()?;
    let s = conn.setup().roots.get(screen)?;
    let (w, h) = (u32::from(s.width_in_pixels), u32::from(s.height_in_pixels));
    (w > 0 && h > 0).then(|| MonitorRect::new(0, 0, w, h))
}

/// X11 needs no DPI opt-in: coordinates are always physical pixels.
pub fn ensure_dpi_aware() -> Result<(), InputError> {
    Ok(())
}

/// Translates a Windows virtual-key code to a wire scancode. Not needed on Linux: the viewer
/// GUI forwards hardware scancodes, so this reports "no mapping".
pub fn vk_to_scancode(_vk: u16) -> Option<ScanKey> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_i16_saturates() {
        assert_eq!(clamp_i16(0), 0);
        assert_eq!(clamp_i16(40_000), i16::MAX);
        assert_eq!(clamp_i16(-40_000), i16::MIN);
    }

    #[test]
    fn new_rejects_empty_monitor() {
        let err = LinuxInjector::new(MonitorRect::new(0, 0, 0, 1080)).err();
        assert!(matches!(err, Some(InputError::Platform(_))));
    }

    /// Needs an X server; never moves anything, only checks the connection and XTest.
    #[test]
    #[ignore = "needs an X server (run under Xvfb)"]
    fn connects_and_releases_without_events() {
        let mut inj = LinuxInjector::new(MonitorRect::new(0, 0, 640, 480)).expect("connect");
        inj.release_all().expect("release_all");
    }
}
