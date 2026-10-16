//! Shared helpers for the sharpness tests: a deterministic text-like scene and image metrics.
#![allow(dead_code)]

use tlp_desk_capture::BgraFrame;

/// Tiny deterministic generator (xorshift64*), so scenes are identical on every run.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Pseudo-glyph: a 5x7 bit pattern with 1-pixel strokes (the hardest case for lossy video).
fn glyph(seed: u64) -> [u8; 7] {
    let mut rng = Rng(seed | 1);
    let mut rows = [0u8; 7];
    for r in &mut rows {
        // Mostly vertical/horizontal strokes, like real letters.
        let v = (rng.next() >> 20) as u8 & 0x1f;
        *r = v & (v >> 1 | 0x11);
    }
    rows
}

/// A page of dark text on a light background, `width` x `height`, of an endless document
/// scrolled by `scroll` pixels (scrolling the scene is how tests create motion). `scale` is the pixel size of one
/// glyph dot (1 = tiny 5x7 text, 2 = about 11 px text).
pub fn text_scene(width: u32, height: u32, scale: u32, scroll: u32) -> BgraFrame {
    let mut f = BgraFrame::new_tight(width, height);
    let (w, h) = (width as usize, height as usize);
    for px in f.data.chunks_exact_mut(4) {
        px.copy_from_slice(&[250, 250, 250, 255]);
    }
    let cell_w = 6 * scale as usize;
    let cell_h = 10 * scale as usize;
    let cols = w / cell_w;
    let total_rows = h / cell_h + 2;
    let s = scale as usize;
    for row in 0..total_rows {
        let y0 = (row * cell_h) as i64 - (scroll as usize % cell_h) as i64;
        let doc_row = row + scroll as usize / cell_h;
        for col in 0..cols {
            let seed = (doc_row as u64) * 7919 + (col as u64) * 104_729 + 17;
            // Word gaps.
            if seed % 5 == 0 {
                continue;
            }
            let g = glyph(seed);
            for (gy, bits) in g.iter().enumerate() {
                for gx in 0..5 {
                    if bits >> gx & 1 == 0 {
                        continue;
                    }
                    for dy in 0..s {
                        for dx in 0..s {
                            let y = y0 + (gy * s + dy) as i64;
                            let x = col * cell_w + gx * s + dx;
                            if y >= 0 && (y as usize) < h && x < w {
                                let i = (y as usize * w + x) * 4;
                                f.data[i..i + 3].copy_from_slice(&[20, 20, 20]);
                            }
                        }
                    }
                }
            }
        }
    }
    f
}

fn luma(f: &BgraFrame) -> Vec<f64> {
    let (w, h) = (f.width as usize, f.height as usize);
    let stride = f.stride as usize;
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let i = y * stride + x * 4;
            let (b, g, r) = (f.data[i] as f64, f.data[i + 1] as f64, f.data[i + 2] as f64);
            out.push(0.2126 * r + 0.7152 * g + 0.0722 * b);
        }
    }
    out
}

/// Luma PSNR in dB over the whole frame (capped at 99 for identical images).
pub fn psnr(a: &BgraFrame, b: &BgraFrame) -> f64 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let (la, lb) = (luma(a), luma(b));
    let mse = la
        .iter()
        .zip(&lb)
        .map(|(x, y)| (x - y) * (x - y))
        .sum::<f64>()
        / la.len() as f64;
    if mse < 1e-9 {
        99.0
    } else {
        (10.0 * (255.0f64 * 255.0 / mse).log10()).min(99.0)
    }
}

/// Mean SSIM of the luma plane over non-overlapping 8x8 windows.
pub fn ssim(a: &BgraFrame, b: &BgraFrame) -> f64 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let (w, h) = (a.width as usize, a.height as usize);
    let (la, lb) = (luma(a), luma(b));
    let (c1, c2) = ((0.01f64 * 255.0).powi(2), (0.03f64 * 255.0).powi(2));
    let (mut sum, mut n) = (0.0, 0u32);
    for by in (0..h.saturating_sub(7)).step_by(8) {
        for bx in (0..w.saturating_sub(7)).step_by(8) {
            let (mut ma, mut mb) = (0.0, 0.0);
            for y in 0..8 {
                for x in 0..8 {
                    let i = (by + y) * w + bx + x;
                    ma += la[i];
                    mb += lb[i];
                }
            }
            ma /= 64.0;
            mb /= 64.0;
            let (mut va, mut vb, mut cov) = (0.0, 0.0, 0.0);
            for y in 0..8 {
                for x in 0..8 {
                    let i = (by + y) * w + bx + x;
                    va += (la[i] - ma).powi(2);
                    vb += (lb[i] - mb).powi(2);
                    cov += (la[i] - ma) * (lb[i] - mb);
                }
            }
            let (va, vb, cov) = (va / 63.0, vb / 63.0, cov / 63.0);
            sum += ((2.0 * ma * mb + c1) * (2.0 * cov + c2))
                / ((ma * ma + mb * mb + c1) * (va + vb + c2));
            n += 1;
        }
    }
    if n == 0 {
        1.0
    } else {
        sum / f64::from(n)
    }
}
