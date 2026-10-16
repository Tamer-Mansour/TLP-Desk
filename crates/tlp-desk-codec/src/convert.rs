//! BGRA <-> I420 (YUV 4:2:0 planar) conversion, BT.709 limited range, integer math.
//!
//! Written so the inner loops use `chunks_exact` (no bounds checks) and auto-vectorise; large
//! frames are split into horizontal bands converted on scoped threads.

use std::thread;

use tlp_desk_capture::BgraFrame;

/// Rows per band below which threading is not worth it.
const MIN_ROWS_PER_BAND: usize = 128;

/// An I420 picture with tight planes. Width/height are always even.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct I420Buffer {
    pub width: usize,
    pub height: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

/// Rounds up to the next even number (4:2:0 needs even dimensions).
pub fn even_up(v: u32) -> u32 {
    (v + 1) & !1
}

impl I420Buffer {
    /// Resizes the planes for a `width` x `height` picture (both rounded up to even), reusing the
    /// existing allocations when possible.
    pub fn resize(&mut self, width: u32, height: u32) {
        let (w, h) = (even_up(width) as usize, even_up(height) as usize);
        self.width = w;
        self.height = h;
        self.y.resize(w * h, 0);
        self.u.resize(w * h / 4, 0);
        self.v.resize(w * h / 4, 0);
    }
}

fn worker_count(rows: usize) -> usize {
    let cpus = thread::available_parallelism().map_or(1, |n| n.get());
    (rows / MIN_ROWS_PER_BAND).clamp(1, cpus.min(8))
}

// BT.709 limited-range coefficients scaled by 256.
const YR: i32 = 47;
const YG: i32 = 157;
const YB: i32 = 16;
const UR: i32 = -26;
const UG: i32 = -87;
const UB: i32 = 113;
const VR: i32 = 112;
const VG: i32 = -102;
const VB: i32 = -10;

#[inline(always)]
fn luma(b: i32, g: i32, r: i32) -> u8 {
    (((YR * r + YG * g + YB * b + 128) >> 8) + 16) as u8
}

/// `b`, `g`, `r` are sums over a 2x2 block (4 samples).
#[inline(always)]
fn chroma(b: i32, g: i32, r: i32) -> (u8, u8) {
    let u = ((UR * r + UG * g + UB * b + 512) >> 10) + 128;
    let v = ((VR * r + VG * g + VB * b + 512) >> 10) + 128;
    (u.clamp(0, 255) as u8, v.clamp(0, 255) as u8)
}

/// Converts one source row pair. `r0`/`r1` hold exactly `src_w * 4` bytes; the output rows are
/// `even_up(src_w)` wide (an odd last column is duplicated).
fn bgra_row_pair(r0: &[u8], r1: &[u8], y0: &mut [u8], y1: &mut [u8], u: &mut [u8], v: &mut [u8]) {
    let pairs = r0.len() / 8;
    let it = r0
        .chunks_exact(8)
        .zip(r1.chunks_exact(8))
        .zip(y0.chunks_exact_mut(2))
        .zip(y1.chunks_exact_mut(2))
        .zip(u.iter_mut())
        .zip(v.iter_mut());
    for (((((a, b), ya), yb), uo), vo) in it {
        let (a0b, a0g, a0r) = (a[0] as i32, a[1] as i32, a[2] as i32);
        let (a1b, a1g, a1r) = (a[4] as i32, a[5] as i32, a[6] as i32);
        let (b0b, b0g, b0r) = (b[0] as i32, b[1] as i32, b[2] as i32);
        let (b1b, b1g, b1r) = (b[4] as i32, b[5] as i32, b[6] as i32);
        ya[0] = luma(a0b, a0g, a0r);
        ya[1] = luma(a1b, a1g, a1r);
        yb[0] = luma(b0b, b0g, b0r);
        yb[1] = luma(b1b, b1g, b1r);
        let (cu, cv) = chroma(
            a0b + a1b + b0b + b1b,
            a0g + a1g + b0g + b1g,
            a0r + a1r + b0r + b1r,
        );
        *uo = cu;
        *vo = cv;
    }
    // Odd width: the last source pixel stands in for the padding column too.
    if r0.len() % 8 == 4 {
        let i = pairs * 8;
        let (a, b) = (&r0[i..i + 4], &r1[i..i + 4]);
        let x = pairs * 2;
        let ya = luma(a[0] as i32, a[1] as i32, a[2] as i32);
        let yb = luma(b[0] as i32, b[1] as i32, b[2] as i32);
        y0[x] = ya;
        y0[x + 1] = ya;
        y1[x] = yb;
        y1[x + 1] = yb;
        let (cu, cv) = chroma(
            2 * (a[0] as i32 + b[0] as i32),
            2 * (a[1] as i32 + b[1] as i32),
            2 * (a[2] as i32 + b[2] as i32),
        );
        u[pairs] = cu;
        v[pairs] = cv;
    }
}

/// Converts BGRA (any stride, any size) into `out`, which is resized to the even-rounded size.
/// An odd last row/column is padded by duplicating the edge pixels.
///
/// # Panics
/// Panics if `src` is not well formed (see [`BgraFrame::is_well_formed`]).
pub fn bgra_to_i420(src: &BgraFrame, out: &mut I420Buffer) {
    assert!(src.is_well_formed(), "malformed BGRA frame");
    out.resize(src.width, src.height);
    let w = out.width;
    let pair_rows = out.height / 2;
    if pair_rows == 0 {
        return;
    }

    let workers = worker_count(out.height);
    let pairs_per_band = pair_rows.div_ceil(workers);
    let convert_band = |first_pair: usize, ys: &mut [u8], us: &mut [u8], vs: &mut [u8]| {
        let it = ys
            .chunks_exact_mut(2 * w)
            .zip(us.chunks_exact_mut(w / 2))
            .zip(vs.chunks_exact_mut(w / 2));
        for (k, ((yy, uu), vv)) in it.enumerate() {
            let sy0 = (first_pair + k) as u32 * 2;
            let sy1 = (sy0 + 1).min(src.height - 1);
            let (y0, y1) = yy.split_at_mut(w);
            bgra_row_pair(src.row(sy0), src.row(sy1), y0, y1, uu, vv);
        }
    };

    if workers == 1 {
        convert_band(0, &mut out.y, &mut out.u, &mut out.v);
        return;
    }
    thread::scope(|s| {
        let bands = out
            .y
            .chunks_mut(pairs_per_band * 2 * w)
            .zip(out.u.chunks_mut(pairs_per_band * w / 2))
            .zip(out.v.chunks_mut(pairs_per_band * w / 2));
        for (i, ((ys, us), vs)) in bands.enumerate() {
            let convert_band = &convert_band;
            s.spawn(move || convert_band(i * pairs_per_band, ys, us, vs));
        }
    });
}

/// Output size of [`bgra_to_i420_half`] for a `width` x `height` source.
pub fn half_size(width: u32, height: u32) -> (u32, u32) {
    (even_up(width.div_ceil(2)), even_up(height.div_ceil(2)))
}

/// Sum of B, G, R over a 2x2 source block whose top-left is (`x`, `y`), edges clamped.
#[inline(always)]
fn block_sum_clamped(src: &BgraFrame, x: u32, y: u32) -> (i32, i32, i32) {
    let (mut b, mut g, mut r) = (0, 0, 0);
    for dy in 0..2 {
        let row = src.row((y + dy).min(src.height - 1));
        for dx in 0..2 {
            let i = (x + dx).min(src.width - 1) as usize * 4;
            b += row[i] as i32;
            g += row[i + 1] as i32;
            r += row[i + 2] as i32;
        }
    }
    (b, g, r)
}

/// One output row pair of the half-size conversion: 4 source rows -> 2 Y rows + 1 chroma row.
fn half_row_quad(rows: [&[u8]; 4], y0: &mut [u8], y1: &mut [u8], u: &mut [u8], v: &mut [u8]) {
    // Fast path: 4 source pixels (16 bytes) per row -> 2 output pixels per row.
    let it = rows[0]
        .chunks_exact(16)
        .zip(rows[1].chunks_exact(16))
        .zip(rows[2].chunks_exact(16))
        .zip(rows[3].chunks_exact(16))
        .zip(y0.chunks_exact_mut(2))
        .zip(y1.chunks_exact_mut(2))
        .zip(u.iter_mut())
        .zip(v.iter_mut());
    for (((((((a, b), c), d), ya), yb), uo), vo) in it {
        let sum = |p: &[u8], q: &[u8], o: usize| -> (i32, i32, i32) {
            (
                p[o] as i32 + p[o + 4] as i32 + q[o] as i32 + q[o + 4] as i32,
                p[o + 1] as i32 + p[o + 5] as i32 + q[o + 1] as i32 + q[o + 5] as i32,
                p[o + 2] as i32 + p[o + 6] as i32 + q[o + 2] as i32 + q[o + 6] as i32,
            )
        };
        let s00 = sum(a, b, 0);
        let s01 = sum(a, b, 8);
        let s10 = sum(c, d, 0);
        let s11 = sum(c, d, 8);
        ya[0] = luma((s00.0 + 2) >> 2, (s00.1 + 2) >> 2, (s00.2 + 2) >> 2);
        ya[1] = luma((s01.0 + 2) >> 2, (s01.1 + 2) >> 2, (s01.2 + 2) >> 2);
        yb[0] = luma((s10.0 + 2) >> 2, (s10.1 + 2) >> 2, (s10.2 + 2) >> 2);
        yb[1] = luma((s11.0 + 2) >> 2, (s11.1 + 2) >> 2, (s11.2 + 2) >> 2);
        let (cu, cv) = chroma(
            (s00.0 + s01.0 + s10.0 + s11.0 + 2) >> 2,
            (s00.1 + s01.1 + s10.1 + s11.1 + 2) >> 2,
            (s00.2 + s01.2 + s10.2 + s11.2 + 2) >> 2,
        );
        *uo = cu;
        *vo = cv;
    }
}

/// Converts BGRA to I420 at half the resolution in each direction (2x2 box filter), in one pass.
/// Used when the source exceeds what the encoder accepts (e.g. 3840x2400 > 3840x2160) or to
/// trade sharpness for speed. `out` becomes [`half_size`] of the source.
///
/// # Panics
/// Panics if `src` is not well formed.
pub fn bgra_to_i420_half(src: &BgraFrame, out: &mut I420Buffer) {
    assert!(src.is_well_formed(), "malformed BGRA frame");
    let (ow, oh) = half_size(src.width, src.height);
    out.resize(ow, oh);
    let w = out.width;
    let pair_rows = out.height / 2;
    if pair_rows == 0 {
        return;
    }
    // Output pixel pairs fully covered by the fast path (4 whole source pixels each).
    let fast_pairs = src.width as usize / 4;

    let convert_band = |first_pair: usize, ys: &mut [u8], us: &mut [u8], vs: &mut [u8]| {
        let it = ys
            .chunks_exact_mut(2 * w)
            .zip(us.chunks_exact_mut(w / 2))
            .zip(vs.chunks_exact_mut(w / 2));
        for (k, ((yy, uu), vv)) in it.enumerate() {
            let oy = (first_pair + k) as u32 * 2; // first output row of this pair
            let sy = oy * 2; // first of 4 source rows
            let last = src.height - 1;
            let rows = [
                src.row(sy.min(last)),
                src.row((sy + 1).min(last)),
                src.row((sy + 2).min(last)),
                src.row((sy + 3).min(last)),
            ];
            let (y0, y1) = yy.split_at_mut(w);
            half_row_quad(rows, y0, y1, uu, vv);
            // Right edge not covered by whole 4-pixel groups: clamped slow path.
            for p in fast_pairs..w / 2 {
                let ox = p as u32 * 2;
                let s00 = block_sum_clamped(src, ox * 2, sy);
                let s01 = block_sum_clamped(src, ox * 2 + 2, sy);
                let s10 = block_sum_clamped(src, ox * 2, sy + 2);
                let s11 = block_sum_clamped(src, ox * 2 + 2, sy + 2);
                let l = |s: (i32, i32, i32)| luma((s.0 + 2) >> 2, (s.1 + 2) >> 2, (s.2 + 2) >> 2);
                y0[p * 2] = l(s00);
                y0[p * 2 + 1] = l(s01);
                y1[p * 2] = l(s10);
                y1[p * 2 + 1] = l(s11);
                let (cu, cv) = chroma(
                    (s00.0 + s01.0 + s10.0 + s11.0 + 2) >> 2,
                    (s00.1 + s01.1 + s10.1 + s11.1 + 2) >> 2,
                    (s00.2 + s01.2 + s10.2 + s11.2 + 2) >> 2,
                );
                uu[p] = cu;
                vv[p] = cv;
            }
        }
    };

    let workers = worker_count(src.height as usize);
    if workers == 1 {
        convert_band(0, &mut out.y, &mut out.u, &mut out.v);
        return;
    }
    let pairs_per_band = pair_rows.div_ceil(workers);
    thread::scope(|s| {
        let bands = out
            .y
            .chunks_mut(pairs_per_band * 2 * w)
            .zip(out.u.chunks_mut(pairs_per_band * w / 2))
            .zip(out.v.chunks_mut(pairs_per_band * w / 2));
        for (i, ((ys, us), vs)) in bands.enumerate() {
            let convert_band = &convert_band;
            s.spawn(move || convert_band(i * pairs_per_band, ys, us, vs));
        }
    });
}

/// Bytes of a tight NV12 picture of the buffer's size: the Y plane then interleaved U/V.
pub fn nv12_len(src: &I420Buffer) -> usize {
    src.width * src.height * 3 / 2
}

/// Writes `src` as tight NV12 (Y plane, then one half-height plane of interleaved U, V) into
/// the first [`nv12_len`] bytes of `dst`. Returns `false` (writing nothing) when `dst` is too
/// small or `src`'s planes do not match its size.
pub fn i420_to_nv12(src: &I420Buffer, dst: &mut [u8]) -> bool {
    let (w, h) = (src.width, src.height);
    let y_len = w * h;
    let c_len = y_len / 4;
    if dst.len() < nv12_len(src)
        || src.y.len() < y_len
        || src.u.len() < c_len
        || src.v.len() < c_len
    {
        return false;
    }
    let (y_dst, uv_dst) = dst.split_at_mut(y_len);
    y_dst.copy_from_slice(&src.y[..y_len]);
    for ((pair, &u), &v) in uv_dst[..c_len * 2]
        .chunks_exact_mut(2)
        .zip(&src.u[..c_len])
        .zip(&src.v[..c_len])
    {
        pair[0] = u;
        pair[1] = v;
    }
    true
}

// BT.709 limited-range inverse, scaled by 4096.
const CY: i32 = 4769; // 1.1644
const CRV: i32 = 7343; // 1.7927
const CGU: i32 = 873; // 0.2132
const CGV: i32 = 2183; // 0.5329
const CBU: i32 = 8652; // 2.1124

#[inline(always)]
fn clamp_u8(v: i32) -> u8 {
    (v >> 12).clamp(0, 255) as u8
}

#[inline(always)]
fn put_px(out: &mut [u8], y: u8, rv: i32, guv: i32, bu: i32) {
    let yy = (y as i32 - 16) * CY + 2048;
    out[0] = clamp_u8(yy + bu);
    out[1] = clamp_u8(yy - guv);
    out[2] = clamp_u8(yy + rv);
    out[3] = 255;
}

/// Converts one output row (`out` holds `width * 4` bytes) from its Y row and chroma row.
fn i420_row(y: &[u8], u: &[u8], v: &[u8], out: &mut [u8]) {
    let it = out
        .chunks_exact_mut(8)
        .zip(y.chunks_exact(2))
        .zip(u.iter())
        .zip(v.iter());
    for (((o, yy), &cu), &cv) in it {
        let (du, dv) = (cu as i32 - 128, cv as i32 - 128);
        let (rv, guv, bu) = (CRV * dv, CGU * du + CGV * dv, CBU * du);
        let (o0, o1) = o.split_at_mut(4);
        put_px(o0, yy[0], rv, guv, bu);
        put_px(o1, yy[1], rv, guv, bu);
    }
    let width = out.len() / 4;
    if width % 2 == 1 {
        let x = width - 1;
        let (du, dv) = (u[x / 2] as i32 - 128, v[x / 2] as i32 - 128);
        put_px(
            &mut out[x * 4..x * 4 + 4],
            y[x],
            CRV * dv,
            CGU * du + CGV * dv,
            CBU * du,
        );
    }
}

/// Borrowed I420 planes with explicit strides (as produced by a decoder).
#[derive(Debug, Clone, Copy)]
pub struct I420Planes<'a> {
    pub width: usize,
    pub height: usize,
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
    pub y_stride: usize,
    pub uv_stride: usize,
}

impl<'a> From<&'a I420Buffer> for I420Planes<'a> {
    fn from(b: &'a I420Buffer) -> Self {
        Self {
            width: b.width,
            height: b.height,
            y: &b.y,
            u: &b.u,
            v: &b.v,
            y_stride: b.width,
            uv_stride: b.width / 2,
        }
    }
}

/// Converts I420 into `out` (resized to a tight-stride BGRA frame, alpha = 255).
pub fn i420_to_bgra(src: I420Planes<'_>, out: &mut BgraFrame) {
    let (w, h) = (src.width, src.height);
    out.width = w as u32;
    out.height = h as u32;
    out.stride = (w * 4) as u32;
    out.data.resize(w * h * 4, 0);
    if w == 0 || h == 0 {
        return;
    }
    let workers = worker_count(h);
    // Bands are whole row pairs so each band starts on a chroma row.
    let rows_per_band = h.div_ceil(workers).div_ceil(2) * 2;
    let convert_band = |first_row: usize, band: &mut [u8]| {
        for (k, row) in band.chunks_exact_mut(w * 4).enumerate() {
            let r = first_row + k;
            let cr = r / 2;
            let y = &src.y[r * src.y_stride..r * src.y_stride + w];
            let cw = w.div_ceil(2);
            let u = &src.u[cr * src.uv_stride..cr * src.uv_stride + cw];
            let v = &src.v[cr * src.uv_stride..cr * src.uv_stride + cw];
            i420_row(y, u, v, row);
        }
    };
    if workers == 1 {
        convert_band(0, &mut out.data);
        return;
    }
    thread::scope(|s| {
        for (i, band) in out.data.chunks_mut(rows_per_band * w * 4).enumerate() {
            let convert_band = &convert_band;
            s.spawn(move || convert_band(i * rows_per_band, band));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_from_fn(w: u32, h: u32, stride: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> BgraFrame {
        let mut fr = BgraFrame {
            width: w,
            height: h,
            stride,
            data: vec![0xEE; (stride * h) as usize],
            timestamp_us: 0,
        };
        for y in 0..h {
            let row = fr.row_mut(y);
            for x in 0..w {
                row[x as usize * 4..x as usize * 4 + 4].copy_from_slice(&f(x, y));
            }
        }
        fr
    }

    fn max_channel_error(a: &BgraFrame, b: &BgraFrame) -> u8 {
        let mut worst = 0;
        for y in 0..a.height {
            for (p, q) in a.row(y).chunks_exact(4).zip(b.row(y).chunks_exact(4)) {
                for c in 0..3 {
                    worst = worst.max(p[c].abs_diff(q[c]));
                }
            }
        }
        worst
    }

    #[test]
    fn known_colors_convert_to_bt709_limited() {
        let cases: [([u8; 4], (u8, u8, u8)); 5] = [
            ([0, 0, 0, 255], (16, 128, 128)),
            ([255, 255, 255, 255], (235, 128, 128)),
            ([0, 0, 255, 255], (63, 102, 240)), // red
            ([0, 255, 0, 255], (173, 42, 26)),  // green
            ([255, 0, 0, 255], (32, 240, 118)), // blue
        ];
        for (bgra, (ey, eu, ev)) in cases {
            let f = frame_from_fn(2, 2, 8, |_, _| bgra);
            let mut yuv = I420Buffer::default();
            bgra_to_i420(&f, &mut yuv);
            assert!(
                yuv.y[0].abs_diff(ey) <= 1,
                "{bgra:?}: Y {} vs {ey}",
                yuv.y[0]
            );
            assert!(
                yuv.u[0].abs_diff(eu) <= 1,
                "{bgra:?}: U {} vs {eu}",
                yuv.u[0]
            );
            assert!(
                yuv.v[0].abs_diff(ev) <= 1,
                "{bgra:?}: V {} vs {ev}",
                yuv.v[0]
            );
        }
    }

    #[test]
    fn flat_color_round_trip_within_tolerance() {
        // Flat 2x2 blocks lose nothing to chroma subsampling, so only rounding error remains.
        for &(b, g, r) in &[
            (0, 0, 0),
            (255, 255, 255),
            (10, 200, 30),
            (250, 5, 128),
            (90, 90, 91),
        ] {
            let src = frame_from_fn(64, 32, 64 * 4 + 32, |_, _| [b, g, r, 255]);
            let mut yuv = I420Buffer::default();
            bgra_to_i420(&src, &mut yuv);
            let mut back = BgraFrame::new_tight(0, 0);
            i420_to_bgra((&yuv).into(), &mut back);
            assert_eq!((back.width, back.height), (64, 32));
            let err = max_channel_error(&src, &back);
            assert!(err <= 3, "({b},{g},{r}) round-trip error {err}");
        }
    }

    #[test]
    fn gradient_round_trip_within_tolerance() {
        // Smooth gradient over a large (threaded) frame with padded stride.
        let src = frame_from_fn(1920, 1080, 1920 * 4 + 64, |x, y| {
            [(x / 8) as u8, (y / 5) as u8, ((x + y) / 12) as u8, 255]
        });
        let mut yuv = I420Buffer::default();
        bgra_to_i420(&src, &mut yuv);
        let mut back = BgraFrame::new_tight(0, 0);
        i420_to_bgra((&yuv).into(), &mut back);
        assert_eq!((back.width, back.height), (1920, 1080));
        let err = max_channel_error(&src, &back);
        assert!(err <= 6, "gradient round-trip error {err}");
    }

    #[test]
    fn odd_dimensions_are_padded_by_edge_duplication() {
        let src = frame_from_fn(5, 3, 5 * 4, |x, y| {
            [(x * 50) as u8, (y * 100) as u8, 0, 255]
        });
        let mut yuv = I420Buffer::default();
        bgra_to_i420(&src, &mut yuv);
        assert_eq!((yuv.width, yuv.height), (6, 4));
        assert_eq!(yuv.y.len(), 24);
        assert_eq!(yuv.u.len(), 6);
        // Padding column equals the last real column; padding row equals the last real row.
        for row in 0..4 {
            assert_eq!(yuv.y[row * 6 + 5], yuv.y[row * 6 + 4]);
        }
        assert_eq!(&yuv.y[3 * 6..4 * 6], &yuv.y[2 * 6..3 * 6]);
        // And it converts back at the padded size without panicking.
        let mut back = BgraFrame::new_tight(0, 0);
        i420_to_bgra((&yuv).into(), &mut back);
        assert_eq!((back.width, back.height), (6, 4));
    }

    #[test]
    fn half_conversion_equals_conversion_of_downsampled_image() {
        // A source made of flat 2x2 blocks halves exactly to the block colours. Width 1030 is not
        // a multiple of 4, so the clamped right-edge path is exercised too.
        let color = |bx: u32, by: u32| [(bx * 3) as u8, (by * 5) as u8, ((bx ^ by) * 7) as u8, 255];
        let (w, h) = (1030, 402);
        let big = frame_from_fn(w, h, w * 4 + 16, |x, y| color(x / 2, y / 2));
        let small = frame_from_fn(w / 2, h / 2, w / 2 * 4, color);
        let mut a = I420Buffer::default();
        let mut b = I420Buffer::default();
        bgra_to_i420_half(&big, &mut a);
        bgra_to_i420(&small, &mut b);
        assert_eq!((a.width, a.height), (516, 202));
        assert_eq!((b.width, b.height), (516, 202));
        let max_diff = |p: &[u8], q: &[u8]| p.iter().zip(q).map(|(x, y)| x.abs_diff(*y)).max();
        assert!(max_diff(&a.y, &b.y).unwrap() <= 1);
        assert!(max_diff(&a.u, &b.u).unwrap() <= 1);
        assert!(max_diff(&a.v, &b.v).unwrap() <= 1);
    }

    #[test]
    fn half_size_handles_odd_and_large_sources() {
        assert_eq!(half_size(3840, 2400), (1920, 1200));
        assert_eq!(half_size(7, 5), (4, 4));
        let src = frame_from_fn(7, 5, 28, |_, _| [10, 20, 30, 255]);
        let mut out = I420Buffer::default();
        bgra_to_i420_half(&src, &mut out);
        assert_eq!((out.width, out.height), (4, 4));
        let expect_y = luma(10, 20, 30);
        assert!(out.y.iter().all(|&y| y == expect_y));
    }

    #[test]
    fn nv12_interleaves_chroma_after_luma() {
        let src = I420Buffer {
            width: 4,
            height: 2,
            y: (0..8).collect(),
            u: vec![100, 101],
            v: vec![200, 201],
        };
        assert_eq!(nv12_len(&src), 12);
        let mut out = vec![0xEE; 13];
        assert!(i420_to_nv12(&src, &mut out));
        assert_eq!(
            out,
            vec![0, 1, 2, 3, 4, 5, 6, 7, 100, 200, 101, 201, 0xEE],
            "Y plane, then U/V pairs; bytes past nv12_len untouched"
        );
        // Too small a destination or a short plane is refused without writing.
        let mut small = vec![0; 11];
        assert!(!i420_to_nv12(&src, &mut small));
        assert!(small.iter().all(|&b| b == 0));
        let short = I420Buffer {
            u: vec![1],
            ..src.clone()
        };
        assert!(!i420_to_nv12(&short, &mut out));
    }

    #[test]
    fn odd_width_bgra_output() {
        // i420_to_bgra must handle an odd output width (decoders may crop to odd sizes).
        let yuv = I420Buffer {
            width: 6,
            height: 2,
            y: vec![235; 12],
            u: vec![128; 3],
            v: vec![128; 3],
        };
        let planes = I420Planes {
            width: 5,
            height: 2,
            ..(&yuv).into()
        };
        let mut out = BgraFrame::new_tight(0, 0);
        i420_to_bgra(planes, &mut out);
        assert_eq!((out.width, out.height, out.stride), (5, 2, 20));
        assert!(out.data.chunks_exact(4).all(|p| p == [255, 255, 255, 255]));
    }
}
