//! Ramer-Douglas-Peucker polyline simplification on normalized points.
//!
//! Iterative (an explicit stack instead of recursion), so a hostile or very long input cannot
//! overflow the thread stack; `O(n log n)` typical, `O(n^2)` worst case on at most
//! [`MAX_POINTS_PER_STROKE`](super::MAX_POINTS_PER_STROKE)-sized chunks.

use super::NormPoint;

/// Default tolerance in normalized units: about 0.4 px on a 1080-line monitor, so
/// simplification is invisible while freehand input usually shrinks to a third or less.
pub const SIMPLIFY_EPSILON: f64 = 24.0;

/// Removes points that deviate less than `epsilon` (normalized units) from the line through
/// their neighbours. Consecutive duplicates are dropped; the first and last points are always
/// kept. `epsilon <= 0` (or NaN) only drops duplicates.
pub fn simplify(points: &[NormPoint], epsilon: f64) -> Vec<NormPoint> {
    let mut pts: Vec<NormPoint> = Vec::with_capacity(points.len());
    for &p in points {
        if pts.last() != Some(&p) {
            pts.push(p);
        }
    }
    let n = pts.len();
    if n <= 2 || epsilon.is_nan() || epsilon <= 0.0 {
        return pts;
    }
    let mut keep = vec![false; n];
    if let Some(k) = keep.first_mut() {
        *k = true;
    }
    if let Some(k) = keep.last_mut() {
        *k = true;
    }
    let mut stack = vec![(0usize, n - 1)];
    while let Some((start, end)) = stack.pop() {
        if end <= start + 1 {
            continue;
        }
        let (Some(&a), Some(&b)) = (pts.get(start), pts.get(end)) else {
            continue;
        };
        let mut best = (0usize, -1.0f64);
        for (i, &p) in pts.iter().enumerate().take(end).skip(start + 1) {
            let d = segment_distance(p, a, b);
            if d > best.1 {
                best = (i, d);
            }
        }
        if best.1 > epsilon {
            if let Some(k) = keep.get_mut(best.0) {
                *k = true;
            }
            stack.push((start, best.0));
            stack.push((best.0, end));
        }
    }
    pts.into_iter()
        .zip(keep)
        .filter_map(|(p, k)| k.then_some(p))
        .collect()
}

/// Distance from `p` to the segment `a`-`b` (not the infinite line, so closed shapes and
/// back-and-forth scribbles keep their turning points).
pub(crate) fn segment_distance(p: NormPoint, a: NormPoint, b: NormPoint) -> f64 {
    let (px, py) = (f64::from(p.0), f64::from(p.1));
    let (ax, ay) = (f64::from(a.0), f64::from(a.1));
    let (bx, by) = (f64::from(b.0), f64::from(b.1));
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
    };
    let (cx, cy) = (ax + t * dx, ay + t * dy);
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_line_collapses_to_its_ends() {
        let line: Vec<NormPoint> = (0..=1000u16).map(|i| (i * 10, i * 5)).collect();
        assert_eq!(
            simplify(&line, SIMPLIFY_EPSILON),
            vec![(0, 0), (10000, 5000)]
        );
    }

    #[test]
    fn corners_survive() {
        let mut l: Vec<NormPoint> = (0..=100u16).map(|i| (i * 100, 0)).collect();
        l.extend((1..=100u16).map(|i| (10000, i * 100)));
        assert_eq!(
            simplify(&l, SIMPLIFY_EPSILON),
            vec![(0, 0), (10000, 0), (10000, 10000)]
        );
    }

    #[test]
    fn closed_and_back_and_forth_shapes_keep_turning_points() {
        // A stroke that goes right and comes back: the far end must stay.
        let mut l: Vec<NormPoint> = (0..=50u16).map(|i| (i * 100, 1000)).collect();
        l.extend((0..50u16).rev().map(|i| (i * 100, 1000)));
        let s = simplify(&l, SIMPLIFY_EPSILON);
        assert!(s.contains(&(5000, 1000)), "{s:?}");
        // A closed square keeps all four corners.
        let sq = [(0, 0), (500, 0), (1000, 0), (1000, 1000), (0, 1000), (0, 0)];
        assert_eq!(
            simplify(&sq, SIMPLIFY_EPSILON),
            vec![(0, 0), (1000, 0), (1000, 1000), (0, 1000), (0, 0)]
        );
    }

    #[test]
    fn freehand_curve_stays_within_tolerance() {
        let curve: Vec<NormPoint> = (0..2000)
            .map(|i| {
                let t = f64::from(i) / 2000.0 * std::f64::consts::TAU;
                (
                    (32768.0 + 20000.0 * t.cos()) as u16,
                    (32768.0 + 12000.0 * (2.0 * t).sin()) as u16,
                )
            })
            .collect();
        let s = simplify(&curve, SIMPLIFY_EPSILON);
        assert!(s.len() < curve.len() / 3, "{} of {}", s.len(), curve.len());
        // Every original point is within epsilon of the simplified polyline.
        for &p in &curve {
            let d = s
                .windows(2)
                .map(|w| segment_distance(p, w[0], w[1]))
                .fold(f64::INFINITY, f64::min);
            assert!(d <= SIMPLIFY_EPSILON + 1e-9, "{p:?} is {d} away");
        }
    }

    #[test]
    fn degenerate_inputs() {
        assert!(simplify(&[], 10.0).is_empty());
        assert_eq!(simplify(&[(1, 1)], 10.0), vec![(1, 1)]);
        assert_eq!(simplify(&[(1, 1), (1, 1), (1, 1)], 10.0), vec![(1, 1)]);
        assert_eq!(simplify(&[(0, 0), (5, 5), (9, 0)], f64::NAN).len(), 3);
        assert_eq!(simplify(&[(0, 0), (5, 5), (9, 0)], -1.0).len(), 3);
        // Extreme coordinates do not overflow.
        let s = simplify(&[(0, 0), (65535, 65535), (0, 65535), (65535, 0)], 1.0);
        assert_eq!(s.len(), 4);
        assert_eq!(segment_distance((3, 4), (0, 0), (0, 0)), 5.0);
    }
}
