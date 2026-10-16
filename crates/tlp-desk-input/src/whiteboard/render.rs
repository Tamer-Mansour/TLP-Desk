//! Anti-aliased stroke rendering with `tiny-skia`, shared by the host overlay window and the
//! viewer, so both sides draw the very same pixels.
//!
//! Output is premultiplied alpha in RGBA order (egui textures, tests) or BGRA order (GDI DIB
//! sections for `UpdateLayeredWindow`); BGRA is produced by swapping red and blue in the paint,
//! so no per-pixel conversion is needed.
//!
//! Rendering is incremental: [`damage_region`] turns a board's [`Damage`] into the pixel
//! rectangle that changed, [`render_region`] redraws only that rectangle (every stroke that
//! touches it, clipped, so translucent strokes are never drawn twice) and [`blit`] copies it
//! into the target buffer.

use tiny_skia::{
    FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke as SkStroke, Transform,
};

use super::{Board, Damage, NormPoint, Stroke};

/// Byte order of the rendered pixels (both premultiplied).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelOrder {
    /// `R, G, B, A` bytes (egui, PNG-like).
    Rgba,
    /// `B, G, R, A` bytes (Windows DIB sections).
    Bgra,
}

/// A rectangle in canvas pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PxRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PxRect {
    /// The whole canvas, `None` if it is empty.
    pub fn full(width: u32, height: u32) -> Option<Self> {
        (width > 0 && height > 0).then_some(Self {
            x: 0,
            y: 0,
            width,
            height,
        })
    }

    /// Smallest rectangle holding both.
    pub fn union(self, other: Self) -> Self {
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = self.right().max(other.right());
        let y1 = self.bottom().max(other.bottom());
        Self {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        }
    }

    /// One past the last column.
    pub fn right(self) -> u32 {
        self.x.saturating_add(self.width)
    }

    /// One past the last row.
    pub fn bottom(self) -> u32 {
        self.y.saturating_add(self.height)
    }

    /// Clips a float rectangle `[x0, x1) x [y0, y1)` to a `width` x `height` canvas (outward
    /// rounding). `None` if nothing is left.
    fn clip(x0: f32, y0: f32, x1: f32, y1: f32, width: u32, height: u32) -> Option<Self> {
        if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) {
            return PxRect::full(width, height);
        }
        let cx0 = x0.floor().clamp(0.0, width as f32) as u32;
        let cy0 = y0.floor().clamp(0.0, height as f32) as u32;
        let cx1 = x1.ceil().clamp(0.0, width as f32) as u32;
        let cy1 = y1.ceil().clamp(0.0, height as f32) as u32;
        (cx1 > cx0 && cy1 > cy0).then_some(Self {
            x: cx0,
            y: cy0,
            width: cx1 - cx0,
            height: cy1 - cy0,
        })
    }

    fn intersects(self, other: Self) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}

/// Canvas position of a normalized point: the center of the pixel the host's input mapping
/// (`denormalize`) would pick, continuous in between.
pub fn norm_to_px(p: NormPoint, width: u32, height: u32) -> (f32, f32) {
    let axis = |v: u16, extent: u32| {
        let span = extent.saturating_sub(1) as f32;
        f32::from(v) * span / f32::from(u16::MAX) + 0.5
    };
    (axis(p.0, width), axis(p.1, height))
}

/// Line width in canvas pixels for a wire width (1/10000 of the height), at least 1 px.
pub fn width_px(width: u16, canvas_height: u32) -> f32 {
    (f32::from(width) * canvas_height as f32 / 10_000.0).max(1.0)
}

/// Pixel bounds of `points` drawn with `stroke`'s width, clipped to the canvas.
fn points_bounds(points: &[NormPoint], stroke: &Stroke, width: u32, height: u32) -> Option<PxRect> {
    let half = width_px(stroke.width, height) / 2.0 + 2.0;
    let mut it = points.iter().map(|&p| norm_to_px(p, width, height));
    let (fx, fy) = it.next()?;
    let (x0, y0, x1, y1) = it.fold((fx, fy, fx, fy), |(a, b, c, d), (x, y)| {
        (a.min(x), b.min(y), c.max(x), d.max(y))
    });
    PxRect::clip(x0 - half, y0 - half, x1 + half, y1 + half, width, height)
}

/// The canvas rectangle that `damage` touches, `None` when nothing needs drawing.
pub fn damage_region(board: &Board, damage: &Damage, width: u32, height: u32) -> Option<PxRect> {
    match damage {
        Damage::None => None,
        Damage::Full => PxRect::full(width, height),
        Damage::Segments(segments) => segments
            .iter()
            .filter_map(|&(index, from)| {
                let stroke = board.strokes().get(index)?;
                // Include the previous point: the new segment starts there.
                let pts = stroke.points.get(from.saturating_sub(1)..)?;
                points_bounds(pts, stroke, width, height)
            })
            .reduce(PxRect::union),
    }
}

/// Renders every stroke of `board` that touches `region` of a `width` x `height` canvas into a
/// new transparent pixmap the size of `region` (premultiplied, `order` bytes). `None` for an
/// empty region.
pub fn render_region(
    board: &Board,
    width: u32,
    height: u32,
    region: PxRect,
    order: ChannelOrder,
) -> Option<Pixmap> {
    // The rasterizer's anti-aliasing differs slightly right at the pixmap border, so render a
    // margin around the region and crop it: no visible seam where regions meet.
    let outer = PxRect::clip(
        region.x as f32 - CLIP_MARGIN,
        region.y as f32 - CLIP_MARGIN,
        region.right() as f32 + CLIP_MARGIN,
        region.bottom() as f32 + CLIP_MARGIN,
        width,
        height,
    )?;
    let mut pixmap = Pixmap::new(outer.width, outer.height)?;
    let transform = Transform::from_translate(-(outer.x as f32), -(outer.y as f32));
    for stroke in board.strokes() {
        let touches = points_bounds(&stroke.points, stroke, width, height)
            .is_some_and(|b| b.intersects(outer));
        if touches {
            draw_stroke(&mut pixmap, stroke, width, height, transform, order);
        }
    }
    if outer == region {
        return Some(pixmap);
    }
    let inner = tiny_skia::IntRect::from_xywh(
        i32::try_from(region.x.checked_sub(outer.x)?).ok()?,
        i32::try_from(region.y.checked_sub(outer.y)?).ok()?,
        region.width,
        region.height,
    )?;
    pixmap.clone_rect(inner)
}

/// Extra pixels rendered around a region (see [`render_region`]).
const CLIP_MARGIN: f32 = 4.0;

/// Renders the whole board (see [`render_region`]).
pub fn render_full(board: &Board, width: u32, height: u32, order: ChannelOrder) -> Option<Pixmap> {
    render_region(board, width, height, PxRect::full(width, height)?, order)
}

fn draw_stroke(
    pixmap: &mut Pixmap,
    stroke: &Stroke,
    width: u32,
    height: u32,
    transform: Transform,
    order: ChannelOrder,
) {
    let c = stroke.color;
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    match order {
        ChannelOrder::Rgba => paint.set_color_rgba8(c.r(), c.g(), c.b(), c.a()),
        ChannelOrder::Bgra => paint.set_color_rgba8(c.b(), c.g(), c.r(), c.a()),
    }
    let line = width_px(stroke.width, height);
    let mut pts = stroke.points.iter().map(|&p| norm_to_px(p, width, height));
    let Some((x0, y0)) = pts.next() else {
        return;
    };
    if stroke.points.len() == 1 {
        // A click: a round dot as wide as the line.
        if let Some(dot) = PathBuilder::from_circle(x0, y0, line / 2.0) {
            pixmap.fill_path(&dot, &paint, FillRule::Winding, transform, None);
        }
        return;
    }
    let mut pb = PathBuilder::new();
    pb.move_to(x0, y0);
    for (x, y) in pts {
        pb.line_to(x, y);
    }
    let Some(path) = pb.finish() else {
        return;
    };
    let style = SkStroke {
        width: line,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..SkStroke::default()
    };
    pixmap.stroke_path(&path, &paint, &style, transform, None);
}

/// Copies `src` into the `region` of a tightly packed 4-byte-per-pixel buffer that is
/// `dst_width` pixels wide, replacing what was there. Returns `false` (copying nothing) if the
/// sizes do not fit.
pub fn blit(dst: &mut [u8], dst_width: u32, region: PxRect, src: &Pixmap) -> bool {
    if src.width() != region.width || src.height() != region.height || region.right() > dst_width {
        return false;
    }
    let row_bytes = region.width as usize * 4;
    let stride = dst_width as usize * 4;
    let rows_needed = region.bottom() as usize;
    if dst.len() < rows_needed.saturating_mul(stride) {
        return false;
    }
    for (row, src_row) in src.data().chunks_exact(row_bytes).enumerate() {
        let start = (region.y as usize + row) * stride + region.x as usize * 4;
        if let Some(d) = dst.get_mut(start..start + row_bytes) {
            d.copy_from_slice(src_row);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::whiteboard::{Rgba, StrokeStreamer};

    const W: u32 = 200;
    const H: u32 = 100;

    fn stroke(id: u64, pts: &[NormPoint], color: Rgba, width: u16) -> Stroke {
        Stroke {
            id,
            points: pts.to_vec(),
            color,
            width,
        }
    }

    fn px(p: &Pixmap, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * p.width() + x) * 4) as usize;
        [
            p.data()[i],
            p.data()[i + 1],
            p.data()[i + 2],
            p.data()[i + 3],
        ]
    }

    fn inked(p: &Pixmap) -> usize {
        p.data().chunks_exact(4).filter(|c| c[3] > 0).count()
    }

    /// Alpha-weighted centroid of the ink.
    fn centroid(p: &Pixmap) -> (f64, f64) {
        let (mut sx, mut sy, mut s) = (0.0, 0.0, 0.0);
        for (i, c) in p.data().chunks_exact(4).enumerate() {
            let a = f64::from(c[3]);
            sx += a * (i as u32 % p.width()) as f64;
            sy += a * (i as u32 / p.width()) as f64;
            s += a;
        }
        (sx / s, sy / s)
    }

    #[test]
    fn horizontal_line_pixel_stats() {
        let mut b = Board::new();
        // 10 % to 90 % across, mid-height, 10 px wide (1000/10000 of 100 px).
        let red = Rgba::new(255, 0, 0, 255);
        b.add(&stroke(1, &[(6554, 32768), (58982, 32768)], red, 1000));
        let p = render_full(&b, W, H, ChannelOrder::Rgba).expect("pixmap");
        assert_eq!(px(&p, 100, 50), [255, 0, 0, 255]);
        assert_eq!(px(&p, 100, 10), [0, 0, 0, 0]);
        assert_eq!(px(&p, 2, 50), [0, 0, 0, 0]);
        // Shaft 160 px x 10 px plus two round caps (pi * 25), plus anti-aliased edges.
        let n = inked(&p);
        assert!((1650..=2000).contains(&n), "{n}");
        let (cx, cy) = centroid(&p);
        assert!(
            (cx - 100.0).abs() < 1.0 && (cy - 50.0).abs() < 1.0,
            "{cx},{cy}"
        );
        // Fully opaque core: at least 160 x 8 pixels.
        let solid = p.data().chunks_exact(4).filter(|c| c[3] == 255).count();
        assert!(solid >= 160 * 8, "{solid}");

        let bgra = render_full(&b, W, H, ChannelOrder::Bgra).expect("pixmap");
        assert_eq!(px(&bgra, 100, 50), [0, 0, 255, 255]);
        assert_eq!(inked(&bgra), n);
    }

    #[test]
    fn highlighter_is_translucent_and_premultiplied() {
        let mut b = Board::new();
        let yellow = Rgba::new(255, 255, 0, 0x60);
        b.add(&stroke(1, &[(0, 32768), (65535, 32768)], yellow, 2000));
        let p = render_full(&b, W, H, ChannelOrder::Rgba).expect("pixmap");
        let [r, g, bl, a] = px(&p, 100, 50);
        assert_eq!(a, 0x60);
        assert!(
            r.abs_diff(0x60) <= 1 && g.abs_diff(0x60) <= 1 && bl == 0,
            "{r} {g} {bl}"
        );
        // A second translucent stroke across it composites (1 - (1 - a)^2).
        b.add(&stroke(2, &[(32768, 0), (32768, 65535)], yellow, 2000));
        let p = render_full(&b, W, H, ChannelOrder::Rgba).expect("pixmap");
        let a2 = px(&p, 100, 50)[3];
        let want = 255.0 * (1.0 - (1.0 - 96.0 / 255.0f64).powi(2));
        assert!((f64::from(a2) - want).abs() <= 2.0, "{a2} vs {want}");
        // ...but the joints of one polyline never double up.
        let mut zig = Board::new();
        zig.add(&stroke(
            1,
            &[(10000, 10000), (30000, 50000), (50000, 10000)],
            yellow,
            800,
        ));
        let p = render_full(&zig, W, H, ChannelOrder::Rgba).expect("pixmap");
        assert!(p.data().chunks_exact(4).all(|c| c[3] <= 0x60));
    }

    #[test]
    fn single_point_is_a_dot() {
        let mut b = Board::new();
        b.add(&stroke(
            1,
            &[(32768, 32768)],
            Rgba::new(0, 0, 255, 255),
            2000,
        ));
        let p = render_full(&b, W, H, ChannelOrder::Rgba).expect("pixmap");
        // The width is capped at 10 % of the height (10 px): a disc of radius 5, area ~79,
        // plus its anti-aliased rim.
        let n = inked(&p);
        assert!((75..=110).contains(&n), "{n}");
        assert_eq!(px(&p, 100, 50), [0, 0, 255, 255]);
    }

    #[test]
    fn region_render_matches_full_render() {
        let mut b = Board::new();
        let mut s = StrokeStreamer::new(1, Rgba::new(10, 200, 30, 200), 300, (1000, 1000));
        for i in 0..300u32 {
            let t = f64::from(i) / 300.0 * std::f64::consts::TAU;
            s.push((
                (32768.0 + 25000.0 * t.cos()) as u16,
                (32768.0 + 25000.0 * (3.0 * t).sin()) as u16,
            ));
        }
        b.add(&s.finish().1);
        b.add(&stroke(
            2,
            &[(0, 0), (65535, 65535)],
            Rgba::new(255, 0, 255, 0x60),
            900,
        ));
        let full = render_full(&b, W, H, ChannelOrder::Bgra).expect("pixmap");
        for region in [
            PxRect {
                x: 0,
                y: 0,
                width: 50,
                height: 40,
            },
            PxRect {
                x: 37,
                y: 21,
                width: 101,
                height: 59,
            },
            PxRect {
                x: 150,
                y: 60,
                width: 50,
                height: 40,
            },
        ] {
            let part = render_region(&b, W, H, region, ChannelOrder::Bgra).expect("region");
            let mut canvas = full.data().to_vec();
            // Overwrite the region with its separate render: at most a few anti-aliased edge
            // pixels may differ slightly (the rasterizer clips paths to the smaller pixmap).
            assert!(blit(&mut canvas, W, region, &part));
            let diffs: Vec<u8> = canvas
                .iter()
                .zip(full.data())
                .map(|(a, b)| a.abs_diff(*b))
                .filter(|d| *d > 1)
                .collect();
            let area = (region.width * region.height * 4) as usize;
            assert!(
                diffs.len() * 100 <= area,
                "{region:?}: {} bytes",
                diffs.len()
            );
            assert!(diffs.iter().all(|d| *d <= 24), "{region:?}: {diffs:?}");
        }
    }

    #[test]
    fn appended_segments_damage_only_their_area() {
        let mut b = Board::new();
        let black = Rgba::new(0, 0, 0, 255);
        b.add(&stroke(1, &[(0, 0), (6553, 6553)], black, 100));
        b.take_damage();
        b.add(&stroke(1, &[(13107, 6553)], black, 100));
        let d = b.take_damage();
        let r = damage_region(&b, &d, W, H).expect("region");
        // Covers the new segment from (20, 10) to (40, 10) plus the half width and margin.
        assert!(
            r.x <= 20 && r.right() >= 41 && r.y <= 9 && r.bottom() >= 11,
            "{r:?}"
        );
        assert!(r.width < 40 && r.height < 20, "{r:?}");
        assert_eq!(damage_region(&b, &Damage::None, W, H), None);
        assert_eq!(damage_region(&b, &Damage::Full, W, H), PxRect::full(W, H));
        // Stale indices are ignored, not a panic.
        assert_eq!(
            damage_region(&b, &Damage::Segments(vec![(42, 0)]), W, H),
            None
        );
    }

    #[test]
    fn degenerate_canvases_and_bad_blits() {
        let mut b = Board::new();
        b.add(&stroke(
            1,
            &[(0, 0), (65535, 65535)],
            Rgba::new(1, 2, 3, 4),
            50,
        ));
        assert!(render_full(&b, 0, 100, ChannelOrder::Rgba).is_none());
        assert!(render_full(&b, 1, 1, ChannelOrder::Rgba).is_some());
        let part = render_full(&b, 10, 10, ChannelOrder::Rgba).expect("pixmap");
        let mut small = vec![0u8; 10 * 9 * 4];
        assert!(!blit(
            &mut small,
            10,
            PxRect::full(10, 10).expect("rect"),
            &part
        ));
        let mut ok = vec![0u8; 20 * 20 * 4];
        let r = PxRect {
            x: 15,
            y: 0,
            width: 10,
            height: 10,
        };
        assert!(!blit(&mut ok, 20, r, &part));
        assert_eq!(norm_to_px((0, 65535), 1920, 1080), (0.5, 1079.5));
        assert_eq!(width_px(0, 1080), 1.0);
    }
}
