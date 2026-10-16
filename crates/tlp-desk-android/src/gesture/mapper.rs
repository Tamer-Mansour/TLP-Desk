//! Turns recognized [`Action`]s into wire input and changes of the local view.

use tlp_desk_input::ViewRect;
use tlp_desk_proto::{InputEvent, MouseButton};

use super::Action;
use crate::view::{Point, ViewTransform};

/// Finger travel (screen units) that equals one wheel notch.
pub const PX_PER_NOTCH: f32 = 40.0;

/// Wheel units per notch on the wire (`WHEEL_DELTA`).
const WHEEL_DELTA: f32 = 120.0;

/// Maps gestures to input for one session. Owns the zoom/pan state.
#[derive(Debug)]
pub struct InputMapper {
    /// Current zoom and pan of the picture.
    pub view: ViewTransform,
    /// Where the picture area is on screen.
    pub viewport: ViewRect,
    /// Size of the decoded remote frame; `(0, 0)` until the first frame.
    pub image: (u32, u32),
    scroll_x: f32,
    scroll_y: f32,
    scroll_anchored: bool,
}

impl InputMapper {
    /// Creates a mapper for a viewport.
    pub fn new(viewport: ViewRect) -> Self {
        Self {
            view: ViewTransform::default(),
            viewport,
            image: (0, 0),
            scroll_x: 0.0,
            scroll_y: 0.0,
            scroll_anchored: false,
        }
    }

    /// Updates the viewport / frame size (rotation, new monitor) and keeps the view legal.
    pub fn set_geometry(&mut self, viewport: ViewRect, image: (u32, u32)) {
        self.viewport = viewport;
        self.image = image;
        self.view.clamp(viewport, image);
    }

    fn at(&self, p: Point) -> InputEvent {
        let (x, y) = self.view.to_normalized(p, self.viewport, self.image);
        InputEvent::MouseMove { x, y }
    }

    fn ready(&self) -> bool {
        self.image.0 > 0 && self.image.1 > 0
    }

    /// Input events for one action (empty for view-only actions or before the first frame).
    pub fn apply(&mut self, action: Action) -> Vec<InputEvent> {
        if !self.ready() {
            return Vec::new();
        }
        let button = |button, down| InputEvent::MouseButton { button, down };
        match action {
            Action::Click { pos, button: b } => {
                if !self.view.hits_image(pos, self.viewport, self.image) {
                    return Vec::new();
                }
                vec![self.at(pos), button(b, true), button(b, false)]
            }
            Action::DragBegin(p) => vec![self.at(p), button(MouseButton::Left, true)],
            Action::DragMove(p) => vec![self.at(p)],
            Action::DragEnd(p) => vec![self.at(p), button(MouseButton::Left, false)],
            Action::Scroll { dx, dy, at } => {
                let mut out = Vec::new();
                if !self.scroll_anchored {
                    // The wheel acts on the window under the pointer.
                    out.push(self.at(at));
                    self.scroll_anchored = true;
                }
                // Natural scrolling: fingers moving up scroll the page down (negative wheel);
                // fingers moving left scroll right (positive horizontal wheel).
                self.scroll_y += dy * WHEEL_DELTA / PX_PER_NOTCH;
                self.scroll_x -= dx * WHEEL_DELTA / PX_PER_NOTCH;
                let (wx, wy) = (
                    take_whole(&mut self.scroll_x),
                    take_whole(&mut self.scroll_y),
                );
                if wx != 0 || wy != 0 {
                    out.push(InputEvent::MouseWheel { dx: wx, dy: wy });
                }
                out
            }
            Action::Zoom { factor, focal } => {
                self.view
                    .zoom_about(factor, focal, self.viewport, self.image);
                Vec::new()
            }
            Action::Pan { dx, dy } => {
                self.view.pan_by(dx, dy, self.viewport, self.image);
                Vec::new()
            }
        }
    }

    /// Call when all fingers are up so the next scroll re-anchors the pointer.
    pub fn gesture_finished(&mut self) {
        self.scroll_anchored = false;
        self.scroll_x = 0.0;
        self.scroll_y = 0.0;
    }
}

/// Removes and returns the whole wheel units of `acc`, keeping the fraction.
fn take_whole(acc: &mut f32) -> i16 {
    let whole = acc.trunc().clamp(f32::from(i16::MIN), f32::from(i16::MAX));
    *acc -= whole;
    whole as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapper() -> InputMapper {
        let mut m = InputMapper::new(ViewRect::new(0.0, 0.0, 400.0, 800.0));
        m.set_geometry(ViewRect::new(0.0, 0.0, 400.0, 800.0), (1920, 1080));
        m
    }

    #[test]
    fn nothing_is_sent_before_the_first_frame() {
        let mut m = InputMapper::new(ViewRect::new(0.0, 0.0, 400.0, 800.0));
        let click = Action::Click {
            pos: Point::new(10.0, 10.0),
            button: MouseButton::Left,
        };
        assert!(m.apply(click).is_empty());
    }

    #[test]
    fn click_is_move_press_release_at_the_mapped_point() {
        let mut m = mapper();
        let out = m.apply(Action::Click {
            pos: Point::new(200.0, 400.0),
            button: MouseButton::Right,
        });
        assert_eq!(out.len(), 3);
        let InputEvent::MouseMove { x, y } = out[0] else {
            panic!("first event must be a move: {out:?}");
        };
        // Centre of the picture is the centre of the wire range.
        assert!((i32::from(x) - 32768).abs() < 200 && (i32::from(y) - 32768).abs() < 200);
        assert_eq!(
            out[1],
            InputEvent::MouseButton {
                button: MouseButton::Right,
                down: true
            }
        );
        assert_eq!(
            out[2],
            InputEvent::MouseButton {
                button: MouseButton::Right,
                down: false
            }
        );
    }

    #[test]
    fn tapping_the_letterbox_bars_does_nothing() {
        let mut m = mapper();
        // The picture is 225 high, centred: y 287..512. y = 20 is a black bar.
        let out = m.apply(Action::Click {
            pos: Point::new(200.0, 20.0),
            button: MouseButton::Left,
        });
        assert!(out.is_empty());
    }

    #[test]
    fn drag_presses_moves_and_releases() {
        let mut m = mapper();
        let begin = m.apply(Action::DragBegin(Point::new(100.0, 400.0)));
        assert!(matches!(
            begin[1],
            InputEvent::MouseButton { down: true, .. }
        ));
        assert_eq!(m.apply(Action::DragMove(Point::new(300.0, 400.0))).len(), 1);
        let end = m.apply(Action::DragEnd(Point::new(300.0, 400.0)));
        assert!(matches!(
            end[1],
            InputEvent::MouseButton { down: false, .. }
        ));
    }

    #[test]
    fn fingers_moving_up_scroll_down_and_fractions_accumulate() {
        let mut m = mapper();
        let at = Point::new(200.0, 400.0);
        // 10 px is a quarter notch = 30 units.
        let out = m.apply(Action::Scroll {
            dx: 0.0,
            dy: -10.0,
            at,
        });
        assert!(matches!(out[0], InputEvent::MouseMove { .. }));
        assert_eq!(out[1], InputEvent::MouseWheel { dx: 0, dy: -30 });
        // The pointer is only anchored once per gesture.
        let out = m.apply(Action::Scroll {
            dx: 0.0,
            dy: -0.1,
            at,
        });
        assert!(out.is_empty(), "0.3 unit is below one wheel unit: {out:?}");
        m.gesture_finished();
        let out = m.apply(Action::Scroll {
            dx: -4.0,
            dy: 0.0,
            at,
        });
        assert!(matches!(out[0], InputEvent::MouseMove { .. }));
        assert_eq!(out[1], InputEvent::MouseWheel { dx: 12, dy: 0 });
    }

    #[test]
    fn pinch_changes_the_view_not_the_remote() {
        let mut m = mapper();
        let out = m.apply(Action::Zoom {
            factor: 2.0,
            focal: Point::new(200.0, 400.0),
        });
        assert!(out.is_empty());
        assert!((m.view.zoom - 2.0).abs() < 1e-6);
        // After zooming 2x about the centre, a tap 100 px right of centre lands nearer the
        // centre of the remote than before.
        let tap = |m: &mut InputMapper| {
            let out = m.apply(Action::Click {
                pos: Point::new(300.0, 400.0),
                button: MouseButton::Left,
            });
            match out[0] {
                InputEvent::MouseMove { x, .. } => x,
                _ => 0,
            }
        };
        let zoomed = tap(&mut m);
        let mut plain = mapper();
        let unzoomed = tap(&mut plain);
        assert!(zoomed < unzoomed);
    }
}
