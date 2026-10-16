//! Pure helpers of the X11 capture backend: buffer arithmetic, monitor ordering, cursor
//! conversion and the CPU downscaler. No X calls, so they are unit tested on every platform.

use crate::{BgraFrame, MonitorInfo};

/// Upper bound of one `GetImage` reply. Capturing in horizontal bands keeps each reply (and the
/// server-side copy) small and lets all band requests be pipelined.
pub(crate) const MAX_BAND_BYTES: usize = 4 * 1024 * 1024;

/// Number of rows per `GetImage` band for a `width`-pixel-wide 32-bit image: at least 1, and
/// at most `height`. A zero width yields the full height (nothing to transfer).
pub(crate) fn band_rows(width: u32, height: u32, max_bytes: usize) -> u32 {
    let row = (width as usize).saturating_mul(4);
    if row == 0 {
        return height.max(1);
    }
    let rows = (max_bytes / row).clamp(1, height.max(1) as usize);
    u32::try_from(rows).unwrap_or(height.max(1))
}

/// Splits `height` rows into `(first_row, rows)` bands of at most `rows_per_band` rows.
pub(crate) fn bands(height: u32, rows_per_band: u32) -> Vec<(u32, u32)> {
    let step = rows_per_band.max(1);
    let mut out = Vec::new();
    let mut y = 0u32;
    while y < height {
        let n = step.min(height - y);
        out.push((y, n));
        y += n;
    }
    out
}

/// Whether a visual/pixmap format is the 32-bit little-endian `0xXXRRGGBB` layout whose bytes
/// are already BGRA order. Anything else (16-bit, big-endian, exotic masks) is rejected rather
/// than captured wrongly.
pub(crate) fn is_bgrx32(
    bits_per_pixel: u8,
    lsb_first: bool,
    red: u32,
    green: u32,
    blue: u32,
) -> bool {
    bits_per_pixel == 32 && lsb_first && red == 0x00FF_0000 && green == 0x0000_FF00 && blue == 0xFF
}

/// Copies one band of `GetImage` data (rows of `width * 4` bytes, tightly packed) into `dst` at
/// row `first_row`, forcing the undefined padding byte to opaque alpha. Returns `false` when
/// `data` is too short or the band does not fit in `dst`.
pub(crate) fn copy_band(dst: &mut BgraFrame, first_row: u32, rows: u32, data: &[u8]) -> bool {
    let row_bytes = dst.width as usize * 4;
    let Some(need) = row_bytes.checked_mul(rows as usize) else {
        return false;
    };
    if data.len() < need || first_row.checked_add(rows).is_none_or(|e| e > dst.height) {
        return false;
    }
    for r in 0..rows {
        let src = &data[r as usize * row_bytes..(r as usize + 1) * row_bytes];
        let out = dst.row_mut(first_row + r);
        out.copy_from_slice(src);
        for px in out.chunks_exact_mut(4) {
            if let Some(a) = px.get_mut(3) {
                *a = 0xFF;
            }
        }
    }
    true
}

/// Orders monitors the way [`crate::list_monitors`] promises: primary first, the rest
/// left-to-right then top-to-bottom, and renumbers `index`. The first monitor becomes primary
/// when none is flagged.
pub(crate) fn order_monitors(mut monitors: Vec<MonitorInfo>) -> Vec<MonitorInfo> {
    monitors.sort_by_key(|m| (!m.primary, m.x, m.y));
    if let Some(first) = monitors.first_mut() {
        first.primary = true;
    }
    for (i, m) in monitors.iter_mut().enumerate() {
        m.index = u32::try_from(i).unwrap_or(u32::MAX);
    }
    monitors
}

/// Converts an XFixes cursor image (premultiplied `0xAARRGGBB` words) to straight RGBA8.
pub(crate) fn argb_premul_to_rgba(argb: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(argb.len().saturating_mul(4));
    for &p in argb {
        let a = (p >> 24) & 0xFF;
        let un = |c: u32| -> u8 {
            (c * 255 + a / 2)
                .checked_div(a)
                .map_or(0, |v| v.min(255) as u8)
        };
        out.extend_from_slice(&[
            un((p >> 16) & 0xFF),
            un((p >> 8) & 0xFF),
            un(p & 0xFF),
            a as u8,
        ]);
    }
    out
}

/// Area-average downscale of `src` into `dst` (both BGRA). `dst` must already have the target
/// size; each output pixel is the mean of the source rectangle it covers.
pub(crate) fn downscale_area(src: &BgraFrame, dst: &mut BgraFrame) {
    let (sw, sh, dw, dh) = (src.width, src.height, dst.width, dst.height);
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return;
    }
    let mut sums = vec![0u32; dw as usize * 4];
    let mut counts = vec![0u32; dw as usize];
    for oy in 0..dh {
        let y0 = (u64::from(oy) * u64::from(sh) / u64::from(dh)) as u32;
        let y1 = (((u64::from(oy) + 1) * u64::from(sh)).div_ceil(u64::from(dh)) as u32)
            .max(y0 + 1)
            .min(sh);
        sums.fill(0);
        counts.fill(0);
        for y in y0..y1 {
            let row = src.row(y);
            for sx in 0..sw {
                let ox = (u64::from(sx) * u64::from(dw) / u64::from(sw)) as usize;
                let px = &row[sx as usize * 4..sx as usize * 4 + 4];
                for (c, v) in px.iter().enumerate() {
                    sums[ox * 4 + c] += u32::from(*v);
                }
                counts[ox] += 1;
            }
        }
        let out = dst.row_mut(oy);
        for ox in 0..dw as usize {
            let n = counts[ox].max(1);
            for c in 0..4 {
                out[ox * 4 + c] = (sums[ox * 4 + c] / n) as u8;
            }
        }
    }
}

/// Which kind of desktop session the process runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionKind {
    /// An X server is reachable and the session is not Wayland.
    X11,
    /// A Wayland session: the X root window (XWayland) does not show native Wayland windows.
    Wayland,
    /// Neither `DISPLAY` nor `WAYLAND_DISPLAY` is set.
    NoDisplay,
}

/// Classifies the session from the relevant environment values.
pub(crate) fn classify_session(
    xdg_session_type: Option<&str>,
    display: bool,
    wayland_display: bool,
) -> SessionKind {
    let wayland = xdg_session_type.is_some_and(|t| t.eq_ignore_ascii_case("wayland"))
        || (wayland_display && !display);
    if wayland {
        SessionKind::Wayland
    } else if display {
        SessionKind::X11
    } else {
        SessionKind::NoDisplay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(index: u32, name: &str, primary: bool, x: i32, y: i32) -> MonitorInfo {
        MonitorInfo {
            index,
            name: name.into(),
            width: 1920,
            height: 1080,
            primary,
            x,
            y,
        }
    }

    #[test]
    fn band_rows_bounds() {
        assert_eq!(band_rows(1920, 1080, MAX_BAND_BYTES), 546);
        assert_eq!(band_rows(1920, 100, MAX_BAND_BYTES), 100);
        // A row larger than the budget still moves one row at a time.
        assert_eq!(band_rows(10_000_000, 50, MAX_BAND_BYTES), 1);
        assert_eq!(band_rows(0, 50, MAX_BAND_BYTES), 50);
        assert_eq!(band_rows(1920, 0, MAX_BAND_BYTES), 1);
    }

    #[test]
    fn bands_cover_every_row_once() {
        for (h, step) in [(1080u32, 546u32), (7, 3), (5, 5), (5, 100), (1, 1), (0, 4)] {
            let b = bands(h, step);
            assert_eq!(b.iter().map(|x| x.1).sum::<u32>(), h);
            let mut next = 0;
            for (y, n) in b {
                assert_eq!(y, next);
                assert!(n >= 1 && n <= step.max(1));
                next = y + n;
            }
        }
    }

    #[test]
    fn pixel_format_check() {
        assert!(is_bgrx32(32, true, 0xFF0000, 0xFF00, 0xFF));
        assert!(!is_bgrx32(16, true, 0xF800, 0x7E0, 0x1F));
        assert!(!is_bgrx32(32, false, 0xFF0000, 0xFF00, 0xFF));
        assert!(!is_bgrx32(32, true, 0xFF, 0xFF00, 0xFF0000));
    }

    #[test]
    fn copy_band_sets_alpha_and_checks_bounds() {
        let mut f = BgraFrame::new_tight(2, 3);
        let data = [1, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0, 10, 11, 12, 0];
        assert!(copy_band(&mut f, 1, 2, &data));
        assert_eq!(f.row(0), &[0; 8]);
        assert_eq!(f.row(1), &[1, 2, 3, 255, 4, 5, 6, 255]);
        assert_eq!(f.row(2), &[7, 8, 9, 255, 10, 11, 12, 255]);
        assert!(!copy_band(&mut f, 2, 2, &data), "band past the last row");
        assert!(!copy_band(&mut f, 0, 3, &data), "short data");
        assert!(!copy_band(&mut f, u32::MAX, 2, &data), "overflowing offset");
    }

    #[test]
    fn copy_band_respects_destination_stride() {
        let mut f = BgraFrame {
            width: 1,
            height: 2,
            stride: 8,
            data: vec![0; 16],
            timestamp_us: 0,
        };
        assert!(copy_band(&mut f, 0, 2, &[1, 2, 3, 0, 5, 6, 7, 0]));
        assert_eq!(&f.data[..4], &[1, 2, 3, 255]);
        assert_eq!(&f.data[4..8], &[0; 4]);
        assert_eq!(&f.data[8..12], &[5, 6, 7, 255]);
    }

    #[test]
    fn monitors_primary_first_then_left_to_right() {
        let got = order_monitors(vec![
            mon(0, "DP-2", false, 1920, 0),
            mon(1, "HDMI-1", false, 0, 1080),
            mon(2, "DP-1", true, 0, 0),
            mon(3, "DP-3", false, 0, 2160),
        ]);
        let names: Vec<_> = got.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["DP-1", "HDMI-1", "DP-3", "DP-2"]);
        assert!(got.iter().enumerate().all(|(i, m)| m.index as usize == i));
        assert_eq!(got.iter().filter(|m| m.primary).count(), 1);
    }

    #[test]
    fn first_monitor_becomes_primary_when_none_flagged() {
        let got = order_monitors(vec![mon(0, "b", false, 100, 0), mon(1, "a", false, 0, 0)]);
        assert_eq!(got[0].name, "a");
        assert!(got[0].primary);
        assert!(order_monitors(Vec::new()).is_empty());
    }

    #[test]
    fn cursor_conversion_unpremultiplies() {
        // Opaque red, half-transparent premultiplied white-ish, fully transparent.
        let rgba = argb_premul_to_rgba(&[0xFFFF_0000, 0x8040_4040, 0x0000_0000]);
        assert_eq!(&rgba[0..4], &[255, 0, 0, 255]);
        assert_eq!(&rgba[4..8], &[128, 128, 128, 128]);
        assert_eq!(&rgba[8..12], &[0, 0, 0, 0]);
        // Corrupt premultiplied values (colour above alpha) saturate instead of wrapping.
        assert_eq!(argb_premul_to_rgba(&[0x01FF_0000])[0], 255);
    }

    fn gradient(w: u32, h: u32) -> BgraFrame {
        let mut f = BgraFrame::new_tight(w, h);
        for y in 0..h {
            for (x, px) in f.row_mut(y).chunks_exact_mut(4).enumerate() {
                px.copy_from_slice(&[(x * 255 / w.max(1) as usize) as u8, y as u8, 7, 255]);
            }
        }
        f
    }

    #[test]
    fn downscale_flat_colour_is_exact() {
        let mut src = BgraFrame::new_tight(8, 6);
        for y in 0..6 {
            for px in src.row_mut(y).chunks_exact_mut(4) {
                px.copy_from_slice(&[10, 20, 30, 255]);
            }
        }
        let mut dst = BgraFrame::new_tight(4, 3);
        downscale_area(&src, &mut dst);
        for y in 0..3 {
            for px in dst.row(y).chunks_exact(4) {
                assert_eq!(px, &[10, 20, 30, 255]);
            }
        }
    }

    #[test]
    fn downscale_two_by_two_averages() {
        let mut src = BgraFrame::new_tight(2, 2);
        let vals = [
            [0u8, 0, 0, 255],
            [100, 0, 0, 255],
            [0, 100, 0, 255],
            [100, 100, 0, 255],
        ];
        for (i, v) in vals.iter().enumerate() {
            src.row_mut(i as u32 / 2)[(i % 2) * 4..(i % 2) * 4 + 4].copy_from_slice(v);
        }
        let mut dst = BgraFrame::new_tight(1, 1);
        downscale_area(&src, &mut dst);
        assert_eq!(dst.row(0), &[50, 50, 0, 255]);
    }

    #[test]
    fn downscale_non_integer_ratio_keeps_every_pixel_covered() {
        let src = gradient(10, 7);
        let mut dst = BgraFrame::new_tight(4, 3);
        downscale_area(&src, &mut dst);
        assert!(dst.is_well_formed());
        // Blue channel is constant 7 and alpha 255: averaging must preserve both exactly.
        for y in 0..3 {
            for px in dst.row(y).chunks_exact(4) {
                assert_eq!((px[2], px[3]), (7, 255));
            }
        }
        // The gradient stays monotonic left to right.
        assert!(dst.row(0)[0] <= dst.row(0)[4] && dst.row(0)[4] <= dst.row(0)[12]);
    }

    #[test]
    fn downscale_ignores_empty_sizes() {
        let src = BgraFrame::new_tight(0, 0);
        let mut dst = BgraFrame::new_tight(2, 2);
        downscale_area(&src, &mut dst);
        assert_eq!(dst.data, vec![0; 16]);
    }

    #[test]
    fn session_classification() {
        use SessionKind::*;
        assert_eq!(classify_session(Some("x11"), true, false), X11);
        assert_eq!(classify_session(Some("wayland"), true, true), Wayland);
        assert_eq!(classify_session(Some("Wayland"), false, true), Wayland);
        assert_eq!(classify_session(None, false, true), Wayland);
        assert_eq!(classify_session(None, true, true), X11);
        assert_eq!(classify_session(Some("tty"), false, false), NoDisplay);
    }
}
