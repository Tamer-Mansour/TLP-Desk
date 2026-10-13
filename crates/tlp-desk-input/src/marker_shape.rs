//! Software rasterizer for the green "remote pointer" marker shown on the host screen.
//!
//! Pure Rust and platform-independent so it can be unit-tested; the Windows overlay window
//! (`win::marker`) only uploads the result with `UpdateLayeredWindow`. The arrow is an
//! original polygon drawn with signed-distance anti-aliasing: a green fill, a white border and
//! a soft dark rim so it reads on light and dark backgrounds.

/// Fill colour (#22C55E).
pub const MARKER_GREEN: [u8; 3] = [0x22, 0xC5, 0x5E];
/// Rim / label background colour (#14532D).
pub const MARKER_DARK: [u8; 3] = [0x14, 0x53, 0x2D];

/// Arrow outline at 96 DPI, tip at the origin (x right, y down). Drawn larger than the
/// standard system pointer so the green shows around the real cursor drawn on top of it.
const ARROW: [(f32, f32); 7] = [
    (0.0, 0.0),
    (0.0, 30.0),
    (7.5, 23.5),
    (12.5, 34.0),
    (17.5, 31.8),
    (12.8, 21.6),
    (22.5, 21.6),
];

/// A premultiplied, top-down BGRA image with the pointer hotspot (the arrow tip).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerImage {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, premultiplied BGRA (what `UpdateLayeredWindow` expects).
    pub bgra: Vec<u8>,
    /// Arrow tip inside the image; place the image at `cursor - hotspot`.
    pub hotspot: (i32, i32),
}

/// Text coverage for the optional label: one byte per pixel, 0 = background, 255 = ink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextMask {
    pub width: u32,
    pub height: u32,
    pub coverage: Vec<u8>,
}

/// Signed distance from `p` to the polygon boundary (negative inside).
fn polygon_sd(poly: &[(f32, f32)], p: (f32, f32)) -> f32 {
    let mut d2 = f32::MAX;
    let mut inside = false;
    let n = poly.len();
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        let (ex, ey) = (b.0 - a.0, b.1 - a.1);
        let (wx, wy) = (p.0 - a.0, p.1 - a.1);
        let len2 = ex * ex + ey * ey;
        let t = if len2 > 0.0 {
            ((wx * ex + wy * ey) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (dx, dy) = (wx - ex * t, wy - ey * t);
        d2 = d2.min(dx * dx + dy * dy);
        if (a.1 > p.1) != (b.1 > p.1) {
            let x = a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0);
            if p.0 < x {
                inside = !inside;
            }
        }
    }
    let d = d2.sqrt();
    if inside {
        -d
    } else {
        d
    }
}

/// Signed distance to a rounded rectangle `(x, y, w, h)` with corner radius `r`.
fn round_rect_sd(rect: (f32, f32, f32, f32), r: f32, p: (f32, f32)) -> f32 {
    let (x, y, w, h) = rect;
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let qx = (p.0 - cx).abs() - (w / 2.0 - r);
    let qy = (p.1 - cy).abs() - (h / 2.0 - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

/// Coverage of the area where `sd <= edge`, anti-aliased over one pixel.
fn cov(sd: f32, edge: f32) -> f32 {
    (edge - sd + 0.5).clamp(0.0, 1.0)
}

/// "Source over" of a straight colour with coverage `a` onto a premultiplied BGRA pixel.
fn over(px: &mut [u8], rgb: [u8; 3], a: f32) {
    if a <= 0.0 {
        return;
    }
    let inv = 1.0 - a;
    let blend = |dst: u8, src: u8| (f32::from(src) * a + f32::from(dst) * inv).round() as u8;
    px[0] = blend(px[0], rgb[2]);
    px[1] = blend(px[1], rgb[1]);
    px[2] = blend(px[2], rgb[0]);
    px[3] = blend(px[3], 255);
}

impl MarkerImage {
    /// Renders the marker for a monitor scale factor (`dpi / 96`), with an optional label
    /// (e.g. the viewer's name) in a dark-green pill under the arrow.
    pub fn render(scale: f32, label: Option<&TextMask>) -> MarkerImage {
        let s = scale.clamp(0.5, 8.0);
        let rim = 1.6 * s;
        let border = 1.6 * s;
        let pad = (rim + 1.0).ceil();
        let poly: Vec<(f32, f32)> = ARROW
            .iter()
            .map(|&(x, y)| (x * s + pad, y * s + pad))
            .collect();
        let max_x = ARROW.iter().map(|p| p.0).fold(0.0f32, f32::max) * s;
        let max_y = ARROW.iter().map(|p| p.1).fold(0.0f32, f32::max) * s;
        let arrow_w = (max_x + 2.0 * pad).ceil() as u32;
        let arrow_h = (max_y + 2.0 * pad).ceil() as u32;

        // Label pill geometry.
        // Never let a label blow the window up: anything beyond the cap is dropped.
        let cap = (240.0 * s) as u32;
        let pill = label
            .filter(|m| m.width > 0 && m.height > 0 && m.width <= cap && m.height <= cap / 4)
            .map(|m| {
                let hp = (7.0 * s).round();
                let vp = (2.0 * s).round();
                let x = (pad + 10.0 * s).round();
                let y = (arrow_h as f32 - pad + 1.0 * s).round();
                let w = m.width as f32 + 2.0 * hp;
                let h = m.height as f32 + 2.0 * vp;
                (m, (x, y, w, h), (x + hp, y + vp))
            });
        let (width, height) = match &pill {
            Some((_, (x, y, w, h), _)) => (
                arrow_w.max((x + w + 1.0).ceil() as u32),
                arrow_h.max((y + h + 1.0).ceil() as u32),
            ),
            None => (arrow_w, arrow_h),
        };

        let mut bgra = vec![0u8; (width * height * 4) as usize];
        for py in 0..height {
            for px in 0..width {
                let p = (px as f32 + 0.5, py as f32 + 0.5);
                let i = ((py * width + px) * 4) as usize;
                let dst = &mut bgra[i..i + 4];
                if let Some((_, rect, _)) = &pill {
                    let r = rect.3 / 2.0;
                    let sd = round_rect_sd(*rect, r, p);
                    over(dst, MARKER_DARK, cov(sd, 0.0) * 0.92);
                }
                let sd = polygon_sd(&poly, p);
                if sd < rim + 1.0 {
                    over(dst, MARKER_DARK, cov(sd, rim) * 0.55);
                    over(dst, [255, 255, 255], cov(sd, 0.0));
                    over(dst, MARKER_GREEN, cov(sd, -border));
                }
            }
        }
        if let Some((mask, _, (tx, ty))) = &pill {
            let (tx, ty) = (*tx as i64, *ty as i64);
            for my in 0..mask.height {
                for mx in 0..mask.width {
                    let c = mask.coverage[(my * mask.width + mx) as usize];
                    if c == 0 {
                        continue;
                    }
                    let (x, y) = (tx + i64::from(mx), ty + i64::from(my));
                    if x < 0 || y < 0 || x >= i64::from(width) || y >= i64::from(height) {
                        continue;
                    }
                    let i = ((y as u32 * width + x as u32) * 4) as usize;
                    over(&mut bgra[i..i + 4], [255, 255, 255], f32::from(c) / 255.0);
                }
            }
        }
        MarkerImage {
            width,
            height,
            bgra,
            hotspot: (pad as i32, pad as i32),
        }
    }

    /// Upper bound for either side of a rendered marker at `scale` (arrow + capped label);
    /// the overlay window is never larger than this.
    pub fn max_extent(scale: f32) -> u32 {
        (300.0 * scale.clamp(0.5, 8.0)).ceil() as u32
    }

    /// Premultiplied BGRA of one pixel (for tests).
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.bgra[i],
            self.bgra[i + 1],
            self.bgra[i + 2],
            self.bgra[i + 3],
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrow_is_green_inside_and_transparent_outside() {
        let m = MarkerImage::render(1.0, None);
        assert_eq!(m.bgra.len(), (m.width * m.height * 4) as usize);
        assert!(m.width >= 24 && m.height >= 34, "{}x{}", m.width, m.height);
        // Deep inside the arrow body: opaque green (BGRA).
        let (hx, hy) = m.hotspot;
        let inside = m.pixel((hx + 5) as u32, (hy + 15) as u32);
        assert_eq!(inside, [0x5E, 0xC5, 0x22, 255]);
        // Far right of the tip, above the wing: fully transparent.
        assert_eq!(m.pixel(m.width - 1, 0), [0, 0, 0, 0]);
        // The tip itself is covered (white border or green).
        assert!(m.pixel(hx as u32, hy as u32)[3] > 100);
    }

    #[test]
    fn premultiplied_invariant_holds() {
        let m = MarkerImage::render(1.5, None);
        for px in m.bgra.chunks_exact(4) {
            assert!(px[0] <= px[3] && px[1] <= px[3] && px[2] <= px[3], "{px:?}");
        }
    }

    #[test]
    fn scales_with_dpi() {
        let a = MarkerImage::render(1.0, None);
        let b = MarkerImage::render(2.0, None);
        assert!(b.width >= a.width * 2 - 4 && b.height >= a.height * 2 - 4);
    }

    #[test]
    fn oversized_label_is_dropped_and_image_stays_small() {
        let huge = TextMask {
            width: 2000,
            height: 400,
            coverage: vec![255; 2000 * 400],
        };
        for scale in [1.0, 1.5, 2.5] {
            let m = MarkerImage::render(scale, Some(&huge));
            assert_eq!(m, MarkerImage::render(scale, None));
            assert!(m.width <= MarkerImage::max_extent(scale));
            assert!(m.height <= MarkerImage::max_extent(scale));
            // Mostly transparent: the arrow covers well under half of its box.
            let opaque = m.bgra.chunks_exact(4).filter(|p| p[3] == 255).count();
            assert!(opaque * 2 < (m.width * m.height) as usize);
        }
    }

    #[test]
    fn label_extends_image_and_draws_ink() {
        let mask = TextMask {
            width: 60,
            height: 14,
            coverage: vec![255; 60 * 14],
        };
        let plain = MarkerImage::render(1.0, None);
        let m = MarkerImage::render(1.0, Some(&mask));
        assert!(m.width > plain.width && m.height > plain.height);
        // Some pure-white text pixels exist.
        assert!(m.bgra.chunks_exact(4).any(|p| p == [255, 255, 255, 255]));
    }
}
