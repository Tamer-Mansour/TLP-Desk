//! Touch gesture recognition (pure logic, no egui or Android types).
//!
//! Direct-touch model, like tapping on the remote screen itself:
//!
//! | Gesture | Result |
//! |---|---|
//! | tap | left click at the finger |
//! | hold still ([`GestureConfig::long_press_ms`]) | right click at the finger |
//! | one-finger drag | left-button drag (select text, move windows) |
//! | two fingers moving together | scroll the remote window (natural direction) |
//! | two fingers spreading / pinching | zoom the local view about the fingers, and pan it |
//!
//! The recognizer turns raw [`Touch`] events into [`Action`]s; [`mapper`] turns those into wire
//! [`tlp_desk_proto::InputEvent`]s and view changes.

pub mod mapper;

use tlp_desk_proto::MouseButton;

use crate::view::Point;

/// Thresholds, in screen units (egui points) and milliseconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GestureConfig {
    /// A finger that moves less than this is still "still" (tap / long press).
    pub tap_slop: f32,
    /// Longest touch that still counts as a tap.
    pub tap_max_ms: u64,
    /// How long a still finger must stay down to right-click.
    pub long_press_ms: u64,
    /// Change of finger distance that turns two fingers into a pinch.
    pub pinch_slop: f32,
    /// Movement of the two-finger centre that turns it into a scroll.
    pub scroll_slop: f32,
}

impl Default for GestureConfig {
    fn default() -> Self {
        Self {
            tap_slop: 12.0,
            tap_max_ms: 300,
            long_press_ms: 500,
            pinch_slop: 24.0,
            scroll_slop: 10.0,
        }
    }
}

/// Phase of one finger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Finger touched the screen.
    Start,
    /// Finger moved.
    Move,
    /// Finger lifted.
    End,
    /// The system took the touch away.
    Cancel,
}

/// One raw touch event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Touch {
    /// Finger id, stable from Start to End.
    pub id: u64,
    /// What happened.
    pub phase: Phase,
    /// Where.
    pub pos: Point,
    /// Timestamp in milliseconds (any monotonic origin).
    pub t_ms: u64,
}

/// A recognized gesture step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    /// A complete click (press and release) at `pos`.
    Click {
        /// Where.
        pos: Point,
        /// Which button.
        button: MouseButton,
    },
    /// The left button goes down and a drag starts at `pos`.
    DragBegin(Point),
    /// The drag moved.
    DragMove(Point),
    /// The drag ended (button released).
    DragEnd(Point),
    /// Two fingers moved by `(dx, dy)`; `at` is their centre.
    Scroll {
        /// Finger movement, horizontal.
        dx: f32,
        /// Finger movement, vertical.
        dy: f32,
        /// Centre of the two fingers.
        at: Point,
    },
    /// Pinch: scale the local view by `factor` about `focal`.
    Zoom {
        /// New distance divided by the previous one.
        factor: f32,
        /// Centre of the two fingers.
        focal: Point,
    },
    /// The centre of a pinch moved: pan the local view.
    Pan {
        /// Horizontal movement.
        dx: f32,
        /// Vertical movement.
        dy: f32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum OneMode {
    Pending,
    Dragging,
    LongFired,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum TwoMode {
    Undecided,
    Pinch,
    Scroll,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Idle,
    One {
        start: Point,
        start_ms: u64,
        mode: OneMode,
    },
    Two {
        mode: TwoMode,
        d0: f32,
        c0: Point,
        last_d: f32,
        last_c: Point,
    },
    /// After a multi-touch gesture: ignore the remaining fingers until all are up.
    Drain,
}

/// The gesture state machine. Feed it every [`Touch`] and call [`Recognizer::tick`] regularly
/// (each frame) so a long press fires while the finger is held still.
#[derive(Debug)]
pub struct Recognizer {
    cfg: GestureConfig,
    state: State,
    /// Fingers currently down, in touch order.
    down: Vec<(u64, Point)>,
}

fn dist(a: Point, b: Point) -> f32 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

fn mid(a: Point, b: Point) -> Point {
    Point::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0)
}

/// Most fingers tracked at once; more are ignored (bounded state for hostile input).
const MAX_FINGERS: usize = 10;

impl Recognizer {
    /// Creates a recognizer with `cfg`.
    pub fn new(cfg: GestureConfig) -> Self {
        Self {
            cfg,
            state: State::Idle,
            down: Vec::new(),
        }
    }

    /// True while a drag holds the left button down.
    pub fn is_dragging(&self) -> bool {
        matches!(
            self.state,
            State::One {
                mode: OneMode::Dragging,
                ..
            }
        )
    }

    /// True while any finger is down (the caller keeps ticking so a long press can fire).
    pub fn is_active(&self) -> bool {
        !self.down.is_empty()
    }

    fn two_points(&self) -> Option<(Point, Point)> {
        match (self.down.first(), self.down.get(1)) {
            (Some(a), Some(b)) => Some((a.1, b.1)),
            _ => None,
        }
    }

    /// Processes one touch event; recognized actions are appended to `out`.
    pub fn touch(&mut self, t: Touch, out: &mut Vec<Action>) {
        match t.phase {
            Phase::Start => self.start(t, out),
            Phase::Move => self.moved(t, out),
            Phase::End | Phase::Cancel => self.end(t, out),
        }
    }

    fn start(&mut self, t: Touch, out: &mut Vec<Action>) {
        if self.down.len() >= MAX_FINGERS || self.down.iter().any(|(id, _)| *id == t.id) {
            return;
        }
        self.down.push((t.id, t.pos));
        match self.down.len() {
            1 => {
                self.state = State::One {
                    start: t.pos,
                    start_ms: t.t_ms,
                    mode: OneMode::Pending,
                }
            }
            2 => {
                if let State::One {
                    mode: OneMode::Dragging,
                    ..
                } = self.state
                {
                    // The second finger arrived mid-drag: release the button first.
                    if let Some((_, p)) = self.down.first() {
                        out.push(Action::DragEnd(*p));
                    }
                }
                if let Some((a, b)) = self.two_points() {
                    let d = dist(a, b);
                    let c = mid(a, b);
                    self.state = State::Two {
                        mode: TwoMode::Undecided,
                        d0: d,
                        c0: c,
                        last_d: d,
                        last_c: c,
                    };
                }
            }
            _ => {}
        }
    }

    fn moved(&mut self, t: Touch, out: &mut Vec<Action>) {
        let Some(slot) = self.down.iter_mut().find(|(id, _)| *id == t.id) else {
            return;
        };
        slot.1 = t.pos;
        match self.state {
            State::One {
                start,
                start_ms,
                mode,
            } => match mode {
                OneMode::Pending if dist(start, t.pos) > self.cfg.tap_slop => {
                    out.push(Action::DragBegin(start));
                    out.push(Action::DragMove(t.pos));
                    self.state = State::One {
                        start,
                        start_ms,
                        mode: OneMode::Dragging,
                    };
                }
                OneMode::Dragging => out.push(Action::DragMove(t.pos)),
                _ => {}
            },
            State::Two {
                mode,
                d0,
                c0,
                last_d,
                last_c,
            } => {
                let Some((a, b)) = self.two_points() else {
                    return;
                };
                let (d, c) = (dist(a, b), mid(a, b));
                let mut mode = mode;
                if mode == TwoMode::Undecided {
                    if (d - d0).abs() > self.cfg.pinch_slop {
                        mode = TwoMode::Pinch;
                    } else if dist(c, c0) > self.cfg.scroll_slop {
                        mode = TwoMode::Scroll;
                    }
                }
                let (mut nd, mut nc) = (last_d, last_c);
                match mode {
                    TwoMode::Pinch => {
                        if last_d > 0.0 && d > 0.0 {
                            out.push(Action::Zoom {
                                factor: d / last_d,
                                focal: c,
                            });
                        }
                        out.push(Action::Pan {
                            dx: c.x - last_c.x,
                            dy: c.y - last_c.y,
                        });
                        nd = d;
                        nc = c;
                    }
                    TwoMode::Scroll => {
                        out.push(Action::Scroll {
                            dx: c.x - last_c.x,
                            dy: c.y - last_c.y,
                            at: c,
                        });
                        nc = c;
                    }
                    TwoMode::Undecided => {}
                }
                self.state = State::Two {
                    mode,
                    d0,
                    c0,
                    last_d: nd,
                    last_c: nc,
                };
            }
            _ => {}
        }
    }

    fn end(&mut self, t: Touch, out: &mut Vec<Action>) {
        let Some(i) = self.down.iter().position(|(id, _)| *id == t.id) else {
            return;
        };
        let (_, last) = self.down.remove(i);
        match self.state {
            State::One {
                start,
                start_ms,
                mode,
            } => {
                match mode {
                    OneMode::Pending
                        if t.phase == Phase::End
                            && t.t_ms.saturating_sub(start_ms) <= self.cfg.tap_max_ms =>
                    {
                        out.push(Action::Click {
                            pos: start,
                            button: MouseButton::Left,
                        });
                    }
                    OneMode::Dragging => out.push(Action::DragEnd(last)),
                    _ => {}
                }
                self.state = State::Idle;
            }
            State::Two { .. } => {
                self.state = if self.down.is_empty() {
                    State::Idle
                } else {
                    State::Drain
                };
            }
            State::Drain => {
                if self.down.is_empty() {
                    self.state = State::Idle;
                }
            }
            State::Idle => {}
        }
    }

    /// Time passed without a touch event: fires the long press.
    pub fn tick(&mut self, now_ms: u64, out: &mut Vec<Action>) {
        if let State::One {
            start,
            start_ms,
            mode: OneMode::Pending,
        } = self.state
        {
            if now_ms.saturating_sub(start_ms) >= self.cfg.long_press_ms {
                out.push(Action::Click {
                    pos: start,
                    button: MouseButton::Right,
                });
                self.state = State::One {
                    start,
                    start_ms,
                    mode: OneMode::LongFired,
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: u64, phase: Phase, x: f32, y: f32, ms: u64) -> Touch {
        Touch {
            id,
            phase,
            pos: Point::new(x, y),
            t_ms: ms,
        }
    }

    fn run(r: &mut Recognizer, events: &[Touch]) -> Vec<Action> {
        let mut out = Vec::new();
        for e in events {
            r.touch(*e, &mut out);
        }
        out
    }

    #[test]
    fn quick_tap_is_a_left_click_where_the_finger_went_down() {
        let mut r = Recognizer::new(GestureConfig::default());
        let out = run(
            &mut r,
            &[
                t(1, Phase::Start, 50.0, 60.0, 0),
                t(1, Phase::Move, 53.0, 62.0, 40),
                t(1, Phase::End, 53.0, 62.0, 100),
            ],
        );
        assert_eq!(
            out,
            vec![Action::Click {
                pos: Point::new(50.0, 60.0),
                button: MouseButton::Left
            }]
        );
    }

    #[test]
    fn slow_release_is_not_a_tap() {
        let mut r = Recognizer::new(GestureConfig::default());
        let out = run(
            &mut r,
            &[
                t(1, Phase::Start, 5.0, 5.0, 0),
                t(1, Phase::End, 5.0, 5.0, 400),
            ],
        );
        assert!(out.is_empty());
    }

    #[test]
    fn long_press_right_clicks_once_and_release_does_nothing() {
        let mut r = Recognizer::new(GestureConfig::default());
        let mut out = Vec::new();
        r.touch(t(1, Phase::Start, 10.0, 20.0, 0), &mut out);
        r.tick(300, &mut out);
        assert!(out.is_empty());
        r.tick(520, &mut out);
        r.tick(900, &mut out);
        r.touch(t(1, Phase::End, 10.0, 20.0, 950), &mut out);
        assert_eq!(
            out,
            vec![Action::Click {
                pos: Point::new(10.0, 20.0),
                button: MouseButton::Right
            }]
        );
    }

    #[test]
    fn one_finger_drag_presses_at_the_start_and_releases_at_the_end() {
        let mut r = Recognizer::new(GestureConfig::default());
        let out = run(
            &mut r,
            &[
                t(1, Phase::Start, 10.0, 10.0, 0),
                t(1, Phase::Move, 40.0, 10.0, 30),
                t(1, Phase::Move, 80.0, 30.0, 60),
                t(1, Phase::End, 80.0, 30.0, 90),
            ],
        );
        assert_eq!(
            out,
            vec![
                Action::DragBegin(Point::new(10.0, 10.0)),
                Action::DragMove(Point::new(40.0, 10.0)),
                Action::DragMove(Point::new(80.0, 30.0)),
                Action::DragEnd(Point::new(80.0, 30.0)),
            ]
        );
        assert!(!r.is_dragging());
    }

    #[test]
    fn two_fingers_moving_together_scroll() {
        let mut r = Recognizer::new(GestureConfig::default());
        let out = run(
            &mut r,
            &[
                t(1, Phase::Start, 100.0, 300.0, 0),
                t(2, Phase::Start, 160.0, 300.0, 5),
                t(1, Phase::Move, 100.0, 280.0, 20),
                t(2, Phase::Move, 160.0, 280.0, 21),
                t(1, Phase::Move, 100.0, 250.0, 40),
                t(2, Phase::Move, 160.0, 250.0, 41),
            ],
        );
        let scrolls: Vec<_> = out
            .iter()
            .filter_map(|a| match a {
                Action::Scroll { dy, .. } => Some(*dy),
                _ => None,
            })
            .collect();
        assert!(!scrolls.is_empty());
        assert!((scrolls.iter().sum::<f32>() + 50.0).abs() < 0.01, "{out:?}");
        assert!(!out.iter().any(|a| matches!(a, Action::Zoom { .. })));
    }

    #[test]
    fn spreading_fingers_zoom_and_never_click() {
        let mut r = Recognizer::new(GestureConfig::default());
        let out = run(
            &mut r,
            &[
                t(1, Phase::Start, 100.0, 300.0, 0),
                t(2, Phase::Start, 140.0, 300.0, 5),
                t(1, Phase::Move, 80.0, 300.0, 20),
                t(2, Phase::Move, 160.0, 300.0, 21),
                t(1, Phase::End, 80.0, 300.0, 60),
                t(2, Phase::End, 160.0, 300.0, 61),
            ],
        );
        let zoom: f32 = out
            .iter()
            .filter_map(|a| match a {
                Action::Zoom { factor, .. } => Some(*factor),
                _ => None,
            })
            .product();
        assert!(zoom > 1.5, "{out:?}");
        assert!(!out.iter().any(|a| matches!(a, Action::Click { .. })));
    }

    #[test]
    fn lifting_one_finger_after_two_does_not_click_or_drag() {
        let mut r = Recognizer::new(GestureConfig::default());
        let out = run(
            &mut r,
            &[
                t(1, Phase::Start, 100.0, 300.0, 0),
                t(2, Phase::Start, 160.0, 300.0, 5),
                t(2, Phase::End, 160.0, 300.0, 50),
                t(1, Phase::Move, 300.0, 300.0, 80),
                t(1, Phase::End, 300.0, 300.0, 90),
            ],
        );
        assert!(out.is_empty(), "{out:?}");
        // The next touch works normally again.
        let out = run(
            &mut r,
            &[
                t(3, Phase::Start, 5.0, 5.0, 1000),
                t(3, Phase::End, 5.0, 5.0, 1050),
            ],
        );
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn second_finger_during_a_drag_releases_the_button() {
        let mut r = Recognizer::new(GestureConfig::default());
        let out = run(
            &mut r,
            &[
                t(1, Phase::Start, 10.0, 10.0, 0),
                t(1, Phase::Move, 60.0, 10.0, 20),
                t(2, Phase::Start, 200.0, 200.0, 30),
            ],
        );
        assert!(matches!(out.last(), Some(Action::DragEnd(_))));
    }

    #[test]
    fn duplicate_and_unknown_ids_and_finger_floods_are_harmless() {
        let mut r = Recognizer::new(GestureConfig::default());
        let mut out = Vec::new();
        r.touch(t(9, Phase::Move, 1.0, 1.0, 0), &mut out);
        r.touch(t(9, Phase::End, 1.0, 1.0, 0), &mut out);
        for id in 0..100 {
            r.touch(t(id, Phase::Start, id as f32, 0.0, 0), &mut out);
            r.touch(t(id, Phase::Start, id as f32, 0.0, 0), &mut out);
        }
        for id in 0..100 {
            r.touch(t(id, Phase::Cancel, 0.0, 0.0, 10), &mut out);
        }
        assert!(r.down.is_empty());
        assert!(out.is_empty());
    }
}
