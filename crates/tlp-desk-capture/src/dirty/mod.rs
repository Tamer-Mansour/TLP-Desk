//! Static-screen and dirty-region detection on captured frames.
//!
//! Capture APIs report a new frame for many reasons that do not change a single pixel of the
//! image we stream (a window repainting identical content, a composition tick, a cursor-only
//! update). [`DirtyTracker`] hashes every tile of each frame and compares the hashes with the
//! previous frame, so the host can skip encoding (and sending) frames that are identical. It is
//! exact for practical purposes: every byte of the visible image is hashed (a one-pixel text
//! caret blink is detected), with a 64-bit hash per tile.
//!
//! Cost: one pass over the frame, about the speed of a `memcmp` (~1 ms for 1920x1200 in a
//! release build), on the capture thread that otherwise waits for the next frame, and no copy
//! of the previous frame is kept.

use crate::BgraFrame;

/// Default tile edge in pixels. 64x64 tiles keep the hash table small (600 tiles for
/// 1920x1200) while still giving a useful dirty rectangle.
pub const DEFAULT_TILE: u32 = 64;

/// A rectangle in frame pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DirtyRect {
    /// Left edge, pixels.
    pub x: u32,
    /// Top edge, pixels.
    pub y: u32,
    /// Width, pixels.
    pub width: u32,
    /// Height, pixels.
    pub height: u32,
}

/// What changed in a frame compared with the previous one given to [`DirtyTracker::update`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameChange {
    /// No previous frame of this size (first frame, after [`DirtyTracker::reset`] or a size
    /// change): everything counts as changed.
    First,
    /// Every pixel is identical to the previous frame: nothing needs to be encoded.
    Unchanged,
    /// Some tiles changed.
    Changed {
        /// Number of changed tiles.
        tiles: u32,
        /// Total number of tiles in the frame.
        total: u32,
        /// Bounding box of the changed tiles (clipped to the frame).
        bounds: DirtyRect,
    },
}

impl FrameChange {
    /// True unless the frame is identical to the previous one.
    pub fn is_changed(&self) -> bool {
        !matches!(self, FrameChange::Unchanged)
    }
}

/// Remembers per-tile hashes of the last frame (see the module docs).
#[derive(Debug, Clone)]
pub struct DirtyTracker {
    tile: u32,
    size: (u32, u32),
    hashes: Vec<u64>,
    scratch: Vec<u64>,
}

impl Default for DirtyTracker {
    fn default() -> Self {
        Self::new(DEFAULT_TILE)
    }
}

const SEED: u64 = 0x9E37_79B9_7F4A_7C15;
const MUL: u64 = 0xA076_1D64_78BD_642F;

#[inline]
fn mix(h: u64, v: u64) -> u64 {
    (h ^ v).wrapping_mul(MUL).rotate_left(29)
}

/// Hashes `bytes` into `h` (8 bytes at a time, then the tail).
#[inline]
fn hash_bytes(mut h: u64, bytes: &[u8]) -> u64 {
    let mut chunks = bytes.chunks_exact(8);
    for c in &mut chunks {
        let mut b = [0u8; 8];
        b.copy_from_slice(c);
        h = mix(h, u64::from_le_bytes(b));
    }
    let rest = chunks.remainder();
    if !rest.is_empty() {
        let mut b = [0u8; 8];
        b[..rest.len()].copy_from_slice(rest);
        h = mix(h, u64::from_le_bytes(b) ^ ((rest.len() as u64) << 56));
    }
    h
}

impl DirtyTracker {
    /// A tracker with `tile` x `tile` pixel tiles (at least 8).
    pub fn new(tile: u32) -> Self {
        Self {
            tile: tile.max(8),
            size: (0, 0),
            hashes: Vec::new(),
            scratch: Vec::new(),
        }
    }

    /// Forgets the previous frame: the next [`DirtyTracker::update`] returns
    /// [`FrameChange::First`]. Call it when the frame source changes (new monitor, new size cap).
    pub fn reset(&mut self) {
        self.size = (0, 0);
        self.hashes.clear();
    }

    /// Compares `frame` with the previous frame and remembers it for the next call.
    ///
    /// A malformed frame (buffer too short for its geometry) is reported as
    /// [`FrameChange::First`] and resets the tracker, so it is never wrongly skipped.
    pub fn update(&mut self, frame: &BgraFrame) -> FrameChange {
        if !frame.is_well_formed() || frame.width == 0 || frame.height == 0 {
            self.reset();
            return FrameChange::First;
        }
        let tile = self.tile;
        let cols = frame.width.div_ceil(tile);
        let rows = frame.height.div_ceil(tile);
        let total = cols.saturating_mul(rows);
        self.scratch.clear();
        self.scratch.resize(total as usize, SEED);
        let row_bytes = frame.width as usize * 4;
        let tile_bytes = tile as usize * 4;
        for y in 0..frame.height {
            let start = y as usize * frame.stride as usize;
            let Some(row) = frame.data.get(start..start + row_bytes) else {
                self.reset();
                return FrameChange::First;
            };
            let base = (y / tile) as usize * cols as usize;
            for (tx, seg) in row.chunks(tile_bytes).enumerate() {
                if let Some(h) = self.scratch.get_mut(base + tx) {
                    *h = hash_bytes(*h, seg);
                }
            }
        }
        let first = self.size != (frame.width, frame.height) || self.hashes.len() != total as usize;
        std::mem::swap(&mut self.hashes, &mut self.scratch);
        self.size = (frame.width, frame.height);
        if first {
            return FrameChange::First;
        }
        let mut changed = 0u32;
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
        for (i, (a, b)) in self.hashes.iter().zip(&self.scratch).enumerate() {
            if a != b {
                changed += 1;
                let i = i as u32;
                let (tx, ty) = (i % cols, i / cols);
                x0 = x0.min(tx);
                y0 = y0.min(ty);
                x1 = x1.max(tx + 1);
                y1 = y1.max(ty + 1);
            }
        }
        if changed == 0 {
            return FrameChange::Unchanged;
        }
        let px = |t: u32, extent: u32| t.saturating_mul(tile).min(extent);
        let (bx, by) = (px(x0, frame.width), px(y0, frame.height));
        FrameChange::Changed {
            tiles: changed,
            total,
            bounds: DirtyRect {
                x: bx,
                y: by,
                width: px(x1, frame.width) - bx,
                height: px(y1, frame.height) - by,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32, stride_pad: u32, seed: u8) -> BgraFrame {
        let stride = w * 4 + stride_pad;
        let mut f = BgraFrame {
            width: w,
            height: h,
            stride,
            data: vec![0xEE; (stride * h) as usize],
            timestamp_us: 0,
        };
        for y in 0..h {
            for (x, px) in f.row_mut(y).chunks_exact_mut(4).enumerate() {
                let v = (x as u32 * 7 + y * 13) as u8 ^ seed;
                px.copy_from_slice(&[v, v.wrapping_add(1), v.wrapping_add(2), 255]);
            }
        }
        f
    }

    fn set_px(f: &mut BgraFrame, x: u32, y: u32, v: [u8; 4]) {
        let i = x as usize * 4;
        f.row_mut(y)[i..i + 4].copy_from_slice(&v);
    }

    #[test]
    fn identical_frames_are_unchanged() {
        let mut t = DirtyTracker::default();
        let f = frame(200, 150, 0, 1);
        assert_eq!(t.update(&f), FrameChange::First);
        assert_eq!(t.update(&f), FrameChange::Unchanged);
        assert_eq!(t.update(&f.clone()), FrameChange::Unchanged);
    }

    #[test]
    fn single_pixel_caret_blink_is_detected() {
        let mut t = DirtyTracker::new(64);
        let mut f = frame(1920, 1200, 0, 3);
        t.update(&f);
        // A one-pixel-wide caret at (1000, 600).
        for y in 600..616 {
            set_px(&mut f, 1000, y, [0, 0, 0, 255]);
        }
        match t.update(&f) {
            FrameChange::Changed {
                tiles,
                total,
                bounds,
            } => {
                assert_eq!(total, 30 * 19);
                assert_eq!(tiles, 1);
                assert_eq!(
                    bounds,
                    DirtyRect {
                        x: 960,
                        y: 576,
                        width: 64,
                        height: 64
                    }
                );
            }
            other => panic!("caret not detected: {other:?}"),
        }
        // Blinking back restores the old content: a change again, then static.
        assert!(t.update(&frame(1920, 1200, 0, 3)).is_changed());
        assert_eq!(t.update(&frame(1920, 1200, 0, 3)), FrameChange::Unchanged);
    }

    #[test]
    fn bounds_cover_all_changes_and_clip_to_frame() {
        let mut t = DirtyTracker::new(64);
        let mut f = frame(130, 100, 12, 0);
        t.update(&f);
        set_px(&mut f, 0, 0, [1, 2, 3, 4]);
        set_px(&mut f, 129, 99, [1, 2, 3, 4]);
        match t.update(&f) {
            FrameChange::Changed { tiles, bounds, .. } => {
                assert_eq!(tiles, 2);
                assert_eq!(
                    bounds,
                    DirtyRect {
                        x: 0,
                        y: 0,
                        width: 130,
                        height: 100
                    }
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn stride_padding_is_ignored() {
        let mut t = DirtyTracker::default();
        let mut f = frame(100, 80, 16, 9);
        t.update(&f);
        // Scribble on the padding bytes only.
        let stride = f.stride as usize;
        for y in 0..80usize {
            f.data[y * stride + 400] ^= 0xFF;
        }
        assert_eq!(t.update(&f), FrameChange::Unchanged);
    }

    #[test]
    fn size_change_and_reset_report_first() {
        let mut t = DirtyTracker::default();
        t.update(&frame(64, 64, 0, 0));
        assert_eq!(t.update(&frame(66, 64, 0, 0)), FrameChange::First);
        assert_eq!(t.update(&frame(66, 64, 0, 0)), FrameChange::Unchanged);
        t.reset();
        assert_eq!(t.update(&frame(66, 64, 0, 0)), FrameChange::First);
    }

    #[test]
    fn malformed_and_empty_frames_never_panic_and_never_skip() {
        let mut t = DirtyTracker::default();
        let bad = BgraFrame {
            width: 100,
            height: 100,
            stride: 400,
            data: vec![0; 10],
            timestamp_us: 0,
        };
        assert_eq!(t.update(&bad), FrameChange::First);
        assert_eq!(t.update(&bad), FrameChange::First);
        let empty = BgraFrame::new_tight(0, 0);
        assert_eq!(t.update(&empty), FrameChange::First);
        // Tiny tiles are raised to the minimum.
        let mut t = DirtyTracker::new(0);
        t.update(&frame(9, 9, 0, 0));
        assert_eq!(t.update(&frame(9, 9, 0, 0)), FrameChange::Unchanged);
    }
}
