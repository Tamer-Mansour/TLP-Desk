//! Windows implementation: `SendInput` injection, DPI helpers and the viewer keyboard grab.
//!
//! Built only from Microsoft Learn documentation of `SendInput`, `MOUSEINPUT`, `KEYBDINPUT`,
//! `GetSystemMetrics`, `SetThreadDpiAwarenessContext`, `SetWindowsHookExW` and
//! `LowLevelKeyboardProc`.

mod activity;
mod grab;
mod marker;

pub use activity::LocalActivityMonitor;
pub use grab::KeyboardGrab;
pub use marker::RemotePointerMarker;

use windows::Win32::Foundation::GetLastError;
use windows::Win32::UI::HiDpi::{
    GetAwarenessFromDpiAwarenessContext, GetThreadDpiAwarenessContext,
    SetProcessDpiAwarenessContext, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, DPI_AWARENESS_PER_MONITOR_AWARE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
    KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
    MAPVK_VK_TO_VSC_EX, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL,
    MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    XBUTTON1, XBUTTON2,
};

use crate::keys::{normalize_scancode, ScanKey};
use crate::mapping::{pixel_to_virtual_absolute, MonitorRect};
use crate::tracker::{InputSink, RawInput, TrackingInjector};
use crate::{InputError, InputEvent, InputInjector, MouseButton};

/// Makes the process per-monitor DPI aware (v2), so every Win32 coordinate it sees is in
/// physical pixels.
///
/// The host process should call this once at startup, before creating any window (or declare
/// `PerMonitorV2` in its manifest). Returns `Ok` if the process is already per-monitor aware
/// (v1 or v2, e.g. set by the manifest or the GUI toolkit); returns an error if the process is
/// locked into a weaker mode. [`WindowsInjector`] works either way because it switches its own
/// thread to per-monitor-v2 for every call, but other host code (capture, monitor
/// enumeration) needs the process-wide setting to agree on pixel coordinates.
pub fn ensure_dpi_aware() -> Result<(), InputError> {
    // SAFETY: takes a predefined pseudo-handle constant; no pointers involved.
    if unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }.is_ok()
    {
        return Ok(());
    }
    // Already set (ERROR_ACCESS_DENIED). Accept any per-monitor mode.
    // SAFETY: both calls only read the calling thread's DPI context; no pointers involved.
    let awareness = unsafe { GetAwarenessFromDpiAwarenessContext(GetThreadDpiAwarenessContext()) };
    if awareness == DPI_AWARENESS_PER_MONITOR_AWARE {
        Ok(())
    } else {
        Err(InputError::Platform(format!(
            "process DPI awareness is {awareness:?}, not per-monitor; \
             set PerMonitorV2 in the manifest or call ensure_dpi_aware() first"
        )))
    }
}

/// Switches the calling thread to per-monitor-v2 DPI awareness until dropped.
struct ThreadDpiGuard(DPI_AWARENESS_CONTEXT);

impl ThreadDpiGuard {
    fn per_monitor_v2() -> Self {
        // SAFETY: takes a predefined pseudo-handle constant and returns the previous context
        // (null on failure, which we then never restore).
        Self(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}

impl Drop for ThreadDpiGuard {
    fn drop(&mut self) {
        if !self.0 .0.is_null() {
            // SAFETY: restores the context value this thread had before; it came from the OS.
            unsafe { SetThreadDpiAwarenessContext(self.0) };
        }
    }
}

/// Bounding rectangle of all monitors in physical pixels (`SM_*VIRTUALSCREEN`), or `None`
/// if the system reports an empty desktop.
pub fn virtual_desktop_rect() -> Option<MonitorRect> {
    let _dpi = ThreadDpiGuard::per_monitor_v2();
    virtual_desktop_rect_unguarded()
}

fn virtual_desktop_rect_unguarded() -> Option<MonitorRect> {
    // SAFETY: GetSystemMetrics takes an index and returns an integer; no pointers.
    let (x, y, w, h) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    (w > 0 && h > 0).then(|| MonitorRect::new(x, y, w as u32, h as u32))
}

/// Translates a Windows virtual-key code to its scancode in the current keyboard layout
/// (`MapVirtualKeyW(MAPVK_VK_TO_VSC_EX)`).
///
/// Only a fallback for keys the GUI receives as virtual keys while the [`KeyboardGrab`] is
/// not active; grabbed keys already carry their hardware scancode.
///
/// `MapVirtualKeyW` returns the numeric-keypad scancode for the dedicated navigation keys
/// (arrows, Home/End, PgUp/PgDn, Insert/Delete) without the `E0` prefix, so the extended
/// bit is forced for virtual keys that only exist on the extended cluster.
pub fn vk_to_scancode(vk: u16) -> Option<ScanKey> {
    // SAFETY: plain integer-in/integer-out call.
    let sc = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC_EX) };
    let key = u16::try_from(sc)
        .ok()
        .and_then(|sc| normalize_scancode(sc, false))?;
    Some(if vk_is_extended(vk) {
        ScanKey::new(key.scancode, true)
    } else {
        key
    })
}

/// Virtual keys that are always on the `E0` extended cluster of a 101/102-key keyboard.
fn vk_is_extended(vk: u16) -> bool {
    matches!(
        vk,
        0x21..=0x28 // PgUp, PgDn, End, Home, Left, Up, Right, Down
        | 0x2C // PrintScreen
        | 0x2D | 0x2E // Insert, Delete
        | 0x5B..=0x5D // LWin, RWin, Apps
        | 0x6F // Numpad divide
        | 0xA3 // RControl
        | 0xA5 // RMenu (AltGr)
    )
}

/// `SendInput`-backed sink.
struct SendInputSink;

impl SendInputSink {
    fn to_input(raw: &RawInput, desk: Option<MonitorRect>) -> Option<INPUT> {
        let mouse = |dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS| INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: data,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        Some(match *raw {
            RawInput::MoveTo { x, y } => {
                let (ax, ay) = pixel_to_virtual_absolute(x, y, desk?);
                mouse(
                    ax,
                    ay,
                    0,
                    MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                )
            }
            RawInput::Button { button, down } => {
                let (flags, data) = match (button, down) {
                    (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
                    (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
                    (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
                    (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
                    (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                    (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
                    (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, XBUTTON1),
                    (MouseButton::Back, false) => (MOUSEEVENTF_XUP, XBUTTON1),
                    (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, XBUTTON2),
                    (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, XBUTTON2),
                };
                mouse(0, 0, u32::from(data), flags)
            }
            // mouseData is a DWORD that Windows reads as signed for wheel events: positive is
            // wheel forward (scroll up) / tilt right.
            RawInput::Wheel { horizontal, delta } => mouse(
                0,
                0,
                delta as u32,
                if horizontal {
                    MOUSEEVENTF_HWHEEL
                } else {
                    MOUSEEVENTF_WHEEL
                },
            ),
            RawInput::Key { key, down } => {
                let mut flags: KEYBD_EVENT_FLAGS = KEYEVENTF_SCANCODE;
                if key.extended {
                    flags |= KEYEVENTF_EXTENDEDKEY;
                }
                if !down {
                    flags |= KEYEVENTF_KEYUP;
                }
                INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: VIRTUAL_KEY(0),
                            wScan: key.scancode,
                            dwFlags: flags,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                }
            }
        })
    }
}

impl InputSink for SendInputSink {
    fn send(&mut self, batch: &[RawInput]) -> Result<(), InputError> {
        // Absolute mouse coordinates and the virtual-screen metrics must be in the same
        // (physical) space regardless of how the process was configured.
        let _dpi = ThreadDpiGuard::per_monitor_v2();
        let needs_desk = batch.iter().any(|r| matches!(r, RawInput::MoveTo { .. }));
        // Queried per batch so monitor hot-plug / resolution changes are picked up.
        let desk =
            if needs_desk {
                Some(virtual_desktop_rect_unguarded().ok_or_else(|| {
                    InputError::Platform("virtual desktop has no monitors".into())
                })?)
            } else {
                None
            };
        let inputs: Vec<INPUT> = batch
            .iter()
            .filter_map(|r| Self::to_input(r, desk))
            .collect();
        if inputs.is_empty() {
            return Ok(());
        }
        // SAFETY: `inputs` is a valid slice of fully initialized INPUT structs and cbsize is
        // the size of one INPUT, as SendInput requires.
        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize == inputs.len() {
            Ok(())
        } else {
            // SAFETY: reads the calling thread's last-error value.
            let err = unsafe { GetLastError() };
            // UIPI blocks (elevated foreground window, secure desktop) are not reported
            // through GetLastError, so treat any shortfall as "blocked".
            Err(InputError::Blocked(format!(
                "SendInput inserted {sent} of {} events (last error {err:?}); \
                 the foreground desktop may be elevated or secure",
                inputs.len()
            )))
        }
    }
}

/// Host-side injector for one captured monitor.
///
/// Pointer events are mapped to `monitor` (physical virtual-desktop pixels, as reported by
/// the capture lane) and injected with `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`.
/// Keys are injected with `KEYEVENTF_SCANCODE`, so the host's active keyboard layout
/// (Arabic, English, ...) decides the resulting character. Everything held down is released
/// on [`InputEvent::ReleaseAll`] and when the injector is dropped.
///
/// Limitations: like any `SendInput` caller, it cannot reach elevated windows unless the
/// process is elevated (UIPI), nor the secure desktop (UAC, Ctrl+Alt+Del, lock screen); those
/// return [`InputError::Blocked`] and need the service agent (M3). Pause/Break (an `E1`
/// sequence) cannot be injected by scancode and is ignored.
pub struct WindowsInjector {
    inner: TrackingInjector<SendInputSink>,
}

impl WindowsInjector {
    pub fn new(monitor: MonitorRect) -> Result<Self, InputError> {
        if monitor.width == 0 || monitor.height == 0 {
            return Err(InputError::Platform(format!(
                "empty target monitor {monitor:?}"
            )));
        }
        Ok(Self {
            inner: TrackingInjector::new(SendInputSink, monitor),
        })
    }

    pub fn monitor(&self) -> MonitorRect {
        self.inner.monitor()
    }

    /// Retargets pointer events, e.g. after the viewer switched monitors or the resolution
    /// changed. Held keys/buttons are kept.
    pub fn set_monitor(&mut self, monitor: MonitorRect) {
        self.inner.set_monitor(monitor);
    }
}

impl InputInjector for WindowsInjector {
    fn inject(&mut self, event: &InputEvent) -> Result<(), InputError> {
        self.inner.inject(event)
    }

    fn set_monitor(&mut self, monitor: MonitorRect) -> bool {
        if monitor.width == 0 || monitor.height == 0 {
            return false;
        }
        WindowsInjector::set_monitor(self, monitor);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vk_to_scancode_layout_independent_keys() {
        // VK_ESCAPE, VK_TAB, VK_LEFT, VK_DELETE, VK_LWIN, VK_LCONTROL, VK_RCONTROL.
        assert_eq!(vk_to_scancode(0x1B), Some(ScanKey::ESCAPE));
        assert_eq!(vk_to_scancode(0x09), Some(ScanKey::TAB));
        assert_eq!(vk_to_scancode(0x25), Some(ScanKey::LEFT));
        assert_eq!(vk_to_scancode(0x2E), Some(ScanKey::DELETE));
        assert_eq!(vk_to_scancode(0x5B), Some(ScanKey::LEFT_WIN));
        assert_eq!(vk_to_scancode(0xA2), Some(ScanKey::LEFT_CTRL));
        assert_eq!(vk_to_scancode(0xA3), Some(ScanKey::RIGHT_CTRL));
        // VK_UP, VK_HOME, VK_RMENU, VK_SPACE, VK_RETURN.
        assert_eq!(vk_to_scancode(0x26), Some(ScanKey::UP));
        assert_eq!(vk_to_scancode(0x24), Some(ScanKey::new(0x47, true)));
        assert_eq!(vk_to_scancode(0xA5), Some(ScanKey::RIGHT_ALT));
        assert_eq!(vk_to_scancode(0x20), Some(ScanKey::SPACE));
        assert_eq!(vk_to_scancode(0x0D), Some(ScanKey::ENTER));
        assert_eq!(vk_to_scancode(0), None);
    }

    #[test]
    fn virtual_desktop_is_reported() {
        let d = virtual_desktop_rect().expect("at least one monitor");
        assert!(d.width > 0 && d.height > 0);
    }

    #[test]
    fn ensure_dpi_aware_succeeds_in_test_process() {
        ensure_dpi_aware().unwrap();
        // Idempotent.
        ensure_dpi_aware().unwrap();
    }

    #[test]
    fn input_struct_conversion() {
        let desk = MonitorRect::new(-1920, 0, 3840, 1080);
        let i = SendInputSink::to_input(&RawInput::MoveTo { x: -1920, y: 0 }, Some(desk)).unwrap();
        assert_eq!(i.r#type, INPUT_MOUSE);
        // SAFETY: we just built this INPUT as a mouse input.
        let mi = unsafe { i.Anonymous.mi };
        assert_eq!((mi.dx, mi.dy), (0, 0));
        assert_eq!(
            mi.dwFlags,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK
        );
        assert!(SendInputSink::to_input(&RawInput::MoveTo { x: 0, y: 0 }, None).is_none());

        let i = SendInputSink::to_input(
            &RawInput::Wheel {
                horizontal: false,
                delta: -120,
            },
            None,
        )
        .unwrap();
        // SAFETY: mouse input built above.
        let mi = unsafe { i.Anonymous.mi };
        assert_eq!(mi.mouseData as i32, -120);
        assert_eq!(mi.dwFlags, MOUSEEVENTF_WHEEL);

        let i = SendInputSink::to_input(
            &RawInput::Button {
                button: MouseButton::Forward,
                down: false,
            },
            None,
        )
        .unwrap();
        // SAFETY: mouse input built above.
        let mi = unsafe { i.Anonymous.mi };
        assert_eq!((mi.dwFlags, mi.mouseData), (MOUSEEVENTF_XUP, 2));

        let i = SendInputSink::to_input(
            &RawInput::Key {
                key: ScanKey::RIGHT_ALT,
                down: false,
            },
            None,
        )
        .unwrap();
        assert_eq!(i.r#type, INPUT_KEYBOARD);
        // SAFETY: keyboard input built above.
        let ki = unsafe { i.Anonymous.ki };
        assert_eq!(ki.wScan, 0x38);
        assert_eq!(ki.wVk, VIRTUAL_KEY(0));
        assert_eq!(
            ki.dwFlags,
            KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP
        );
    }

    #[test]
    fn empty_monitor_is_rejected() {
        assert!(WindowsInjector::new(MonitorRect::new(0, 0, 0, 1080)).is_err());
    }
}
