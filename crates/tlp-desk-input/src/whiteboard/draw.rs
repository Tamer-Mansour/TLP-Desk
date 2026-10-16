//! Viewer-side stroke building: live chunked streaming of freehand strokes and the point lists
//! of shapes (arrow, rectangle).

use super::simplify::{simplify, SIMPLIFY_EPSILON};
use super::{NormPoint, Rgba, Stroke, MAX_POINTS_PER_MESSAGE, MAX_POINTS_PER_STROKE};

/// Raw points buffered before a chunk is simplified and sent (the caller also flushes on a
/// timer, so a slow stroke still streams every few frames).
pub const FLUSH_POINTS: usize = 32;

/// A freehand stroke being drawn. Raw pointer positions go in with [`push`](Self::push);
/// simplified chunks come out, ready to send, so the host shows the stroke while it is drawn.
///
/// Each chunk is simplified together with the last point already committed (the anchor), so
/// the stroke stays continuous across chunk borders. [`committed`](Self::committed) is
/// exactly what the host received, so viewer and host draw the same polyline.
#[derive(Debug, Clone)]
pub struct StrokeStreamer {
    committed: Stroke,
    /// Points of `committed` already handed out as chunks.
    sent: usize,
    /// Raw points since the last flush.
    pending: Vec<NormPoint>,
    epsilon: f64,
}

impl StrokeStreamer {
    /// Starts a stroke at `first`.
    pub fn new(id: u64, color: Rgba, width: u16, first: NormPoint) -> Self {
        Self {
            committed: Stroke {
                id,
                points: vec![first],
                color,
                width,
            },
            sent: 0,
            pending: Vec::new(),
            epsilon: SIMPLIFY_EPSILON,
        }
    }

    /// Adds a raw pointer position. Returns chunks to send once [`FLUSH_POINTS`] raw points
    /// are buffered. Points past [`MAX_POINTS_PER_STROKE`] are ignored.
    pub fn push(&mut self, p: NormPoint) -> Vec<Stroke> {
        if self.is_full() {
            return Vec::new();
        }
        let last = self.pending.last().or(self.committed.points.last());
        if last != Some(&p) {
            self.pending.push(p);
        }
        if self.pending.len() >= FLUSH_POINTS {
            self.flush()
        } else {
            Vec::new()
        }
    }

    /// Simplifies the buffered points and returns the not yet sent part of the stroke as
    /// chunks of at most [`MAX_POINTS_PER_MESSAGE`] points (empty when nothing is new).
    pub fn flush(&mut self) -> Vec<Stroke> {
        if !self.pending.is_empty() {
            let anchor = self.committed.points.last().copied();
            let mut run: Vec<NormPoint> = anchor.into_iter().collect();
            run.append(&mut self.pending);
            let simplified = simplify(&run, self.epsilon);
            let room = MAX_POINTS_PER_STROKE.saturating_sub(self.committed.points.len());
            self.committed
                .points
                .extend(simplified.into_iter().skip(1).take(room));
        }
        let unsent = self.committed.points.get(self.sent..).unwrap_or_default();
        let chunks = unsent
            .chunks(MAX_POINTS_PER_MESSAGE)
            .map(|c| Stroke {
                id: self.committed.id,
                points: c.to_vec(),
                color: self.committed.color,
                width: self.committed.width,
            })
            .collect();
        self.sent = self.committed.points.len();
        chunks
    }

    /// Ends the stroke: the last chunks to send and the whole committed stroke.
    pub fn finish(mut self) -> (Vec<Stroke>, Stroke) {
        let chunks = self.flush();
        (chunks, self.committed)
    }

    /// The committed (simplified) stroke so far.
    pub fn committed(&self) -> &Stroke {
        &self.committed
    }

    /// Raw points not yet committed (draw them as a preview tail).
    pub fn pending(&self) -> &[NormPoint] {
        &self.pending
    }

    /// Whether the stroke reached [`MAX_POINTS_PER_STROKE`].
    pub fn is_full(&self) -> bool {
        self.committed
            .points
            .len()
            .saturating_add(self.pending.len())
            >= MAX_POINTS_PER_STROKE
    }
}

/// Converts a normalized point to "height units" (x scaled by the monitor aspect ratio), so
/// lengths and angles are the same in both directions.
fn to_iso(p: NormPoint, aspect: f64) -> (f64, f64) {
    (f64::from(p.0) * aspect, f64::from(p.1))
}

fn from_iso(x: f64, y: f64, aspect: f64) -> NormPoint {
    let x = if aspect > 0.0 { x / aspect } else { x };
    (clamp_unit(x), clamp_unit(y))
}

fn clamp_unit(v: f64) -> u16 {
    if v.is_nan() {
        return 0;
    }
    v.round().clamp(0.0, f64::from(u16::MAX)) as u16
}

/// An arrow from `from` to `to` as one polyline: shaft, then both head barbs
/// (`from, to, barb1, to, barb2`). `aspect` is the monitor's width / height, so the head keeps
/// its shape on any monitor. A zero-length arrow is a single point.
pub fn arrow_points(from: NormPoint, to: NormPoint, aspect: f64) -> Vec<NormPoint> {
    let aspect = if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        1.0
    };
    let (fx, fy) = to_iso(from, aspect);
    let (tx, ty) = to_iso(to, aspect);
    let (dx, dy) = (tx - fx, ty - fy);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1.0 {
        return vec![from];
    }
    // Head: 30 % of the shaft, between 1.5 % and 6 % of the monitor height.
    let unit = f64::from(u16::MAX);
    let head = (len * 0.3).clamp(unit * 0.015, unit * 0.06);
    let angle = dy.atan2(dx);
    let spread = 28f64.to_radians();
    let barb = |a: f64| {
        from_iso(
            tx - head * (angle - a).cos(),
            ty - head * (angle - a).sin(),
            aspect,
        )
    };
    vec![from, to, barb(spread), to, barb(-spread)]
}

/// A closed rectangle with corners `a` and `b` as one polyline (5 points). A degenerate
/// rectangle is a single point.
pub fn rect_points(a: NormPoint, b: NormPoint) -> Vec<NormPoint> {
    if a == b {
        return vec![a];
    }
    vec![a, (b.0, a.1), b, (a.0, b.1), a]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::whiteboard::{Board, WhiteboardOp};

    const RED: Rgba = Rgba::new(255, 0, 0, 255);

    #[test]
    fn streamed_chunks_rebuild_the_committed_stroke_on_the_host() {
        let mut s = StrokeStreamer::new(9, RED, 60, (100, 100));
        let mut sent = Vec::new();
        // A wobbly line of 500 raw points.
        for i in 1..500u16 {
            let y = 100 + (i % 7) * 3;
            sent.extend(s.push((100 + i * 50, y)));
        }
        assert!(!sent.is_empty(), "chunks must stream while drawing");
        let (last, whole) = s.finish();
        sent.extend(last);
        assert!(
            whole.points.len() < 500,
            "simplified: {}",
            whole.points.len()
        );
        let mut host = Board::new();
        for c in &sent {
            assert!(c.points.len() <= MAX_POINTS_PER_MESSAGE);
            for m in c.to_messages() {
                host.apply(&WhiteboardOp::from_message(&m).expect("stroke"));
            }
        }
        assert_eq!(host.get(9), Some(&whole));
        assert_eq!(whole.points.first(), Some(&(100, 100)));
        assert_eq!(
            whole.points.last(),
            Some(&(100 + 499 * 50, 100 + (499 % 7) * 3))
        );
    }

    #[test]
    fn a_click_is_a_one_point_stroke() {
        let s = StrokeStreamer::new(1, RED, 30, (5, 6));
        let (chunks, whole) = s.finish();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].points, vec![(5, 6)]);
        assert_eq!(whole.points, vec![(5, 6)]);
    }

    #[test]
    fn flush_without_new_points_sends_nothing_and_duplicates_are_ignored() {
        let mut s = StrokeStreamer::new(1, RED, 30, (5, 6));
        assert_eq!(s.flush().len(), 1);
        assert!(s.flush().is_empty());
        for _ in 0..100 {
            assert!(s.push((5, 6)).is_empty());
        }
        assert!(s.pending().is_empty());
        assert!(s.flush().is_empty());
    }

    #[test]
    fn stroke_point_cap_holds() {
        let mut s = StrokeStreamer::new(1, RED, 30, (0, 0));
        let mut total = 0;
        // A zig-zag that cannot be simplified away (steps well above the tolerance).
        for i in 0..20_000u32 {
            let p = (
                ((i * 100) % 60_000) as u16,
                if i % 2 == 0 { 0 } else { 30_000 },
            );
            for c in s.push(p) {
                total += c.points.len();
            }
        }
        let (last, whole) = s.finish();
        total += last.iter().map(|c| c.points.len()).sum::<usize>();
        assert_eq!(whole.points.len(), MAX_POINTS_PER_STROKE);
        assert_eq!(total, MAX_POINTS_PER_STROKE);
    }

    #[test]
    fn arrow_head_is_symmetric_and_aspect_corrected() {
        for aspect in [16.0 / 9.0, 4.0 / 3.0, 9.0 / 16.0, 1.0] {
            let from = (10_000, 30_000);
            let to = (50_000, 30_000);
            let pts = arrow_points(from, to, aspect);
            assert_eq!(pts.len(), 5);
            assert_eq!(pts[0], from);
            assert_eq!(pts[1], to);
            assert_eq!(pts[3], to);
            // Horizontal arrow: barbs mirror each other around the shaft, behind the tip.
            let (b1, b2) = (pts[2], pts[4]);
            assert!(b1.0 < to.0 && b2.0 < to.0);
            assert_eq!(b1.0, b2.0);
            assert_eq!(
                i32::from(b1.1) - 30_000,
                30_000 - i32::from(b2.1),
                "{aspect}"
            );
            // In height units the barb is as long as the head length, whatever the aspect.
            let (tx, ty) = to_iso(to, aspect);
            let (bx, by) = to_iso(b1, aspect);
            let barb = ((tx - bx).powi(2) + (ty - by).powi(2)).sqrt();
            assert!(
                (barb - 65535.0 * 0.06).abs() < 3.0 * aspect.max(1.0),
                "{barb}"
            );
        }
        assert_eq!(arrow_points((7, 7), (7, 7), 1.5), vec![(7, 7)]);
        // Barbs that would leave the screen are clamped to its edge.
        let pts = arrow_points((65535, 0), (0, 0), f64::NAN);
        assert_eq!(pts.len(), 5);
        assert_eq!(pts[2].1, 0, "{pts:?}");
        assert!(pts[4].1 > 0, "{pts:?}");
    }

    #[test]
    fn rectangle_is_closed() {
        assert_eq!(
            rect_points((10, 20), (30, 40)),
            vec![(10, 20), (30, 20), (30, 40), (10, 40), (10, 20)]
        );
        assert_eq!(rect_points((1, 1), (1, 1)), vec![(1, 1)]);
    }
}
