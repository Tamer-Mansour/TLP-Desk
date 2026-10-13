//! Pure size arithmetic for GPU-side downscaling (platform independent, unit tested).

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
}
