//! Whiteboard strokes, their wire form and the bounded [`Board`] both sides keep.
//!
//! Wire rules (`Message::WhiteboardStroke`, protocol v4):
//! * points are normalized to `0..=65535` over the streamed monitor, like `MouseMove`;
//! * a message whose `id` is already on the board **appends** its points to that stroke (the
//!   renderer joins the previous last point to the new first point), so a stroke can be
//!   streamed in small chunks while it is being drawn; color and width come from the first
//!   chunk;
//! * `color_rgba` is `0xRRGGBBAA`, `width` is in 1/10000 of the monitor height.
//!
//! Everything from the peer is hostile: [`WhiteboardOp::from_message`] caps the points per
//! message and clamps the width, and the [`Board`] caps points per stroke, strokes per session
//! and points per session (evicting the oldest strokes) before anything is allocated for good.

use tlp_desk_proto::Message;

/// A point normalized to `0..=65535` over the streamed monitor (x right, y down).
pub type NormPoint = (u16, u16);

/// Most points a sender puts into one `WhiteboardStroke` message.
pub const MAX_POINTS_PER_MESSAGE: usize = 256;
/// Most points a receiver takes from one message; the rest is dropped.
pub const MAX_WIRE_POINTS: usize = 1024;
/// Most points one stroke may have; later points are dropped.
pub const MAX_POINTS_PER_STROKE: usize = 4096;
/// Most strokes on a board; the oldest is evicted for a new one.
pub const MAX_STROKES: usize = 256;
/// Most points on a board in total; the oldest strokes are evicted to make room.
pub const MAX_TOTAL_POINTS: usize = 65_536;
/// Thinnest accepted line width (1/10000 of the monitor height).
pub const MIN_WIDTH: u16 = 1;
/// Widest accepted line width: 10 % of the monitor height.
pub const MAX_WIDTH: u16 = 1000;
/// Damage entries kept before the board falls back to a full redraw.
const MAX_DAMAGE_SEGMENTS: usize = 64;

/// A stroke color, `0xRRGGBBAA` like the wire field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgba(pub u32);

impl Rgba {
    /// Builds a color from its channels.
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self(u32::from_be_bytes([r, g, b, a]))
    }

    /// Red channel.
    pub const fn r(self) -> u8 {
        self.0.to_be_bytes()[0]
    }

    /// Green channel.
    pub const fn g(self) -> u8 {
        self.0.to_be_bytes()[1]
    }

    /// Blue channel.
    pub const fn b(self) -> u8 {
        self.0.to_be_bytes()[2]
    }

    /// Alpha (opacity) channel.
    pub const fn a(self) -> u8 {
        self.0.to_be_bytes()[3]
    }

    /// The same color with another alpha.
    pub const fn with_alpha(self, a: u8) -> Self {
        Self::new(self.r(), self.g(), self.b(), a)
    }
}

/// One stroke (or one chunk of a stroke on the wire): a polyline in normalized coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stroke {
    /// Sender-chosen id; chunks with the same id extend the same stroke.
    pub id: u64,
    pub points: Vec<NormPoint>,
    pub color: Rgba,
    /// Line width in 1/10000 of the monitor height.
    pub width: u16,
}

impl Stroke {
    /// The stroke as one wire message (callers keep `points` at most
    /// [`MAX_POINTS_PER_MESSAGE`]; use [`Stroke::to_messages`] otherwise).
    pub fn to_message(&self) -> Message {
        Message::WhiteboardStroke {
            id: self.id,
            points: self.points.clone(),
            color_rgba: self.color.0,
            width: self.width,
        }
    }

    /// The stroke as wire chunks of at most [`MAX_POINTS_PER_MESSAGE`] points each, in order.
    /// Empty strokes produce nothing.
    pub fn to_messages(&self) -> Vec<Message> {
        self.points
            .chunks(MAX_POINTS_PER_MESSAGE)
            .map(|chunk| Message::WhiteboardStroke {
                id: self.id,
                points: chunk.to_vec(),
                color_rgba: self.color.0,
                width: self.width,
            })
            .collect()
    }

    /// Smallest rectangle (normalized) holding every point, `None` without points.
    pub fn bounds(&self) -> Option<NormRect> {
        NormRect::around(&self.points)
    }
}

/// A rectangle in normalized coordinates, edges inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NormRect {
    pub min: NormPoint,
    pub max: NormPoint,
}

impl NormRect {
    /// Bounding box of `points`, `None` if there are none.
    pub fn around(points: &[NormPoint]) -> Option<Self> {
        let (&first, rest) = points.split_first()?;
        Some(rest.iter().fold(
            NormRect {
                min: first,
                max: first,
            },
            |r, &(x, y)| NormRect {
                min: (r.min.0.min(x), r.min.1.min(y)),
                max: (r.max.0.max(x), r.max.1.max(y)),
            },
        ))
    }
}

/// A whiteboard command, decoded and sanitized from a [`Message`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhiteboardOp {
    /// Add a stroke, or append to the stroke with the same id.
    Stroke(Stroke),
    /// Erase everything.
    Clear,
}

impl WhiteboardOp {
    /// Decodes a whiteboard message. Points beyond [`MAX_WIRE_POINTS`] are dropped and the
    /// width is clamped to [`MIN_WIDTH`]`..=`[`MAX_WIDTH`]. `None` for other messages and for
    /// strokes without points.
    pub fn from_message(msg: &Message) -> Option<Self> {
        match msg {
            Message::WhiteboardClear => Some(Self::Clear),
            Message::WhiteboardStroke {
                id,
                points,
                color_rgba,
                width,
            } => {
                if points.is_empty() {
                    return None;
                }
                let n = points.len().min(MAX_WIRE_POINTS);
                Some(Self::Stroke(Stroke {
                    id: *id,
                    points: points.get(..n).unwrap_or_default().to_vec(),
                    color: Rgba(*color_rgba),
                    width: (*width).clamp(MIN_WIDTH, MAX_WIDTH),
                }))
            }
            _ => None,
        }
    }

    /// The wire messages for this op (a long stroke becomes several chunks).
    pub fn to_messages(&self) -> Vec<Message> {
        match self {
            Self::Stroke(s) => s.to_messages(),
            Self::Clear => vec![Message::WhiteboardClear],
        }
    }
}

/// What changed on a [`Board`] since the renderer last looked.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Damage {
    /// Nothing to draw.
    #[default]
    None,
    /// Redraw everything (a clear, an eviction, a removal).
    Full,
    /// `(stroke index, first new point)` pairs: points were appended to these strokes.
    Segments(Vec<(usize, usize)>),
}

impl Damage {
    fn add_segment(&mut self, index: usize, from: usize) {
        match self {
            Damage::Full => {}
            Damage::None => *self = Damage::Segments(vec![(index, from)]),
            Damage::Segments(v) => {
                if let Some(e) = v.iter_mut().find(|(i, _)| *i == index) {
                    e.1 = e.1.min(from);
                } else if v.len() >= MAX_DAMAGE_SEGMENTS {
                    *self = Damage::Full;
                } else {
                    v.push((index, from));
                }
            }
        }
    }
}

/// The strokes of one session, oldest first, with hard caps and damage tracking.
#[derive(Debug, Clone, Default)]
pub struct Board {
    strokes: Vec<Stroke>,
    total_points: usize,
    damage: Damage,
}

impl Board {
    /// An empty board.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies a decoded op.
    pub fn apply(&mut self, op: &WhiteboardOp) {
        match op {
            WhiteboardOp::Stroke(s) => self.add(s),
            WhiteboardOp::Clear => self.clear(),
        }
    }

    /// Adds `chunk` as a new stroke, or appends its points to the stroke with the same id.
    /// Enforces every cap (see the module docs).
    pub fn add(&mut self, chunk: &Stroke) {
        if chunk.points.is_empty() {
            return;
        }
        // Chunks almost always extend the newest stroke: search from the end.
        if let Some(index) = self.strokes.iter().rposition(|s| s.id == chunk.id) {
            let have = self.strokes.get(index).map_or(0, |s| s.points.len());
            let room = MAX_POINTS_PER_STROKE.saturating_sub(have);
            let take = chunk.points.len().min(room);
            if take == 0 {
                return;
            }
            let index = self.make_room(take, Some(index));
            if let Some(s) = index.and_then(|i| self.strokes.get_mut(i)) {
                s.points
                    .extend_from_slice(chunk.points.get(..take).unwrap_or_default());
                self.total_points = self.total_points.saturating_add(take);
                if let Some(i) = index {
                    self.damage.add_segment(i, have);
                }
            }
            return;
        }
        let take = chunk.points.len().min(MAX_POINTS_PER_STROKE);
        if self.strokes.len() >= MAX_STROKES {
            self.evict_oldest();
        }
        self.make_room(take, None);
        self.strokes.push(Stroke {
            id: chunk.id,
            points: chunk.points.get(..take).unwrap_or_default().to_vec(),
            color: chunk.color,
            width: chunk.width.clamp(MIN_WIDTH, MAX_WIDTH),
        });
        self.total_points = self.total_points.saturating_add(take);
        let index = self.strokes.len().saturating_sub(1);
        self.damage.add_segment(index, 0);
    }

    /// Evicts the oldest strokes (other than `keep`) until `extra` more points fit. Returns
    /// the new index of `keep`.
    fn make_room(&mut self, extra: usize, keep: Option<usize>) -> Option<usize> {
        let mut keep = keep;
        while self.total_points.saturating_add(extra) > MAX_TOTAL_POINTS {
            // Oldest stroke that is not the one being extended.
            let victim = if keep == Some(0) { 1 } else { 0 };
            if victim >= self.strokes.len() {
                break;
            }
            let removed = self.strokes.remove(victim);
            self.total_points = self.total_points.saturating_sub(removed.points.len());
            self.damage = Damage::Full;
            keep = keep.map(|k| if k > victim { k - 1 } else { k });
        }
        keep
    }

    fn evict_oldest(&mut self) {
        if !self.strokes.is_empty() {
            let removed = self.strokes.remove(0);
            self.total_points = self.total_points.saturating_sub(removed.points.len());
            self.damage = Damage::Full;
        }
    }

    /// Erases everything.
    pub fn clear(&mut self) {
        if !self.strokes.is_empty() {
            self.damage = Damage::Full;
        }
        self.strokes.clear();
        self.total_points = 0;
    }

    /// Removes the stroke with `id` and returns it.
    pub fn remove(&mut self, id: u64) -> Option<Stroke> {
        let index = self.strokes.iter().rposition(|s| s.id == id)?;
        let removed = self.strokes.remove(index);
        self.total_points = self.total_points.saturating_sub(removed.points.len());
        self.damage = Damage::Full;
        Some(removed)
    }

    /// The strokes, oldest (bottom) first.
    pub fn strokes(&self) -> &[Stroke] {
        &self.strokes
    }

    /// The stroke with `id`, if any.
    pub fn get(&self, id: u64) -> Option<&Stroke> {
        self.strokes.iter().rev().find(|s| s.id == id)
    }

    /// Whether the board has no strokes.
    pub fn is_empty(&self) -> bool {
        self.strokes.is_empty()
    }

    /// Points over all strokes.
    pub fn total_points(&self) -> usize {
        self.total_points
    }

    /// What changed since the last call (resets the damage).
    pub fn take_damage(&mut self) -> Damage {
        std::mem::take(&mut self.damage)
    }

    /// Marks everything for redrawing (e.g. after the render target changed size).
    pub fn invalidate(&mut self) {
        self.damage = Damage::Full;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(id: u64, points: &[NormPoint]) -> Stroke {
        Stroke {
            id,
            points: points.to_vec(),
            color: Rgba::new(255, 0, 0, 255),
            width: 50,
        }
    }

    #[test]
    fn rgba_channels_roundtrip() {
        let c = Rgba::new(0x12, 0x34, 0x56, 0x78);
        assert_eq!(c.0, 0x1234_5678);
        assert_eq!((c.r(), c.g(), c.b(), c.a()), (0x12, 0x34, 0x56, 0x78));
        assert_eq!(c.with_alpha(0xff).0, 0x1234_56ff);
    }

    #[test]
    fn same_id_appends_and_reports_segment_damage() {
        let mut b = Board::new();
        b.add(&chunk(1, &[(0, 0), (10, 10)]));
        assert_eq!(b.take_damage(), Damage::Segments(vec![(0, 0)]));
        b.add(&chunk(1, &[(20, 20), (30, 30)]));
        b.add(&chunk(2, &[(5, 5)]));
        assert_eq!(b.take_damage(), Damage::Segments(vec![(0, 2), (1, 0)]));
        assert_eq!(b.strokes().len(), 2);
        assert_eq!(b.get(1).map(|s| s.points.len()), Some(4));
        assert_eq!(b.total_points(), 5);
        assert_eq!(b.take_damage(), Damage::None);
    }

    #[test]
    fn clear_and_remove_need_a_full_redraw() {
        let mut b = Board::new();
        b.add(&chunk(1, &[(0, 0)]));
        b.add(&chunk(2, &[(1, 1), (2, 2)]));
        b.take_damage();
        assert_eq!(b.remove(1).map(|s| s.id), Some(1));
        assert_eq!(b.take_damage(), Damage::Full);
        assert_eq!(b.total_points(), 2);
        assert!(b.remove(99).is_none());
        b.apply(&WhiteboardOp::Clear);
        assert!(b.is_empty());
        assert_eq!(b.total_points(), 0);
        assert_eq!(b.take_damage(), Damage::Full);
        // Clearing an empty board changes nothing.
        b.clear();
        assert_eq!(b.take_damage(), Damage::None);
    }

    #[test]
    fn per_stroke_point_cap() {
        let mut b = Board::new();
        let pts: Vec<NormPoint> = (0..1000u16).map(|i| (i, i)).collect();
        for _ in 0..10 {
            b.add(&chunk(7, &pts));
        }
        assert_eq!(
            b.get(7).map(|s| s.points.len()),
            Some(MAX_POINTS_PER_STROKE)
        );
        assert_eq!(b.total_points(), MAX_POINTS_PER_STROKE);
    }

    #[test]
    fn stroke_count_cap_evicts_oldest() {
        let mut b = Board::new();
        for id in 0..(MAX_STROKES as u64 + 10) {
            b.add(&chunk(id, &[(1, 1)]));
        }
        assert_eq!(b.strokes().len(), MAX_STROKES);
        assert_eq!(b.strokes().first().map(|s| s.id), Some(10));
        assert_eq!(b.total_points(), MAX_STROKES);
        assert_eq!(b.take_damage(), Damage::Full);
    }

    #[test]
    fn total_point_cap_evicts_oldest_but_keeps_the_growing_stroke() {
        let mut b = Board::new();
        let pts: Vec<NormPoint> = vec![(3, 3); MAX_POINTS_PER_STROKE];
        // 16 full strokes reach the total cap exactly.
        for id in 0..16 {
            b.add(&chunk(id, &pts));
        }
        assert_eq!(b.total_points(), MAX_TOTAL_POINTS);
        // Extending the oldest stroke evicts the next oldest instead of itself.
        let mut b2 = Board::new();
        b2.add(&chunk(100, &pts[..10]));
        for id in 0..16 {
            b2.add(&chunk(id, &pts[..4000]));
        }
        assert_eq!(b2.total_points(), 64_010);
        b2.add(&chunk(100, &pts[..2000]));
        assert_eq!(b2.get(100).map(|s| s.points.len()), Some(2010));
        assert!(b2.get(0).is_none());
        assert!(b2.get(1).is_some());
        assert_eq!(b2.total_points(), 62_010);
        // A 17th full stroke evicts the oldest.
        b.add(&chunk(99, &pts));
        assert!(b.get(0).is_none());
        assert!(b.get(99).is_some());
        assert!(b.total_points() <= MAX_TOTAL_POINTS);
    }

    #[test]
    fn hostile_messages_are_sanitized() {
        let huge: Vec<NormPoint> = vec![(65535, 65535); 100_000];
        let msg = Message::WhiteboardStroke {
            id: u64::MAX,
            points: huge,
            color_rgba: 0,
            width: u16::MAX,
        };
        let Some(WhiteboardOp::Stroke(s)) = WhiteboardOp::from_message(&msg) else {
            panic!("stroke expected");
        };
        assert_eq!(s.points.len(), MAX_WIRE_POINTS);
        assert_eq!(s.width, MAX_WIDTH);
        let thin = Message::WhiteboardStroke {
            id: 1,
            points: vec![(0, 0)],
            color_rgba: 0,
            width: 0,
        };
        assert!(matches!(
            WhiteboardOp::from_message(&thin),
            Some(WhiteboardOp::Stroke(Stroke {
                width: MIN_WIDTH,
                ..
            }))
        ));
        let empty = Message::WhiteboardStroke {
            id: 1,
            points: vec![],
            color_rgba: 0,
            width: 10,
        };
        assert_eq!(WhiteboardOp::from_message(&empty), None);
        assert_eq!(
            WhiteboardOp::from_message(&Message::WhiteboardClear),
            Some(WhiteboardOp::Clear)
        );
        assert_eq!(WhiteboardOp::from_message(&Message::RequestKeyframe), None);
    }

    #[test]
    fn long_strokes_split_into_bounded_chunks_that_rebuild_the_stroke() {
        let s = Stroke {
            id: 3,
            points: (0..600u16).map(|i| (i, 2 * i)).collect(),
            color: Rgba::new(0, 0, 255, 128),
            width: 40,
        };
        let msgs = s.to_messages();
        assert_eq!(msgs.len(), 3);
        let mut b = Board::new();
        for m in &msgs {
            match m {
                Message::WhiteboardStroke { points, .. } => {
                    assert!(points.len() <= MAX_POINTS_PER_MESSAGE)
                }
                other => panic!("unexpected {other:?}"),
            }
            if let Some(op) = WhiteboardOp::from_message(m) {
                b.apply(&op);
            }
        }
        assert_eq!(b.get(3), Some(&s));
        assert_eq!(
            WhiteboardOp::Clear.to_messages(),
            vec![Message::WhiteboardClear]
        );
        assert_eq!(
            s.bounds(),
            Some(NormRect {
                min: (0, 0),
                max: (599, 1198)
            })
        );
    }
}
