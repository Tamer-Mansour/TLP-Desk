//! Cursor compositing.
//!
//! The capturer excludes the mouse pointer and the protocol has no cursor message, so the host
//! draws the pointer into the BGRA frame before encoding. Frames may be smaller than the monitor
//! (the capturer downscales on the GPU); [`CursorOverlay::draw_scaled`] scales the pointer to
//! match.

use tlp_desk_capture::{BgraFrame, CursorShape, CursorState};

/// Remembers the last cursor position and shape reported by a capturer (the shape is only
/// sent when it changes) and draws it into frames.
#[derive(Debug, Clone, Default)]
pub struct CursorOverlay {
    shape: Option<CursorShape>,
    x: i32,
    y: i32,
    visible: bool,
    /// `shape` resized for frames smaller than the monitor, keyed by (frame w, h, monitor w, h).
    scaled: Option<((u32, u32, u32, u32), CursorShape)>,
}

/// Pixels of a frame saved before the cursor was drawn over them, so the clean frame can be
/// restored without keeping a full-frame copy.
#[derive(Debug, Clone, Default)]
pub struct CursorPatch {
    x0: u32,
    y0: u32,
    w: u32,
    h: u32,
    data: Vec<u8>,
}

impl CursorPatch {
    /// Puts the saved pixels back (no-op when nothing was saved) and empties the patch.
    pub fn restore(&mut self, frame: &mut BgraFrame) {
        let row_bytes = self.w as usize * 4;
        let fits = frame.width >= self.x0 + self.w && frame.height >= self.y0 + self.h;
        if row_bytes > 0 && fits {
            for r in 0..self.h {
                let dst = &mut frame.row_mut(self.y0 + r)
                    [self.x0 as usize * 4..(self.x0 + self.w) as usize * 4];
                dst.copy_from_slice(
                    &self.data[r as usize * row_bytes..(r as usize + 1) * row_bytes],
                );
            }
        }
        self.w = 0;
        self.h = 0;
    }

    fn save(&mut self, frame: &BgraFrame, (x0, y0, x1, y1): (u32, u32, u32, u32)) {
        self.x0 = x0;
        self.y0 = y0;
        self.w = x1 - x0;
        self.h = y1 - y0;
        self.data.clear();
        for y in y0..y1 {
            self.data
                .extend_from_slice(&frame.row(y)[x0 as usize * 4..x1 as usize * 4]);
        }
    }
}

impl CursorOverlay {
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies a new cursor poll. Returns `true` when the drawn result would change.
    pub fn update(&mut self, state: CursorState) -> bool {
        let mut changed = state.visible != self.visible;
        if state.visible && (state.x != self.x || state.y != self.y) {
            changed = true;
        }
        self.x = state.x;
        self.y = state.y;
        self.visible = state.visible;
        if let Some(shape) = state.shape {
            changed |= self.visible;
            self.shape = Some(shape);
            self.scaled = None;
        }
        changed
    }

    /// True when [`CursorOverlay::draw`] would modify a frame.
    pub fn is_drawable(&self) -> bool {
        self.visible && self.shape.is_some()
    }

    /// Draws the cursor (hotspot at the remembered position) into `frame`.
    pub fn draw(&self, frame: &mut BgraFrame) {
        if let (true, Some(shape)) = (self.visible, &self.shape) {
            composite_cursor(frame, shape, self.x, self.y);
        }
    }

    /// Draws the cursor into `frame`, which shows a `monitor`-sized screen, possibly downscaled
    /// (the capturer scales on the GPU): position, shape and hotspot are scaled by the same
    /// factor. The pixels underneath are saved into `patch` first so the caller can restore the
    /// clean frame afterwards. Returns false (and leaves `patch` empty) when nothing was drawn.
    pub fn draw_scaled(
        &mut self,
        frame: &mut BgraFrame,
        monitor: (u32, u32),
        patch: &mut CursorPatch,
    ) -> bool {
        patch.w = 0;
        patch.h = 0;
        if !self.visible {
            return false;
        }
        let Some(shape) = self.shape.as_ref() else {
            return false;
        };
        let (fw, fh) = (frame.width, frame.height);
        let (mw, mh) = (monitor.0.max(1), monitor.1.max(1));
        let (x, y, shape) = if (fw, fh) == (mw, mh) {
            (self.x, self.y, shape)
        } else {
            let key = (fw, fh, mw, mh);
            if self.scaled.as_ref().map(|(k, _)| *k) != Some(key) {
                self.scaled = Some((key, scale_shape(shape, (fw, fh), (mw, mh))));
            }
            let Some((_, scaled)) = self.scaled.as_ref() else {
                return false;
            };
            (
                scale_coord(self.x, fw, mw),
                scale_coord(self.y, fh, mh),
                scaled,
            )
        };
        let Some(rect) = cursor_rect(frame, shape, x, y) else {
            return false;
        };
        patch.save(frame, rect);
        composite_cursor(frame, shape, x, y);
        true
    }
}

fn scale_coord(v: i32, frame: u32, monitor: u32) -> i32 {
    (i64::from(v) * i64::from(frame)).div_euclid(i64::from(monitor.max(1))) as i32
}

/// Resizes a cursor shape by `frame / monitor` per axis with an alpha-weighted area average
/// (at least 1x1); the hotspot is scaled the same way.
pub fn scale_shape(shape: &CursorShape, frame: (u32, u32), monitor: (u32, u32)) -> CursorShape {
    let (sw, sh) = (shape.width as u64, shape.height as u64);
    let (fw, fh) = (frame.0 as u64, frame.1 as u64);
    let (mw, mh) = (monitor.0.max(1) as u64, monitor.1.max(1) as u64);
    if sw == 0 || sh == 0 || shape.rgba.len() < (sw * sh * 4) as usize {
        return shape.clone();
    }
    let w = ((sw * fw + mw / 2) / mw).max(1);
    let h = ((sh * fh + mh / 2) / mh).max(1);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for oy in 0..h {
        let y0 = oy * sh / h;
        let y1 = ((oy + 1) * sh).div_ceil(h).max(y0 + 1).min(sh);
        for ox in 0..w {
            let x0 = ox * sw / w;
            let x1 = ((ox + 1) * sw).div_ceil(w).max(x0 + 1).min(sw);
            let (mut r, mut g, mut b, mut a, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = ((y * sw + x) * 4) as usize;
                    let pa = shape.rgba[i + 3] as u64;
                    r += shape.rgba[i] as u64 * pa;
                    g += shape.rgba[i + 1] as u64 * pa;
                    b += shape.rgba[i + 2] as u64 * pa;
                    a += pa;
                    n += 1;
                }
            }
            let o = ((oy * w + ox) * 4) as usize;
            let avg = |sum: u64| (sum + a / 2).checked_div(a).unwrap_or(0) as u8;
            rgba[o] = avg(r);
            rgba[o + 1] = avg(g);
            rgba[o + 2] = avg(b);
            rgba[o + 3] = (a + n / 2).checked_div(n).unwrap_or(0) as u8;
        }
    }
    CursorShape {
        width: w as u32,
        height: h as u32,
        hotspot_x: (shape.hotspot_x as u64 * fw / mw).min(w - 1) as u32,
        hotspot_y: (shape.hotspot_y as u64 * fh / mh).min(h - 1) as u32,
        rgba,
    }
}

/// The clipped frame rectangle `(x0, y0, x1, y1)` the cursor covers, if any.
fn cursor_rect(
    frame: &BgraFrame,
    shape: &CursorShape,
    x: i32,
    y: i32,
) -> Option<(u32, u32, u32, u32)> {
    let (sw, sh) = (shape.width as i64, shape.height as i64);
    if sw == 0 || sh == 0 || shape.rgba.len() < (sw * sh * 4) as usize {
        return None;
    }
    let left = x as i64 - shape.hotspot_x as i64;
    let top = y as i64 - shape.hotspot_y as i64;
    let (fw, fh) = (frame.width as i64, frame.height as i64);
    let x0 = left.max(0);
    let y0 = top.max(0);
    let x1 = (left + sw).min(fw);
    let y1 = (top + sh).min(fh);
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    Some((x0 as u32, y0 as u32, x1 as u32, y1 as u32))
}

/// Alpha-blends the straight-alpha RGBA `shape` onto the BGRA `frame` so that the shape's
/// hotspot lands on pixel `(x, y)`. Parts outside the frame are clipped. The frame's alpha
/// channel is left untouched.
pub fn composite_cursor(frame: &mut BgraFrame, shape: &CursorShape, x: i32, y: i32) {
    let Some((x0, y0, x1, y1)) = cursor_rect(frame, shape, x, y) else {
        return;
    };
    let sw = shape.width as i64;
    let left = x as i64 - shape.hotspot_x as i64;
    let top = y as i64 - shape.hotspot_y as i64;
    for fy in y0 as i64..y1 as i64 {
        let sy = fy - top;
        let src_row = &shape.rgba[(sy * sw * 4) as usize..((sy + 1) * sw * 4) as usize];
        let dst_row = frame.row_mut(fy as u32);
        for fx in x0 as i64..x1 as i64 {
            let sx = (fx - left) as usize;
            let s = &src_row[sx * 4..sx * 4 + 4];
            let a = s[3] as u32;
            if a == 0 {
                continue;
            }
            let d = &mut dst_row[fx as usize * 4..fx as usize * 4 + 4];
            if a == 255 {
                d[0] = s[2];
                d[1] = s[1];
                d[2] = s[0];
                continue;
            }
            let inv = 255 - a;
            let blend = |src: u8, dst: u8| ((src as u32 * a + dst as u32 * inv + 127) / 255) as u8;
            d[0] = blend(s[2], d[0]);
            d[1] = blend(s[1], d[1]);
            d[2] = blend(s[0], d[2]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape_2x2() -> CursorShape {
        // Opaque red, half-transparent green / transparent, opaque blue.
        CursorShape {
            width: 2,
            height: 2,
            hotspot_x: 1,
            hotspot_y: 1,
            rgba: vec![
                255, 0, 0, 255, /**/ 0, 255, 0, 128, //
                0, 0, 0, 0, /*   */ 0, 0, 255, 255,
            ],
        }
    }

    fn px(f: &BgraFrame, x: u32, y: u32) -> [u8; 4] {
        let r = f.row(y);
        let i = x as usize * 4;
        [r[i], r[i + 1], r[i + 2], r[i + 3]]
    }

    #[test]
    fn cursor_is_blended_at_hotspot() {
        let mut f = BgraFrame::new_tight(4, 4);
        for b in f.data.iter_mut() {
            *b = 100;
        }
        composite_cursor(&mut f, &shape_2x2(), 2, 2);
        // Shape top-left lands at (1,1) because the hotspot is (1,1).
        assert_eq!(px(&f, 1, 1), [0, 0, 255, 100], "opaque red -> BGRA");
        // 50% green over grey 100: g = (255*128 + 100*127 + 127) / 255 = 178, b = r = 50.
        assert_eq!(px(&f, 2, 1), [50, 178, 50, 100]);
        assert_eq!(
            px(&f, 1, 2),
            [100, 100, 100, 100],
            "transparent pixel untouched"
        );
        assert_eq!(px(&f, 2, 2), [255, 0, 0, 100], "opaque blue -> BGRA");
        // Everything else untouched.
        assert_eq!(px(&f, 0, 0), [100; 4]);
        assert_eq!(px(&f, 3, 3), [100; 4]);
    }

    #[test]
    fn cursor_is_clipped_at_edges_and_respects_stride() {
        let mut f = BgraFrame {
            width: 2,
            height: 2,
            stride: 12, // 4 bytes of padding per row
            data: vec![0; 24],
            timestamp_us: 0,
        };
        // Hotspot at (0,0): only the shape's bottom-right pixel (blue) is inside.
        composite_cursor(&mut f, &shape_2x2(), 0, 0);
        assert_eq!(px(&f, 0, 0), [255, 0, 0, 0]);
        assert_eq!(px(&f, 1, 0), [0; 4]);
        // Far outside: nothing happens, no panic.
        composite_cursor(&mut f, &shape_2x2(), -50, 1000);
        composite_cursor(&mut f, &shape_2x2(), 3, 3);
        // Hotspot at (2,2) -> top-left pixel (red) at (1,1).
        composite_cursor(&mut f, &shape_2x2(), 2, 2);
        assert_eq!(px(&f, 1, 1), [0, 0, 255, 0]);
        // Padding bytes never written.
        assert_eq!(&f.data[8..12], &[0; 4]);
        assert_eq!(&f.data[20..24], &[0; 4]);
    }

    fn opaque_square(size: u32, hotspot: u32) -> CursorShape {
        CursorShape {
            width: size,
            height: size,
            hotspot_x: hotspot,
            hotspot_y: hotspot,
            rgba: [255, 0, 0, 255].repeat((size * size) as usize),
        }
    }

    #[test]
    fn shape_scales_with_hotspot_and_alpha() {
        let s = scale_shape(&opaque_square(32, 10), (1920, 1200), (3840, 2400));
        assert_eq!(
            (s.width, s.height, s.hotspot_x, s.hotspot_y),
            (16, 16, 5, 5)
        );
        assert!(s.rgba.chunks_exact(4).all(|p| p == [255, 0, 0, 255]));
        // Half-transparent checkerboard: half alpha, colour of the opaque pixels.
        let mut checker = opaque_square(2, 0);
        checker.rgba[4..8].copy_from_slice(&[0, 0, 0, 0]);
        checker.rgba[8..12].copy_from_slice(&[0, 0, 0, 0]);
        let s = scale_shape(&checker, (1, 1), (2, 2));
        assert_eq!((s.width, s.height), (1, 1));
        assert_eq!(s.rgba, vec![255, 0, 0, 128]);
        // Never below 1x1.
        let s = scale_shape(&opaque_square(2, 1), (100, 100), (1000, 1000));
        assert_eq!((s.width, s.height, s.hotspot_x), (1, 1, 0));
    }

    #[test]
    fn scaled_draw_lands_at_scaled_position_and_restores() {
        // A 384x240 monitor shown as a 192x120 frame (same 2:1 ratio as 3840x2400 -> 1920x1200).
        let mut f = BgraFrame::new_tight(192, 120);
        for b in f.data.iter_mut() {
            *b = 7;
        }
        let clean = f.clone();
        let mut o = CursorOverlay::new();
        o.update(CursorState {
            x: 200,
            y: 100,
            visible: true,
            shape: Some(opaque_square(8, 2)),
        });
        let mut patch = CursorPatch::default();
        assert!(o.draw_scaled(&mut f, (384, 240), &mut patch));
        // Hotspot (2,2)->(1,1); position (200,100)->(100,50); 8x8 -> 4x4 over (99,49)..(102,52).
        assert_eq!(px(&f, 99, 49), [0, 0, 255, 7]);
        assert_eq!(px(&f, 102, 52), [0, 0, 255, 7]);
        assert_eq!(px(&f, 103, 53), [7; 4]);
        assert_eq!(px(&f, 98, 48), [7; 4]);
        patch.restore(&mut f);
        assert_eq!(f, clean, "restore must give back the clean frame");
        // Frame the size of the monitor: unscaled drawing, identical to composite_cursor.
        let mut a = BgraFrame::new_tight(256, 128);
        let mut b = a.clone();
        assert!(o.draw_scaled(&mut a, (256, 128), &mut patch));
        composite_cursor(&mut b, &opaque_square(8, 2), 200, 100);
        assert_eq!(a, b);
        // Hidden: nothing drawn, an empty patch restores nothing.
        o.update(CursorState {
            x: 1,
            y: 1,
            visible: false,
            shape: None,
        });
        let mut c = clean.clone();
        assert!(!o.draw_scaled(&mut c, (384, 240), &mut patch));
        patch.restore(&mut c);
        assert_eq!(c, clean);
    }

    #[test]
    fn overlay_remembers_shape_between_polls() {
        let mut o = CursorOverlay::new();
        assert!(!o.is_drawable());
        let first = CursorState {
            x: 2,
            y: 2,
            visible: true,
            shape: Some(shape_2x2()),
        };
        assert!(o.update(first));
        // Same position, no shape: nothing changed, still drawable with the old shape.
        let same = CursorState {
            x: 2,
            y: 2,
            visible: true,
            shape: None,
        };
        assert!(!o.update(same.clone()));
        assert!(o.is_drawable());
        let mut f = BgraFrame::new_tight(4, 4);
        o.draw(&mut f);
        assert_eq!(px(&f, 2, 2), [255, 0, 0, 0]);
        // Moving changes; hiding changes; moving while hidden does not.
        assert!(o.update(CursorState {
            x: 3,
            ..same.clone()
        }));
        assert!(o.update(CursorState {
            visible: false,
            ..same.clone()
        }));
        assert!(!o.update(CursorState {
            x: 0,
            visible: false,
            ..same
        }));
        assert!(!o.is_drawable());
    }
}
