//! Thumbnails of a remote screen: a decoded RGBA frame shrunk to fit [`MAX_W`] x [`MAX_H`]
//! with a box filter (every source pixel counts once, so text and gradients average instead
//! of aliasing). Pure functions; the UI calls [`downscale`] at most every few seconds.

/// Thumbnail size cap (16:9 at 384 wide; other aspect ratios fit inside).
pub const MAX_W: usize = 384;
pub const MAX_H: usize = 216;
/// Largest frame accepted (8K); bigger frames are a bug or hostile and are ignored.
const MAX_SRC_PIXELS: usize = 7_680 * 4_320;

/// A small RGBA image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumb {
    pub width: usize,
    pub height: usize,
    /// `width * height * 4` bytes, RGBA, opaque.
    pub rgba: Vec<u8>,
}

impl Thumb {
    /// Builds a thumbnail from raw bytes; `None` if the size and the data disagree or the
    /// image is empty or larger than the cap.
    pub fn from_rgba(width: usize, height: usize, rgba: Vec<u8>) -> Option<Self> {
        let ok = width > 0
            && height > 0
            && width <= MAX_W
            && height <= MAX_H
            && rgba.len() == width.checked_mul(height)?.checked_mul(4)?;
        ok.then_some(Self {
            width,
            height,
            rgba,
        })
    }
}

/// Shrinks a `w` x `h` RGBA frame to fit the cap, keeping its aspect ratio. A frame that
/// already fits is copied as is. `None` for a malformed frame.
pub fn downscale(rgba: &[u8], w: usize, h: usize) -> Option<Thumb> {
    let pixels = w.checked_mul(h)?;
    if w == 0 || h == 0 || pixels > MAX_SRC_PIXELS || rgba.len() != pixels.checked_mul(4)? {
        return None;
    }
    // Integer shrink factor so every output pixel averages a whole block.
    let factor = w.div_ceil(MAX_W).max(h.div_ceil(MAX_H)).max(1);
    let ow = (w / factor).max(1);
    let oh = (h / factor).max(1);
    let mut out = Vec::with_capacity(ow * oh * 4);
    let block = (factor * factor) as u64;
    for oy in 0..oh {
        for ox in 0..ow {
            let mut sum = [0u64; 3];
            for sy in oy * factor..(oy + 1) * factor {
                let row = sy * w * 4;
                for sx in ox * factor..(ox + 1) * factor {
                    let i = row + sx * 4;
                    if let Some(px) = rgba.get(i..i + 3) {
                        sum[0] += u64::from(px[0]);
                        sum[1] += u64::from(px[1]);
                        sum[2] += u64::from(px[2]);
                    }
                }
            }
            for c in sum {
                out.push(u8::try_from(c / block).unwrap_or(u8::MAX));
            }
            out.push(0xFF);
        }
    }
    Thumb::from_rgba(ow, oh, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_1080p_frame_shrinks_to_the_cap_and_averages_blocks() {
        let (w, h) = (1920, 1080);
        let mut rgba = vec![0u8; w * h * 4];
        // Left half red, right half blue.
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                if x < w / 2 {
                    rgba[i] = 255;
                } else {
                    rgba[i + 2] = 255;
                }
                rgba[i + 3] = 255;
            }
        }
        let t = downscale(&rgba, w, h).unwrap();
        assert_eq!((t.width, t.height), (384, 216));
        assert_eq!(&t.rgba[0..4], &[255, 0, 0, 255]);
        let last = t.rgba.len() - 4;
        assert_eq!(&t.rgba[last..], &[0, 0, 255, 255]);
    }

    #[test]
    fn small_frames_are_kept_and_bad_frames_rejected() {
        let t = downscale(&[1, 2, 3, 255, 4, 5, 6, 255], 2, 1).unwrap();
        assert_eq!((t.width, t.height), (2, 1));
        assert_eq!(t.rgba, [1, 2, 3, 255, 4, 5, 6, 255]);
        assert!(downscale(&[0; 8], 3, 1).is_none(), "size mismatch");
        assert!(downscale(&[], 0, 0).is_none());
        assert!(Thumb::from_rgba(MAX_W + 1, 1, vec![0; (MAX_W + 1) * 4]).is_none());
    }

    #[test]
    fn odd_sizes_round_down_without_panicking() {
        let (w, h) = (1366, 768);
        let t = downscale(&vec![9; w * h * 4], w, h).unwrap();
        assert!(t.width <= MAX_W && t.height <= MAX_H);
        assert_eq!(t.rgba.len(), t.width * t.height * 4);
        assert!(t.rgba.iter().step_by(4).all(|b| *b == 9));
    }
}
