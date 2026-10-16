//! Pure size arithmetic for GPU-side downscaling, plus the CPU reference / fallback resampler
//! (platform independent, unit tested).

use crate::BgraFrame;

/// Size of frames delivered for a `src` monitor when frames must fit inside `max`.
///
/// The aspect ratio is preserved (to within rounding), the result never exceeds `max` and the
/// source is never upscaled: a source that already fits is returned unchanged. A scaled result is
/// rounded down to even dimensions (4:2:0 video needs them), and is at least 2x2.
pub fn fit_within(src: (u32, u32), max: (u32, u32)) -> (u32, u32) {
    let (sw, sh) = src;
    let (mw, mh) = (max.0.max(2), max.1.max(2));
    if sw == 0 || sh == 0 || (sw <= mw && sh <= mh) {
        return src;
    }
    let (sw64, sh64, mw64, mh64) = (sw as u64, sh as u64, mw as u64, mh as u64);
    let (w, h) = if sw64 * mh64 >= mw64 * sh64 {
        // Width is the limiting side.
        (mw64, (sh64 * mw64 + sw64 / 2) / sw64)
    } else {
        ((sw64 * mh64 + sh64 / 2) / sh64, mh64)
    };
    let even = |v: u64, cap: u32| ((v.min(cap as u64) as u32) & !1).max(2);
    (even(w, mw), even(h, mh))
}

/// Mip level of the source to sample with bilinear filtering when shrinking `src` to `out`.
///
/// Level `n` is the source box-filtered by `2^n`. Bilinear sampling on its own averages at most
/// a 2x2 neighbourhood, so it is exact for a 2x reduction and aliases beyond that; this picks the
/// smallest level from which the remaining reduction is at most 2x. Returns 0 when no mips are
/// needed (reduction of 2x or less, or upscaling).
pub fn mip_level_for(src: (u32, u32), out: (u32, u32)) -> u32 {
    if out.0 == 0 || out.1 == 0 {
        return 0;
    }
    // Largest per-axis reduction, as a ratio src/out > 2^(level+1) test in integers.
    let mut level = 0u32;
    while level < 15 {
        let f = 2u64 << level; // reduction still allowed after sampling level `level`
        let fits_w = (src.0 as u64) <= (out.0 as u64) * f;
        let fits_h = (src.1 as u64) <= (out.1 as u64) * f;
        if fits_w && fits_h {
            break;
        }
        level += 1;
    }
    level
}

/// Size of mip `level` of a `size` texture (each level halves, rounding down, minimum 1).
pub fn mip_size(size: (u32, u32), level: u32) -> (u32, u32) {
    (
        (size.0 >> level.min(31)).max(1),
        (size.1 >> level.min(31)).max(1),
    )
}

/// Whether the final scaling pass (from the chosen mip to the output) should use the Lanczos-2
/// shader. A reduction of exactly 2x on both axes is already a perfect 2x2 box filter with a
/// bilinear tap, and a 1:1 copy needs no filter; everything else benefits from Lanczos.
pub fn needs_lanczos(mip: (u32, u32), out: (u32, u32)) -> bool {
    mip != out && !(mip.0 == out.0 * 2 && mip.1 == out.1 * 2)
}

/// Lanczos-2 kernel: `sinc(x) * sinc(x / 2)` on `|x| < 2`. Sharper than bilinear/box filtering
/// (keeps text edges) with only a faint overshoot.
pub fn lanczos2(x: f64) -> f64 {
    let x = x.abs();
    if x < 1e-9 {
        1.0
    } else if x >= 2.0 {
        0.0
    } else {
        let px = std::f64::consts::PI * x;
        2.0 * px.sin() * (px / 2.0).sin() / (px * px)
    }
}

/// Normalised Lanczos-2 taps for output pixel `o` of an axis shrunk from `src` to `out`
/// samples: `(first source index, weights)`. The kernel is stretched by the reduction ratio
/// when shrinking (so it also low-passes against aliasing), source indices are clamped by the
/// caller. The GPU shader in `windows_capture::scaler` computes exactly the same taps.
fn taps(o: u32, src: u32, out: u32) -> (i64, Vec<f64>) {
    let ratio = f64::from(src) / f64::from(out);
    let scale = ratio.max(1.0);
    let centre = (f64::from(o) + 0.5) * ratio;
    let radius = 2.0 * scale;
    let first = (centre - radius).floor() as i64;
    let last = (centre + radius).ceil() as i64;
    let mut w: Vec<f64> = (first..=last)
        .map(|j| lanczos2((j as f64 + 0.5 - centre) / scale))
        .collect();
    let sum: f64 = w.iter().sum();
    if sum > 0.0 {
        for v in &mut w {
            *v /= sum;
        }
    }
    (first, w)
}

/// Resamples `src` to `w` x `h` with a separable Lanczos-2 filter (CPU path: reference for the
/// GPU scaler's tests and fallback wherever no GPU is available). Alpha is forced opaque.
/// Returns an empty frame for an empty source or target.
pub fn resample_lanczos2(src: &BgraFrame, w: u32, h: u32) -> BgraFrame {
    let mut out = BgraFrame::new_tight(w, h);
    out.timestamp_us = src.timestamp_us;
    if src.width == 0 || src.height == 0 || w == 0 || h == 0 || !src.is_well_formed() {
        return out;
    }
    let (sw, sh) = (src.width as usize, src.height as usize);
    // Horizontal pass into f32 (3 channels) at the target width, source height.
    let mut tmp = vec![0f32; w as usize * sh * 3];
    let htaps: Vec<_> = (0..w).map(|o| taps(o, src.width, w)).collect();
    for y in 0..sh {
        let row = src.row(y as u32);
        for (ox, (first, weights)) in htaps.iter().enumerate() {
            let mut acc = [0f64; 3];
            for (k, wt) in weights.iter().enumerate() {
                let x = (first + k as i64).clamp(0, sw as i64 - 1) as usize;
                for (c, a) in acc.iter_mut().enumerate() {
                    *a += wt * f64::from(row[x * 4 + c]);
                }
            }
            for (c, a) in acc.iter().enumerate() {
                tmp[(y * w as usize + ox) * 3 + c] = *a as f32;
            }
        }
    }
    for oy in 0..h {
        let (first, weights) = taps(oy, src.height, h);
        let dst = out.row_mut(oy);
        for ox in 0..w as usize {
            let mut acc = [0f64; 3];
            for (k, wt) in weights.iter().enumerate() {
                let y = (first + k as i64).clamp(0, sh as i64 - 1) as usize;
                for (c, a) in acc.iter_mut().enumerate() {
                    *a += wt * f64::from(tmp[(y * w as usize + ox) * 3 + c]);
                }
            }
            for (c, a) in acc.iter().enumerate() {
                dst[ox * 4 + c] = a.round().clamp(0.0, 255.0) as u8;
            }
            dst[ox * 4 + 3] = 255;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_sources_that_already_fit() {
        assert_eq!(fit_within((1920, 1080), (1920, 1200)), (1920, 1080));
        assert_eq!(fit_within((1920, 1200), (1920, 1200)), (1920, 1200));
        // Odd sizes that fit are not touched (the encoder pads them).
        assert_eq!(fit_within((1365, 767), (1920, 1080)), (1365, 767));
        assert_eq!(fit_within((0, 0), (1920, 1080)), (0, 0));
    }

    #[test]
    fn fit_scales_exact_halves() {
        assert_eq!(fit_within((3840, 2400), (1920, 1200)), (1920, 1200));
        assert_eq!(fit_within((3840, 2160), (1920, 1080)), (1920, 1080));
        assert_eq!(fit_within((3840, 2160), (1920, 1200)), (1920, 1080));
    }

    #[test]
    fn fit_preserves_aspect_and_limits_both_sides() {
        // 16:10 into a 16:9 box: height limits.
        assert_eq!(fit_within((3840, 2400), (1920, 1080)), (1728, 1080));
        // 1440p into 1920x1200: width limits.
        assert_eq!(fit_within((2560, 1440), (1920, 1200)), (1920, 1080));
        // Portrait monitor.
        assert_eq!(fit_within((2160, 3840), (1920, 1200)), (674, 1200));
        // Ultra-wide.
        assert_eq!(fit_within((5120, 1440), (1920, 1200)), (1920, 540));
    }

    #[test]
    fn fit_rounds_down_to_even_and_never_exceeds_max() {
        for &(src, max) in &[
            ((3000, 2001), (1919, 1199)),
            ((7681, 4321), (1280, 720)),
            ((2561, 1601), (1921, 1081)),
            ((4000, 3), (100, 100)),
        ] {
            let (w, h) = fit_within(src, max);
            assert!(
                w <= max.0 && h <= max.1,
                "{src:?} into {max:?} gave {w}x{h}"
            );
            assert_eq!(w % 2, 0, "{src:?} -> {w}");
            assert_eq!(h % 2, 0, "{src:?} -> {h}");
            assert!(w >= 2 && h >= 2);
        }
        // Aspect within one even step.
        let (w, h) = fit_within((3000, 2001), (1919, 1199));
        let want_h = 2001.0 * w as f64 / 3000.0;
        assert!((h as f64 - want_h).abs() <= 2.0, "{w}x{h}");
    }

    #[test]
    fn mip_level_choice() {
        assert_eq!(mip_level_for((1920, 1080), (1920, 1080)), 0);
        assert_eq!(mip_level_for((2560, 1440), (1920, 1080)), 0);
        assert_eq!(mip_level_for((3840, 2400), (1920, 1200)), 0); // exactly 2x: bilinear = box
        assert_eq!(mip_level_for((3840, 2400), (1728, 1080)), 1); // 2.2x
        assert_eq!(mip_level_for((7680, 4320), (1920, 1080)), 1); // 4x: mip 1 + 2x bilinear
        assert_eq!(mip_level_for((7680, 4320), (1280, 720)), 2); // 6x
        assert_eq!(mip_level_for((1280, 720), (1920, 1080)), 0); // upscale
        assert_eq!(mip_level_for((100, 100), (0, 0)), 0);
    }

    fn gradient(w: u32, h: u32) -> BgraFrame {
        let mut f = BgraFrame::new_tight(w, h);
        for y in 0..h {
            for x in 0..w {
                let p = &mut f.row_mut(y)[x as usize * 4..x as usize * 4 + 4];
                p.copy_from_slice(&[(x * 255 / w) as u8, (y * 255 / h) as u8, 90, 255]);
            }
        }
        f
    }

    #[test]
    fn lanczos_is_used_only_for_non_trivial_ratios() {
        assert!(!needs_lanczos((1920, 1080), (1920, 1080)));
        assert!(
            !needs_lanczos((3840, 2160), (1920, 1080)),
            "exact 2x is a box"
        );
        assert!(needs_lanczos((1920, 1200), (1600, 1000)));
        assert!(needs_lanczos((2560, 1440), (1920, 1080)));
        assert!(
            needs_lanczos((3840, 2160), (1920, 1200)),
            "2x wide, 1.8x high"
        );
        assert_eq!(mip_size((3840, 2400), 1), (1920, 1200));
        assert_eq!(mip_size((5, 3), 4), (1, 1));
    }

    #[test]
    fn lanczos_kernel_shape() {
        assert!((lanczos2(0.0) - 1.0).abs() < 1e-12);
        assert!(lanczos2(1.0).abs() < 1e-12, "zero at the integers");
        assert_eq!(lanczos2(2.0), 0.0);
        assert!(
            lanczos2(1.5) < 0.0,
            "negative lobe is what keeps edges sharp"
        );
        assert_eq!(lanczos2(-0.3), lanczos2(0.3));
    }

    #[test]
    fn resample_flat_colour_is_exact_for_any_ratio() {
        let mut f = BgraFrame::new_tight(100, 61);
        for p in f.data.chunks_exact_mut(4) {
            p.copy_from_slice(&[10, 200, 77, 255]);
        }
        for (w, h) in [(50, 30), (37, 23), (100, 61), (7, 5)] {
            let o = resample_lanczos2(&f, w, h);
            assert_eq!((o.width, o.height), (w, h));
            assert!(
                o.data.chunks_exact(4).all(|p| p == [10, 200, 77, 255]),
                "{w}x{h}"
            );
        }
    }

    #[test]
    fn resample_keeps_a_smooth_gradient_in_place() {
        let o = resample_lanczos2(&gradient(300, 200), 160, 100);
        for y in 5..o.height - 5 {
            let r = o.row(y);
            for x in 5..o.width as usize - 5 {
                let want = (x as f64 + 0.5) * 255.0 / 160.0;
                let got = f64::from(r[x * 4]);
                assert!((got - want).abs() < 4.0, "x={x}: {got} vs {want}");
            }
        }
    }

    #[test]
    fn resample_handles_empty_input() {
        let o = resample_lanczos2(&BgraFrame::new_tight(0, 0), 10, 10);
        assert_eq!((o.width, o.height), (10, 10));
        let o = resample_lanczos2(&gradient(8, 8), 0, 4);
        assert_eq!(o.data.len(), 0);
    }
}
