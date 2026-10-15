//! The viewer's drawing document: tools, undo/redo, eraser, fading strokes, and the ops to send.
//!
//! The [`Editor`] owns a [`Board`] that mirrors what the host shows. Every change produces
//! [`WhiteboardOp`]s in an outbox the UI forwards to the host. The wire has no "delete one
//! stroke" message, so removals (undo, eraser, fade) are sent as `Clear` followed by every
//! remaining stroke; boards are capped (see [`MAX_STROKES`]) so this stays small.

use std::time::{Duration, Instant};

use super::draw::{arrow_points, rect_points, StrokeStreamer};
use super::{Board, Damage, NormPoint, Rgba, Stroke, WhiteboardOp, MAX_STROKES, MAX_WIDTH};

/// Opacity of the highlighter (about 38 %).
pub const HIGHLIGHTER_ALPHA: u8 = 0x60;
/// The highlighter is this many times wider than the pen at the same width setting.
pub const HIGHLIGHTER_WIDTH_FACTOR: u16 = 4;
/// A freehand stroke is flushed to the host at least this often while drawing.
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(30);
/// Extra eraser reach around a stroke: 1 % of the monitor height (normalized units).
const ERASER_SLOP: f64 = 655.0;
/// Most undo steps kept.
const MAX_HISTORY: usize = 2 * MAX_STROKES;

/// A drawing tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tool {
    /// Freehand, opaque.
    #[default]
    Pen,
    /// Freehand, wide and translucent.
    Highlighter,
    /// Straight arrow from press to release.
    Arrow,
    /// Rectangle from press to release.
    Rectangle,
    /// Removes the strokes the pointer touches.
    Eraser,
}

/// Tool, color and width for new strokes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub tool: Tool,
    /// Opaque base color; the highlighter applies [`HIGHLIGHTER_ALPHA`].
    pub color: Rgba,
    /// Line width in 1/10000 of the monitor height (before the highlighter factor).
    pub width: u16,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            tool: Tool::Pen,
            color: Rgba::new(0xE5, 0x39, 0x35, 0xFF),
            width: 60,
        }
    }
}

impl Style {
    /// The color strokes of this style get.
    pub fn stroke_color(&self) -> Rgba {
        match self.tool {
            Tool::Highlighter => self.color.with_alpha(HIGHLIGHTER_ALPHA),
            _ => self.color,
        }
    }

    /// The wire width strokes of this style get.
    pub fn stroke_width(&self) -> u16 {
        let w = match self.tool {
            Tool::Highlighter => self.width.saturating_mul(HIGHLIGHTER_WIDTH_FACTOR),
            _ => self.width,
        };
        w.clamp(1, MAX_WIDTH)
    }
}

#[derive(Debug, Clone)]
enum Active {
    Free {
        streamer: StrokeStreamer,
        last_flush: Instant,
    },
    Shape {
        tool: Tool,
        id: u64,
        start: NormPoint,
        end: NormPoint,
        color: Rgba,
        width: u16,
    },
    Erasing,
}

/// An undoable step.
#[derive(Debug, Clone)]
enum Action {
    Add(Stroke),
    Erase(Stroke),
}

/// The viewer's whiteboard state (see the module docs).
#[derive(Debug, Clone)]
pub struct Editor {
    board: Board,
    active: Option<Active>,
    history: Vec<Action>,
    redo: Vec<Action>,
    /// Finished local strokes and when they were finished (for fading).
    finished: Vec<(u64, Instant)>,
    fade_after: Option<Duration>,
    next_id: u64,
    outbox: Vec<WhiteboardOp>,
    /// Width / height of the host monitor (arrow heads, eraser distances).
    aspect: f64,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    /// An empty document for a 16:9 monitor.
    pub fn new() -> Self {
        Self {
            board: Board::new(),
            active: None,
            history: Vec::new(),
            redo: Vec::new(),
            finished: Vec::new(),
            fade_after: None,
            next_id: 1,
            outbox: Vec::new(),
            aspect: 16.0 / 9.0,
        }
    }

    /// Sets the host monitor's width / height (ignored unless finite and positive).
    pub fn set_aspect(&mut self, aspect: f64) {
        if aspect.is_finite() && aspect > 0.0 {
            self.aspect = aspect;
        }
    }

    /// Strokes disappear this long after they were finished (`None`: they stay).
    pub fn set_fade(&mut self, fade_after: Option<Duration>) {
        self.fade_after = fade_after;
    }

    /// The fade setting.
    pub fn fade(&self) -> Option<Duration> {
        self.fade_after
    }

    fn new_id(&mut self) -> u64 {
        let mut id = self.next_id;
        // Never reuse an id that is still on the board (it would append to that stroke).
        while id == 0 || self.board.get(id).is_some() {
            id = id.wrapping_add(1);
        }
        self.next_id = id.wrapping_add(1);
        id
    }

    fn send_chunks(&mut self, chunks: Vec<Stroke>) {
        for c in chunks {
            self.board.add(&c);
            self.outbox.push(WhiteboardOp::Stroke(c));
        }
    }

    fn record(&mut self, action: Action) {
        if self.history.len() >= MAX_HISTORY {
            self.history.remove(0);
        }
        self.history.push(action);
    }

    /// Pointer pressed at `p` with `style`.
    pub fn begin(&mut self, p: NormPoint, style: Style, now: Instant) {
        self.end(now);
        self.active = Some(match style.tool {
            Tool::Pen | Tool::Highlighter => {
                let id = self.new_id();
                let mut streamer =
                    StrokeStreamer::new(id, style.stroke_color(), style.stroke_width(), p);
                // Send the first point at once: the host shows a dot under the pen.
                let first = streamer.flush();
                self.send_chunks(first);
                Active::Free {
                    streamer,
                    last_flush: now,
                }
            }
            Tool::Arrow | Tool::Rectangle => Active::Shape {
                tool: style.tool,
                id: self.new_id(),
                start: p,
                end: p,
                color: style.stroke_color(),
                width: style.stroke_width(),
            },
            Tool::Eraser => {
                self.erase_at(p);
                Active::Erasing
            }
        });
    }

    /// Pointer moved to `p` while pressed.
    pub fn drag(&mut self, p: NormPoint, now: Instant) {
        match self.active.as_mut() {
            Some(Active::Free {
                streamer,
                last_flush,
            }) => {
                let mut chunks = streamer.push(p);
                if !chunks.is_empty() {
                    *last_flush = now;
                } else if now.saturating_duration_since(*last_flush) >= FLUSH_INTERVAL {
                    chunks = streamer.flush();
                    *last_flush = now;
                }
                self.send_chunks(chunks);
            }
            Some(Active::Shape { end, .. }) => *end = p,
            Some(Active::Erasing) => self.erase_at(p),
            None => {}
        }
    }

    /// Pointer released (or drawing interrupted): finishes the current stroke or shape.
    pub fn end(&mut self, now: Instant) {
        match self.active.take() {
            Some(Active::Free { streamer, .. }) => {
                let (chunks, whole) = streamer.finish();
                self.send_chunks(chunks);
                self.finish_stroke(whole, now);
            }
            Some(Active::Shape {
                tool,
                id,
                start,
                end,
                color,
                width,
            }) => {
                let points = match tool {
                    Tool::Arrow => arrow_points(start, end, self.aspect),
                    _ => rect_points(start, end),
                };
                let stroke = Stroke {
                    id,
                    points,
                    color,
                    width,
                };
                self.send_chunks(vec![stroke.clone()]);
                self.finish_stroke(stroke, now);
            }
            Some(Active::Erasing) | None => {}
        }
    }

    fn finish_stroke(&mut self, stroke: Stroke, now: Instant) {
        self.redo.clear();
        self.finished.push((stroke.id, now));
        if self.finished.len() > MAX_STROKES {
            self.finished.remove(0);
        }
        self.record(Action::Add(stroke));
    }

    /// Periodic work (call every frame while the whiteboard is shown): streams a slow stroke
    /// and fades old strokes.
    pub fn tick(&mut self, now: Instant) {
        if let Some(Active::Free {
            streamer,
            last_flush,
        }) = self.active.as_mut()
        {
            if now.saturating_duration_since(*last_flush) >= FLUSH_INTERVAL {
                let chunks = streamer.flush();
                *last_flush = now;
                self.send_chunks(chunks);
            }
        }
        if let Some(fade) = self.fade_after {
            let expired: Vec<u64> = self
                .finished
                .iter()
                .filter(|(_, t)| now.saturating_duration_since(*t) >= fade)
                .map(|(id, _)| *id)
                .collect();
            if !expired.is_empty() {
                self.finished.retain(|(id, _)| !expired.contains(id));
                let mut removed = false;
                for id in expired {
                    removed |= self.board.remove(id).is_some();
                }
                if removed {
                    self.resync();
                }
            }
        }
    }

    fn erase_at(&mut self, p: NormPoint) {
        let hit = self
            .board
            .strokes()
            .iter()
            .rev()
            .find(|s| self.hits(s, p))
            .map(|s| s.id);
        if let Some(stroke) = hit.and_then(|id| self.board.remove(id)) {
            self.finished.retain(|(id, _)| *id != stroke.id);
            self.redo.clear();
            self.record(Action::Erase(stroke));
            self.resync();
        }
    }

    /// Whether `p` touches `stroke` (in height units, so the reach is round on screen).
    fn hits(&self, stroke: &Stroke, p: NormPoint) -> bool {
        let reach = f64::from(stroke.width) * f64::from(u16::MAX) / 10_000.0 / 2.0 + ERASER_SLOP;
        let a = self.aspect;
        let px = (f64::from(p.0) * a, f64::from(p.1));
        let d = match stroke.points.as_slice() {
            [] => f64::INFINITY,
            [q] => dist(px, (f64::from(q.0) * a, f64::from(q.1))),
            pts => pts
                .windows(2)
                .filter_map(|w| match w {
                    [s, e] => Some(iso_segment_distance(px, *s, *e, a)),
                    _ => None,
                })
                .fold(f64::INFINITY, f64::min),
        };
        d <= reach
    }

    /// Undoes the last drawing or erasing step. Returns `false` if there is none.
    pub fn undo(&mut self) -> bool {
        self.end(Instant::now());
        let Some(action) = self.history.pop() else {
            return false;
        };
        match action {
            Action::Add(stroke) => {
                self.board.remove(stroke.id);
                self.finished.retain(|(id, _)| *id != stroke.id);
                self.resync();
                self.redo.push(Action::Add(stroke));
            }
            Action::Erase(stroke) => {
                self.restore(&stroke);
                self.redo.push(Action::Erase(stroke));
            }
        }
        true
    }

    /// Redoes the last undone step. Returns `false` if there is none.
    pub fn redo(&mut self) -> bool {
        self.end(Instant::now());
        let Some(action) = self.redo.pop() else {
            return false;
        };
        match action {
            Action::Add(stroke) => {
                self.restore(&stroke);
                self.record(Action::Add(stroke));
            }
            Action::Erase(stroke) => {
                self.board.remove(stroke.id);
                self.resync();
                self.record(Action::Erase(stroke));
            }
        }
        true
    }

    fn restore(&mut self, stroke: &Stroke) {
        if self.board.get(stroke.id).is_none() {
            self.board.add(stroke);
            self.outbox.push(WhiteboardOp::Stroke(stroke.clone()));
        }
    }

    /// Whether [`undo`](Self::undo) has something to do.
    pub fn can_undo(&self) -> bool {
        !self.history.is_empty()
    }

    /// Whether [`redo`](Self::redo) has something to do.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Erases everything on both sides (not undoable).
    pub fn clear_all(&mut self) {
        self.reset_local();
        self.outbox.push(WhiteboardOp::Clear);
    }

    /// Forgets everything locally without telling the host (the host cleared, or the
    /// permission was revoked).
    pub fn reset_local(&mut self) {
        self.active = None;
        self.board.clear();
        self.history.clear();
        self.redo.clear();
        self.finished.clear();
    }

    /// Applies an op the host sent (its user cleared the screen, or host-side strokes).
    pub fn remote(&mut self, op: &WhiteboardOp) {
        match op {
            WhiteboardOp::Clear => self.reset_local(),
            WhiteboardOp::Stroke(s) => self.board.add(s),
        }
    }

    /// Replaces whatever is queued by `Clear` + every stroke: the host ends up with exactly
    /// our board whatever it had before.
    fn resync(&mut self) {
        self.outbox.clear();
        self.outbox.push(WhiteboardOp::Clear);
        for s in self.board.strokes() {
            self.outbox.push(WhiteboardOp::Stroke(s.clone()));
        }
    }

    /// The ops to send to the host, in order (empties the outbox).
    pub fn take_outbox(&mut self) -> Vec<WhiteboardOp> {
        std::mem::take(&mut self.outbox)
    }

    /// The strokes as the host shows them.
    pub fn board(&self) -> &Board {
        &self.board
    }

    /// What changed on the board since the last call (for incremental rendering).
    pub fn take_damage(&mut self) -> Damage {
        self.board.take_damage()
    }

    /// Marks the whole board for redrawing.
    pub fn invalidate(&mut self) {
        self.board.invalidate();
    }

    /// Whether a stroke, shape or erase gesture is in progress.
    pub fn is_drawing(&self) -> bool {
        self.active.is_some()
    }

    /// What to draw on top of the board while a gesture is in progress: the not yet sent
    /// tail of a freehand stroke, or the shape being dragged.
    pub fn preview(&self) -> Option<Stroke> {
        match self.active.as_ref()? {
            Active::Free { streamer, .. } => {
                let c = streamer.committed();
                let tail = streamer.pending();
                if tail.is_empty() {
                    return None;
                }
                let mut points: Vec<NormPoint> = c.points.last().copied().into_iter().collect();
                points.extend_from_slice(tail);
                Some(Stroke {
                    id: c.id,
                    points,
                    color: c.color,
                    width: c.width,
                })
            }
            Active::Shape {
                tool,
                id,
                start,
                end,
                color,
                width,
            } => Some(Stroke {
                id: *id,
                points: match tool {
                    Tool::Arrow => arrow_points(*start, *end, self.aspect),
                    _ => rect_points(*start, *end),
                },
                color: *color,
                width: *width,
            }),
            Active::Erasing => None,
        }
    }
}

fn dist(a: (f64, f64), b: (f64, f64)) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// Distance from `p` (height units) to the segment `s`-`e` (normalized) with x scaled by
/// `aspect`.
fn iso_segment_distance(p: (f64, f64), s: NormPoint, e: NormPoint, aspect: f64) -> f64 {
    let (ax, ay) = (f64::from(s.0) * aspect, f64::from(s.1));
    let (bx, by) = (f64::from(e.0) * aspect, f64::from(e.1));
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (((p.0 - ax) * dx + (p.1 - ay) * dy) / len2).clamp(0.0, 1.0)
    };
    dist(p, (ax + t * dx, ay + t * dy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::whiteboard::{MAX_POINTS_PER_MESSAGE, MAX_POINTS_PER_STROKE};
    use tlp_desk_proto::Message;

    fn t0() -> Instant {
        Instant::now()
    }

    /// Replays the editor's outbox onto a host-side board, through the wire format.
    fn deliver(e: &mut Editor, host: &mut Board) {
        for op in e.take_outbox() {
            for m in op.to_messages() {
                if let Message::WhiteboardStroke { points, .. } = &m {
                    assert!(points.len() <= MAX_POINTS_PER_MESSAGE);
                }
                host.apply(&WhiteboardOp::from_message(&m).expect("whiteboard op"));
            }
        }
    }

    fn same(a: &Board, b: &Board) -> bool {
        a.strokes() == b.strokes()
    }

    fn pen() -> Style {
        Style::default()
    }

    fn scribble(e: &mut Editor, host: &mut Board, start: Instant, y: u16, style: Style) -> u64 {
        e.begin((1000, y), style, start);
        deliver(e, host);
        for i in 1..120u16 {
            e.drag(
                (1000 + i * 300, y + (i % 5) * 200),
                start + Duration::from_millis(u64::from(i) * 8),
            );
            deliver(e, host);
        }
        e.end(start + Duration::from_secs(1));
        deliver(e, host);
        e.board().strokes().last().map(|s| s.id).unwrap_or(0)
    }

    #[test]
    fn host_follows_while_drawing_and_matches_at_the_end() {
        let start = t0();
        let mut e = Editor::new();
        let mut host = Board::new();
        e.begin((1000, 1000), pen(), start);
        deliver(&mut e, &mut host);
        // The first point is on the host before the pointer even moves.
        assert_eq!(host.strokes().len(), 1);
        let mut grew = 0;
        let mut last_len = 1;
        for i in 1..200u16 {
            e.drag(
                (1000 + i * 200, 1000 + (i % 9) * 150),
                start + Duration::from_millis(u64::from(i) * 8),
            );
            deliver(&mut e, &mut host);
            let len = host.strokes()[0].points.len();
            if len > last_len {
                grew += 1;
                last_len = len;
            }
        }
        assert!(grew >= 5, "host saw the stroke grow only {grew} times");
        e.end(start + Duration::from_secs(2));
        deliver(&mut e, &mut host);
        assert!(same(e.board(), &host));
        assert!(e.preview().is_none());
        assert!(e.can_undo() && !e.can_redo());
    }

    #[test]
    fn slow_strokes_flush_on_tick() {
        let start = t0();
        let mut e = Editor::new();
        let mut host = Board::new();
        e.begin((0, 0), pen(), start);
        e.drag((5000, 5000), start + Duration::from_millis(1));
        deliver(&mut e, &mut host);
        assert_eq!(host.strokes()[0].points.len(), 1);
        assert!(e.preview().is_some());
        e.tick(start + FLUSH_INTERVAL + Duration::from_millis(1));
        deliver(&mut e, &mut host);
        assert_eq!(host.strokes()[0].points, vec![(0, 0), (5000, 5000)]);
    }

    #[test]
    fn shapes_are_sent_on_release_with_a_preview_meanwhile() {
        let start = t0();
        let mut e = Editor::new();
        let mut host = Board::new();
        let style = Style {
            tool: Tool::Rectangle,
            ..pen()
        };
        e.begin((100, 100), style, start);
        e.drag((900, 700), start);
        assert!(e.take_outbox().is_empty());
        assert_eq!(e.preview().map(|s| s.points.len()), Some(5));
        e.end(start);
        deliver(&mut e, &mut host);
        assert_eq!(
            host.strokes()[0].points,
            rect_points((100, 100), (900, 700))
        );
        let arrow = Style {
            tool: Tool::Arrow,
            ..pen()
        };
        e.begin((100, 100), arrow, start);
        e.drag((30_000, 100), start);
        e.end(start);
        deliver(&mut e, &mut host);
        assert_eq!(host.strokes()[1].points.len(), 5);
        assert!(same(e.board(), &host));
    }

    #[test]
    fn highlighter_style_is_wide_and_translucent() {
        let s = Style {
            tool: Tool::Highlighter,
            color: Rgba::new(255, 255, 0, 255),
            width: 60,
        };
        assert_eq!(s.stroke_color().a(), HIGHLIGHTER_ALPHA);
        assert_eq!(s.stroke_width(), 240);
        let huge = Style {
            width: u16::MAX,
            ..s
        };
        assert_eq!(huge.stroke_width(), MAX_WIDTH);
        assert_eq!(pen().stroke_color().a(), 255);
    }

    #[test]
    fn undo_redo_and_eraser_stay_in_sync_with_the_host() {
        let start = t0();
        let mut e = Editor::new();
        let mut host = Board::new();
        let a = scribble(&mut e, &mut host, start, 5000, pen());
        let b = scribble(&mut e, &mut host, start, 30000, pen());
        assert_eq!(host.strokes().len(), 2);
        assert!(e.undo());
        deliver(&mut e, &mut host);
        assert_eq!(
            host.strokes().iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![a]
        );
        assert!(e.redo());
        deliver(&mut e, &mut host);
        assert!(same(e.board(), &host));
        assert_eq!(host.strokes().len(), 2);
        // Erase stroke b by touching one of its points.
        let p = e.board().get(b).map(|s| s.points[3]).expect("stroke b");
        e.begin(
            p,
            Style {
                tool: Tool::Eraser,
                ..pen()
            },
            start,
        );
        e.end(start);
        deliver(&mut e, &mut host);
        assert_eq!(
            host.strokes().iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![a]
        );
        // Undoing the erase brings it back; redo erases again.
        assert!(e.undo());
        deliver(&mut e, &mut host);
        assert!(host.get(b).is_some());
        assert!(e.redo());
        deliver(&mut e, &mut host);
        assert!(host.get(b).is_none());
        assert!(same(e.board(), &host));
        // Erasing in empty space does nothing.
        e.begin(
            (65000, 65000),
            Style {
                tool: Tool::Eraser,
                ..pen()
            },
            start,
        );
        e.end(start);
        assert!(e.take_outbox().is_empty());
        // A new stroke drops the redo stack.
        assert!(e.undo());
        assert!(e.can_redo());
        scribble(&mut e, &mut host, start, 50000, pen());
        assert!(!e.can_redo());
        while e.undo() {}
        deliver(&mut e, &mut host);
        assert!(host.is_empty() && e.board().is_empty());
        assert!(!e.redo() || e.can_undo());
    }

    #[test]
    fn eraser_reach_is_round_on_wide_monitors() {
        let mut e = Editor::new();
        e.set_aspect(32.0 / 9.0);
        let start = t0();
        let mut host = Board::new();
        // A vertical line at x = 20000.
        e.begin((20000, 1000), pen(), start);
        e.drag((20000, 60000), start);
        e.end(start);
        deliver(&mut e, &mut host);
        let id = e.board().strokes()[0].id;
        let s = e.board().get(id).cloned().expect("stroke");
        // 400 normalized x-units are ~1400 height units on a 32:9 monitor: out of reach.
        assert!(!e.hits(&s, (20400, 30000)));
        assert!(e.hits(&s, (20100, 30000)));
        // Vertically the reach is the plain slop.
        assert!(e.hits(&s, (20000, 60600)));
        assert!(!e.hits(&s, (20000, 62000)));
    }

    #[test]
    fn fading_removes_finished_strokes_on_both_sides() {
        let start = t0();
        let mut e = Editor::new();
        let mut host = Board::new();
        e.set_fade(Some(Duration::from_secs(5)));
        scribble(&mut e, &mut host, start, 5000, pen());
        // Finished at start + 1 s.
        e.tick(start + Duration::from_secs(3));
        assert!(e.take_outbox().is_empty());
        // A stroke in progress never fades.
        e.begin((100, 100), pen(), start + Duration::from_secs(5));
        deliver(&mut e, &mut host);
        e.tick(start + Duration::from_secs(7));
        deliver(&mut e, &mut host);
        assert_eq!(host.strokes().len(), 1);
        assert!(same(e.board(), &host));
        assert_eq!(e.fade(), Some(Duration::from_secs(5)));
    }

    #[test]
    fn clear_all_and_remote_ops() {
        let start = t0();
        let mut e = Editor::new();
        let mut host = Board::new();
        scribble(&mut e, &mut host, start, 5000, pen());
        e.clear_all();
        deliver(&mut e, &mut host);
        assert!(host.is_empty() && e.board().is_empty() && !e.can_undo());
        // The host cleared while we were drawing: the gesture is dropped too.
        e.begin((1, 1), pen(), start);
        e.remote(&WhiteboardOp::Clear);
        assert!(!e.is_drawing() && e.board().is_empty());
        // Host-side strokes are shown.
        let s = Stroke {
            id: 77,
            points: vec![(1, 2), (3, 4)],
            color: Rgba::new(1, 2, 3, 255),
            width: 10,
        };
        e.remote(&WhiteboardOp::Stroke(s.clone()));
        assert_eq!(e.board().get(77), Some(&s));
        // Our next stroke does not collide with it.
        e.next_id = 77;
        e.begin((5, 5), pen(), start);
        e.end(start);
        assert!(e.board().strokes().iter().filter(|x| x.id == 77).count() == 1);
        assert_eq!(e.board().strokes().len(), 2);
    }

    #[test]
    fn very_long_strokes_and_many_strokes_stay_bounded() {
        let start = t0();
        let mut e = Editor::new();
        let mut host = Board::new();
        e.begin((0, 0), pen(), start);
        for i in 0..30_000u32 {
            e.drag(
                (
                    ((i * 97) % 65_000) as u16,
                    if i % 2 == 0 { 0 } else { 40_000 },
                ),
                start + Duration::from_millis(u64::from(i)),
            );
        }
        e.end(start);
        deliver(&mut e, &mut host);
        assert!(host.strokes()[0].points.len() <= MAX_POINTS_PER_STROKE);
        for _ in 0..(MAX_STROKES + 20) {
            e.begin((9, 9), pen(), start);
            e.end(start);
        }
        deliver(&mut e, &mut host);
        assert_eq!(e.board().strokes().len(), MAX_STROKES);
        assert!(same(e.board(), &host));
        while e.undo() {}
        assert!(e.history.is_empty());
    }
}
