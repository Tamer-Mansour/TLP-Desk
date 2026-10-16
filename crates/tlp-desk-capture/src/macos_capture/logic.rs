//! Pure helpers of the macOS capturer (compiled and tested on every platform).

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use crate::{CursorShape, MonitorInfo};

/// One display as the OS reports it: id, origin and size in global display **points**.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawDisplay {
    /// `CGDirectDisplayID`.
    pub id: u32,
    /// Left edge in points (the main display is at 0,0).
    pub x: i32,
    /// Top edge in points.
    pub y: i32,
    /// Width in points.
    pub width: u32,
    /// Height in points.
    pub height: u32,
}

/// Builds the monitor list: the main display first (index 0), the rest left-to-right then
/// top-to-bottom, as the `list_monitors` contract requires.
pub fn order_displays(mut displays: Vec<RawDisplay>, main_id: u32) -> Vec<(u32, MonitorInfo)> {
    displays.sort_by_key(|d| (d.id != main_id, d.x, d.y, d.id));
    displays
        .into_iter()
        .enumerate()
        .map(|(i, d)| {
            let index = u32::try_from(i).unwrap_or(u32::MAX);
            (
                d.id,
                MonitorInfo {
                    index,
                    name: format!("Display {}", d.id),
                    width: d.width,
                    height: d.height,
                    primary: d.id == main_id,
                    x: d.x,
                    y: d.y,
                },
            )
        })
        .collect()
}

/// Copies `src` (`src_w` x `src_h` BGRA pixels, row pitch `src_stride`) into a tight
/// `dst_w` x `dst_h` buffer by nearest-neighbour sampling. Returns `None` when the geometry is
/// inconsistent with `src.len()` (never indexes out of range on hostile sizes).
pub fn resample_nearest(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    src_stride: u32,
    dst_w: u32,
    dst_h: u32,
) -> Option<Vec<u8>> {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 || src_stride < src_w.checked_mul(4)? {
        return None;
    }
    let need = (src_h as usize - 1)
        .checked_mul(src_stride as usize)?
        .checked_add(src_w as usize * 4)?;
    if src.len() < need {
        return None;
    }
    let mut out = Vec::with_capacity(
        (dst_w as usize)
            .checked_mul(dst_h as usize)?
            .checked_mul(4)?,
    );
    for dy in 0..dst_h {
        let sy = (u64::from(dy) * u64::from(src_h) / u64::from(dst_h)) as usize;
        let row = sy * src_stride as usize;
        for dx in 0..dst_w {
            let sx = (u64::from(dx) * u64::from(src_w) / u64::from(dst_w)) as usize;
            let p = row + sx * 4;
            out.extend_from_slice(src.get(p..p + 4)?);
        }
    }
    Some(out)
}

/// Converts a pointer position in points (relative to the display) to frame pixels.
pub fn points_to_pixels(p: f64, points: u32, pixels: u32) -> i32 {
    if points == 0 || !p.is_finite() {
        return 0;
    }
    (p * f64::from(pixels) / f64::from(points)).round() as i32
}

/// A small white arrow with a black outline, sent once as the cursor image because
/// `CGDisplayCreateImage` does not composite the system cursor into the frame.
pub fn arrow_cursor() -> CursorShape {
    const W: u32 = 12;
    const H: u32 = 19;
    let mut rgba = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        // Triangle that widens down to row 12, then a stem.
        let fill = if y < 13 { y / 2 + 1 } else { 3 };
        let start = if y < 13 { 0 } else { 2 };
        for x in 0..W {
            let inside = x >= start && x < start + fill;
            let edge = inside && (x == start || x + 1 == start + fill || y == 0 || y == H - 1);
            let idx = ((y * W + x) * 4) as usize;
            if edge {
                rgba[idx..idx + 4].copy_from_slice(&[0, 0, 0, 255]);
            } else if inside {
                rgba[idx..idx + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }
    CursorShape {
        width: W,
        height: H,
        hotspot_x: 0,
        hotspot_y: 0,
        rgba,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(id: u32, x: i32, y: i32) -> RawDisplay {
        RawDisplay {
            id,
            x,
            y,
            width: 1440,
            height: 900,
        }
    }

    #[test]
    fn main_display_is_index_zero_then_left_to_right() {
        let list = order_displays(vec![d(7, 1440, 0), d(3, -1440, 0), d(5, 0, 0)], 5);
        let ids: Vec<u32> = list.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![5, 3, 7]);
        assert!(list[0].1.primary && !list[1].1.primary);
        assert_eq!(list[2].1.index, 2);
        assert_eq!(list[1].1.x, -1440);
    }

    #[test]
    fn resample_halves_a_retina_frame() {
        // 4x2 source; row pitch 20 (padded); pixel value = x + 10*y in every byte.
        let mut src = vec![0u8; 20 + 16];
        for y in 0..2usize {
            for x in 0..4usize {
                let v = (x + 10 * y) as u8;
                src[y * 20 + x * 4..y * 20 + x * 4 + 4].fill(v);
            }
        }
        let out = resample_nearest(&src, 4, 2, 20, 2, 1).unwrap_or_default();
        assert_eq!(out, vec![0, 0, 0, 0, 2, 2, 2, 2]);
    }

    #[test]
    fn resample_rejects_bad_geometry() {
        assert!(resample_nearest(&[0; 16], 4, 2, 16, 2, 2).is_none()); // too short
        assert!(resample_nearest(&[0; 64], 4, 2, 8, 2, 2).is_none()); // stride < width*4
        assert!(resample_nearest(&[0; 64], 0, 2, 16, 2, 2).is_none());
        assert!(resample_nearest(&[0; 64], 4, 2, 16, 0, 2).is_none());
        assert!(resample_nearest(&[], u32::MAX, u32::MAX, u32::MAX, 2, 2).is_none());
    }

    #[test]
    fn points_scale_to_pixels() {
        assert_eq!(points_to_pixels(100.0, 1440, 2880), 200);
        assert_eq!(points_to_pixels(0.4, 1440, 1440), 0);
        assert_eq!(points_to_pixels(f64::NAN, 1440, 2880), 0);
        assert_eq!(points_to_pixels(5.0, 0, 2880), 0);
    }

    #[test]
    fn arrow_cursor_is_well_formed() {
        let c = arrow_cursor();
        assert_eq!(c.rgba.len(), (c.width * c.height * 4) as usize);
        assert!(c.rgba.chunks(4).any(|p| p == [255, 255, 255, 255]));
        assert!(c.rgba.chunks(4).any(|p| p == [0, 0, 0, 255]));
    }
}
