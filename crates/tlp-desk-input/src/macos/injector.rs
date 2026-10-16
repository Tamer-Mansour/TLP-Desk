//! `CGEvent` injection: the macOS [`InputSink`] behind [`MacInjector`].
//!
//! Built only from Apple's documentation of Quartz Event Services (`CGEventCreateMouseEvent`,
//! `CGEventCreateKeyboardEvent`, `CGEventCreateScrollWheelEvent2`, `CGEventPost`,
//! `CGEventSetFlags`). Posting events needs the Accessibility permission; without it macOS drops
//! the events silently, so [`MacInjector::new`] checks it up front and reports
//! [`InputError::Blocked`] (see `permissions`).

use std::time::Instant;

use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
    CGEventType, CGMouseButton, CGScrollEventUnit,
};

use super::keymap::{mac_keycode, modifier_flag};
use super::pointer::{button_number, wheel_pixels, ClickTracker};
use crate::mapping::MonitorRect;
use crate::permissions::{self, PermissionState};
use crate::tracker::{InputSink, RawInput, TrackingInjector};
use crate::{InputError, InputEvent, InputInjector, MouseButton};

/// Posts low-level actions to the HID event tap.
struct CgEventSink {
    source: Option<objc2_core_foundation::CFRetained<CGEventSource>>,
    /// Modifier bits currently held; stamped on every event so shortcuts work.
    flags: u64,
    /// Last pointer position in global display points.
    pos: (i32, i32),
    /// Buttons held, to turn moves into drags.
    held: [bool; 5],
    clicks: ClickTracker,
}

// SAFETY: the sink is only ever used from the one host input thread that owns the injector
// (`InputInjector: Send` moves it there once). CGEventSource is a plain CoreFoundation object
// with no thread affinity; the crate marks it `!Send` only conservatively.
unsafe impl Send for CgEventSink {}

fn point(x: i32, y: i32) -> CGPoint {
    CGPoint::new(f64::from(x), f64::from(y))
}

impl CgEventSink {
    fn new() -> Self {
        Self {
            source: CGEventSource::new(CGEventSourceStateID::HIDSystemState),
            flags: 0,
            pos: (0, 0),
            held: [false; 5],
            clicks: ClickTracker::default(),
        }
    }

    fn post(
        &self,
        event: Option<objc2_core_foundation::CFRetained<CGEvent>>,
    ) -> Result<(), InputError> {
        let event = event.ok_or_else(|| InputError::Platform("CGEvent creation failed".into()))?;
        CGEvent::set_flags(Some(&event), CGEventFlags(self.flags));
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        Ok(())
    }

    fn mouse_type(&self, button: MouseButton, down: bool) -> CGEventType {
        match (button, down) {
            (MouseButton::Left, true) => CGEventType::LeftMouseDown,
            (MouseButton::Left, false) => CGEventType::LeftMouseUp,
            (MouseButton::Right, true) => CGEventType::RightMouseDown,
            (MouseButton::Right, false) => CGEventType::RightMouseUp,
            (_, true) => CGEventType::OtherMouseDown,
            (_, false) => CGEventType::OtherMouseUp,
        }
    }

    fn move_to(&mut self, x: i32, y: i32) -> Result<(), InputError> {
        self.pos = (x, y);
        // A held button turns a move into a drag, which is what text selection and window
        // dragging listen for.
        let (ty, button) = if self.held[0] {
            (CGEventType::LeftMouseDragged, CGMouseButton::Left)
        } else if self.held[1] {
            (CGEventType::RightMouseDragged, CGMouseButton::Right)
        } else if self.held[2] || self.held[3] || self.held[4] {
            (CGEventType::OtherMouseDragged, CGMouseButton(2))
        } else {
            (CGEventType::MouseMoved, CGMouseButton::Left)
        };
        self.post(CGEvent::new_mouse_event(
            self.source.as_deref(),
            ty,
            point(x, y),
            button,
        ))
    }

    fn button(&mut self, button: MouseButton, down: bool) -> Result<(), InputError> {
        let (x, y) = self.pos;
        let idx = button_number(button) as usize;
        let ty = self.mouse_type(button, down);
        let event = CGEvent::new_mouse_event(
            self.source.as_deref(),
            ty,
            point(x, y),
            CGMouseButton(button_number(button)),
        );
        if down {
            let count = self.clicks.press(button, Instant::now(), x, y);
            if let Some(e) = event.as_deref() {
                CGEvent::set_integer_value_field(
                    Some(e),
                    CGEventField::MouseEventClickState,
                    i64::from(count),
                );
            }
        }
        if let Some(h) = self.held.get_mut(idx) {
            *h = down;
        }
        self.post(event)
    }

    fn wheel(&mut self, horizontal: bool, delta: i32) -> Result<(), InputError> {
        let px = wheel_pixels(delta.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16);
        // Windows: positive vertical = away from the user (scroll up), positive horizontal =
        // right. CGEvent wheel1 positive = up; wheel2 positive = left, hence the negation.
        let (w1, w2) = if horizontal { (0, -px) } else { (px, 0) };
        self.post(CGEvent::new_scroll_wheel_event2(
            self.source.as_deref(),
            CGScrollEventUnit::Pixel,
            2,
            w1,
            w2,
            0,
        ))
    }

    fn key(&mut self, scan: crate::keys::ScanKey, down: bool) -> Result<(), InputError> {
        let Some(code) = mac_keycode(scan) else {
            // Keys macOS does not have (Pause, Print Screen): ignore like the Windows side
            // ignores Pause.
            return Ok(());
        };
        if let Some(bit) = modifier_flag(code) {
            if down {
                self.flags |= bit;
            } else {
                self.flags &= !bit;
            }
        }
        self.post(CGEvent::new_keyboard_event(
            self.source.as_deref(),
            code,
            down,
        ))
    }
}

impl InputSink for CgEventSink {
    fn send(&mut self, batch: &[RawInput]) -> Result<(), InputError> {
        for action in batch {
            match *action {
                RawInput::MoveTo { x, y } => self.move_to(x, y)?,
                RawInput::Button { button, down } => self.button(button, down)?,
                RawInput::Wheel { horizontal, delta } => self.wheel(horizontal, delta)?,
                RawInput::Key { key, down } => self.key(key, down)?,
            }
        }
        Ok(())
    }
}

/// Host-side injector for one captured display on macOS.
///
/// `monitor` is in global display **points** (what `CGDisplayBounds` reports; the capture lane
/// reports `MonitorInfo` in points for this reason), so it works unchanged on Retina displays.
/// Everything held down is released on [`InputEvent::ReleaseAll`].
pub struct MacInjector {
    inner: TrackingInjector<CgEventSink>,
}

impl MacInjector {
    /// Creates an injector, failing with [`InputError::Blocked`] when the Accessibility
    /// permission has not been granted (macOS would otherwise drop every event silently).
    pub fn new(monitor: MonitorRect) -> Result<Self, InputError> {
        if monitor.width == 0 || monitor.height == 0 {
            return Err(InputError::Platform(format!(
                "empty target monitor {monitor:?}"
            )));
        }
        if permissions::accessibility() == PermissionState::Denied {
            return Err(InputError::Blocked(
                "Accessibility permission is required (System Settings > Privacy & Security > \
                 Accessibility)"
                    .into(),
            ));
        }
        Ok(Self {
            inner: TrackingInjector::new(CgEventSink::new(), monitor),
        })
    }
}

impl InputInjector for MacInjector {
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
