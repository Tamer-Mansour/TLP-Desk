//! Pure pointer helpers for the macOS injector: click counting and wheel conversion.
//!
//! macOS applications expect a `clickState` on every mouse event (1 = single, 2 = double,
//! 3 = triple click); `CGEventCreateMouseEvent` leaves it at 0, so double clicks would never
//! register. The injector therefore counts clicks itself. Kept free of Apple types so the logic is
//! unit-tested on every platform.

use std::time::{Duration, Instant};

use tlp_desk_proto::MouseButton;

/// Longest gap between two presses that still counts as a multi-click.
pub const MULTI_CLICK_WINDOW: Duration = Duration::from_millis(500);

/// Largest distance (points) between two presses of one multi-click.
pub const MULTI_CLICK_SLOP: i32 = 4;

/// Counts consecutive presses of the same button at nearly the same place.
#[derive(Debug, Default)]
pub struct ClickTracker {
    last: Option<(MouseButton, Instant, i32, i32)>,
    count: u32,
}

impl ClickTracker {
    /// Registers a button press and returns its click count (`1..=3`, then wraps to 1).
    pub fn press(&mut self, button: MouseButton, now: Instant, x: i32, y: i32) -> u32 {
        let repeat = match self.last {
            Some((b, t, lx, ly)) => {
                b == button
                    && now.saturating_duration_since(t) <= MULTI_CLICK_WINDOW
                    && (x - lx).abs() <= MULTI_CLICK_SLOP
                    && (y - ly).abs() <= MULTI_CLICK_SLOP
            }
            None => false,
        };
        self.count = if repeat && self.count < 3 {
            self.count + 1
        } else {
            1
        };
        self.last = Some((button, now, x, y));
        self.count
    }

    /// Forgets the click history (e.g. after [`crate::InputEvent::ReleaseAll`]).
    pub fn reset(&mut self) {
        self.last = None;
        self.count = 0;
    }
}

/// Converts a wire wheel delta (1/120 notch units) to scroll pixels: one notch scrolls 40 px.
/// Any non-zero delta yields at least one pixel so tiny smooth-scroll touchpad deltas are not lost.
pub fn wheel_pixels(delta: i16) -> i32 {
    let d = i32::from(delta);
    match d / 3 {
        0 if d != 0 => d.signum(),
        px => px,
    }
}

/// Mouse button number for `CGMouseButton` (0 left, 1 right, 2 other, 3 back, 4 forward).
pub fn button_number(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Right => 1,
        MouseButton::Middle => 2,
        MouseButton::Back => 3,
        MouseButton::Forward => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_and_triple_click_then_wrap() {
        let mut t = ClickTracker::default();
        let t0 = Instant::now();
        assert_eq!(t.press(MouseButton::Left, t0, 10, 10), 1);
        assert_eq!(
            t.press(MouseButton::Left, t0 + Duration::from_millis(200), 11, 9),
            2
        );
        assert_eq!(
            t.press(MouseButton::Left, t0 + Duration::from_millis(400), 11, 9),
            3
        );
        assert_eq!(
            t.press(MouseButton::Left, t0 + Duration::from_millis(600), 11, 9),
            1
        );
    }

    #[test]
    fn slow_far_or_other_button_resets() {
        let mut t = ClickTracker::default();
        let t0 = Instant::now();
        t.press(MouseButton::Left, t0, 0, 0);
        assert_eq!(
            t.press(MouseButton::Left, t0 + Duration::from_millis(900), 0, 0),
            1
        );
        assert_eq!(
            t.press(MouseButton::Left, t0 + Duration::from_millis(1000), 50, 0),
            1
        );
        assert_eq!(
            t.press(MouseButton::Right, t0 + Duration::from_millis(1100), 50, 0),
            1
        );
        t.reset();
        assert_eq!(t.press(MouseButton::Right, t0, 50, 0), 1);
    }

    #[test]
    fn wheel_notch_is_forty_pixels() {
        assert_eq!(wheel_pixels(120), 40);
        assert_eq!(wheel_pixels(-120), -40);
        assert_eq!(wheel_pixels(0), 0);
        assert_eq!(wheel_pixels(1), 1);
        assert_eq!(wheel_pixels(-2), -1);
        assert_eq!(wheel_pixels(i16::MIN), i32::from(i16::MIN) / 3);
    }

    #[test]
    fn button_numbers_match_cg_mouse_button() {
        assert_eq!(button_number(MouseButton::Left), 0);
        assert_eq!(button_number(MouseButton::Right), 1);
        assert_eq!(button_number(MouseButton::Middle), 2);
        assert_eq!(button_number(MouseButton::Forward), 4);
    }
}
