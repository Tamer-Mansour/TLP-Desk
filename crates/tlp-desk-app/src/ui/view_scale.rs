//! How the remote screen is placed and sampled on the viewer's display, so text stays crisp.
//!
//! * The image rectangle is snapped to **physical pixels** (`pixels_per_point` aware): an image
//!   drawn at a half-pixel offset is resampled by the GPU and looks blurry even at 1:1.
//! * When the displayed size equals the frame size (actual size, or a fit that happens to be
//!   1:1) the texture is sampled with **nearest** filtering: exact pixels, no blur at all.
//! * Otherwise it is **linear** (smooth up- and down-scaling). egui's wgpu backend has no mip
//!   chains, so when the picture is shrunk and the screen has been still for a moment,
//!   [`downscale_rgba`] pre-scales it once with a Lanczos-2 filter to the exact physical size
//!   and that texture is shown 1:1 instead (a still screen is exactly when the host sends its
//!   crisp refresh picture).

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, pos2, ColorImage, ImageData, Pos2, Rect, TextureHandle, Vec2};
use tlp_desk_capture::{resample_lanczos2, BgraFrame};

/// Texture filter for the remote frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sampling {
    /// Exact texels (1:1 display).
    Nearest,
    /// Bilinear (any other scale).
    Linear,
}

impl Sampling {
    /// The matching egui texture options.
    pub fn options(self) -> egui::TextureOptions {
        match self {
            Self::Nearest => egui::TextureOptions::NEAREST,
            Self::Linear => egui::TextureOptions::LINEAR,
        }
    }
}

/// Smallest shrink factor the still-screen pre-scaler handles (below this the filter is huge
/// and bilinear is acceptable anyway).
const MIN_PRESCALE_RATIO: f32 = 0.2;
/// Tolerance when deciding that a displayed size equals the frame size, in physical pixels.
const ONE_TO_ONE_EPS: f32 = 0.5;

/// Rounds a coordinate in points to the nearest physical pixel.
pub fn snap(v: f32, ppp: f32) -> f32 {
    if ppp <= 0.0 || !v.is_finite() {
        return v;
    }
    (v * ppp).round() / ppp
}

/// Rounds both corners to physical pixels (never collapsing below one physical pixel).
pub fn snap_rect(r: Rect, ppp: f32) -> Rect {
    if ppp <= 0.0 {
        return r;
    }
    let min = pos2(snap(r.min.x, ppp), snap(r.min.y, ppp));
    let max = pos2(
        snap(r.max.x, ppp).max(min.x + 1.0 / ppp),
        snap(r.max.y, ppp).max(min.y + 1.0 / ppp),
    );
    Rect::from_min_max(min, max)
}

/// Fits a `tex` (pixels) image into `avail` (points), centred, aspect preserved, with an
/// integer physical size and a physical-pixel origin. Near 1:1 it is exactly 1:1.
pub fn fit_rect_snapped(avail: Rect, tex: [usize; 2], ppp: f32) -> Rect {
    let (tw, th) = (tex[0] as f32, tex[1] as f32);
    if tw <= 0.0 || th <= 0.0 || ppp <= 0.0 {
        return Rect::from_center_size(avail.center(), Vec2::ZERO);
    }
    // Scale in physical pixels per texel.
    let s = (avail.width() * ppp / tw).min(avail.height() * ppp / th);
    let (pw, ph) = if (s - 1.0).abs() * tw.max(th) < ONE_TO_ONE_EPS {
        (tw, th)
    } else {
        ((tw * s).round().max(1.0), (th * s).round().max(1.0))
    };
    let size = Vec2::new(pw / ppp, ph / ppp);
    let min = avail.center() - size / 2.0;
    let min = pos2(snap(min.x, ppp), snap(min.y, ppp));
    Rect::from_min_size(min, size)
}

/// "Actual size" rectangle: exactly one physical pixel per texel at a physical-pixel origin.
pub fn original_rect(origin: Pos2, tex: [usize; 2], ppp: f32) -> Rect {
    let size = Vec2::new(tex[0] as f32, tex[1] as f32) / ppp.max(f32::EPSILON);
    Rect::from_min_size(Pos2::new(snap(origin.x, ppp), snap(origin.y, ppp)), size)
}

/// Displayed size in physical pixels.
fn physical_size(rect: Rect, ppp: f32) -> (f32, f32) {
    (rect.width() * ppp, rect.height() * ppp)
}

/// The texture filter for showing `tex` in `rect`.
pub fn sampling_for(tex: [usize; 2], rect: Rect, ppp: f32) -> Sampling {
    let (pw, ph) = physical_size(rect, ppp);
    if (pw - tex[0] as f32).abs() <= ONE_TO_ONE_EPS && (ph - tex[1] as f32).abs() <= ONE_TO_ONE_EPS
    {
        Sampling::Nearest
    } else {
        Sampling::Linear
    }
}

/// Size to pre-scale a still frame to, when it is shown smaller than the frame: the displayed
/// physical size. `None` when shown 1:1 / enlarged, or shrunk by more than 5x.
pub fn prescale_target(tex: [usize; 2], rect: Rect, ppp: f32) -> Option<[usize; 2]> {
    let (pw, ph) = physical_size(rect, ppp);
    let (w, h) = (pw.round(), ph.round());
    if w < 1.0 || h < 1.0 || tex[0] == 0 || tex[1] == 0 {
        return None;
    }
    let ratio = (w / tex[0] as f32).min(h / tex[1] as f32);
    (MIN_PRESCALE_RATIO..0.98)
        .contains(&ratio)
        .then_some([w as usize, h as usize])
}

/// Lanczos-2 downscale of an opaque RGBA image (alpha is forced to 255).
pub fn downscale_rgba(img: &ColorImage, size: [usize; 2]) -> ColorImage {
    let src = BgraFrame {
        width: img.size[0] as u32,
        height: img.size[1] as u32,
        stride: img.size[0] as u32 * 4,
        data: img.as_raw().to_vec(),
        timestamp_us: 0,
    };
    // The filter treats the three colour channels alike, so BGRA vs RGBA order is irrelevant.
    let out = resample_lanczos2(&src, size[0] as u32, size[1] as u32);
    ColorImage::from_rgba_premultiplied(size, &out.data)
}

/// The screen has to stay unchanged this long before the pre-scaled still image replaces the
/// bilinear one.
const STILL_AFTER: Duration = Duration::from_millis(120);

/// A pre-scaled copy of a still frame, shown 1:1.
struct Prescaled {
    tex: TextureHandle,
    size: [usize; 2],
    generation: u64,
}

/// Viewer-side state for sharp display of the remote frame: remembers the newest frame (cheap
/// `Arc`) so the texture filter can change without waiting for the next frame, and owns the
/// still-screen pre-scaled texture.
#[derive(Default)]
pub struct RemoteView {
    image: Option<Arc<ColorImage>>,
    sampling: Option<Sampling>,
    generation: u64,
    last_frame: Option<Instant>,
    prescaled: Option<Prescaled>,
}

impl RemoteView {
    /// Uploads a new frame into `texture` (created on first use) with the current filter.
    pub fn set_frame(
        &mut self,
        ctx: &egui::Context,
        texture: &mut Option<TextureHandle>,
        image: ColorImage,
        now: Instant,
    ) {
        let image = Arc::new(image);
        let options = self.sampling.unwrap_or(Sampling::Linear).options();
        match texture {
            Some(tex) => tex.set(ImageData::Color(image.clone()), options),
            None => {
                *texture = Some(ctx.load_texture(
                    "remote-screen",
                    ImageData::Color(image.clone()),
                    options,
                ));
            }
        }
        self.image = Some(image);
        self.generation += 1;
        self.last_frame = Some(now);
    }

    /// The texture to paint into `rect` (points, already snapped): switches the main texture's
    /// filter when the scale class changed, and swaps in the Lanczos pre-scaled still image
    /// when the picture is shrunk and the screen has been still.
    pub fn texture_for(
        &mut self,
        ctx: &egui::Context,
        tex: &mut TextureHandle,
        rect: Rect,
        now: Instant,
    ) -> egui::TextureId {
        let ppp = ctx.pixels_per_point();
        let Some(image) = self.image.clone() else {
            return tex.id();
        };
        let want = sampling_for(image.size, rect, ppp);
        if self.sampling != Some(want) {
            tex.set(ImageData::Color(image.clone()), want.options());
            self.sampling = Some(want);
        }
        let Some(target) = prescale_target(image.size, rect, ppp) else {
            return tex.id();
        };
        let idle = self
            .last_frame
            .map_or(STILL_AFTER, |t| now.saturating_duration_since(t));
        if idle < STILL_AFTER {
            // Come back once the screen has been still long enough.
            ctx.request_repaint_after(STILL_AFTER - idle);
            return tex.id();
        }
        let fresh = self
            .prescaled
            .as_ref()
            .is_some_and(|p| p.generation == self.generation && p.size == target);
        if !fresh {
            let scaled = downscale_rgba(&image, target);
            match &mut self.prescaled {
                Some(p) => p.tex.set(scaled, egui::TextureOptions::NEAREST),
                None => {
                    self.prescaled = Some(Prescaled {
                        tex: ctx.load_texture(
                            "remote-screen-still",
                            scaled,
                            egui::TextureOptions::NEAREST,
                        ),
                        size: target,
                        generation: self.generation,
                    })
                }
            }
            if let Some(p) = &mut self.prescaled {
                p.size = target;
                p.generation = self.generation;
            }
        }
        self.prescaled.as_ref().map_or(tex.id(), |p| p.tex.id())
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::Color32;

    use super::*;

    fn avail(w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(w, h))
    }

    fn on_grid(v: f32, ppp: f32) -> bool {
        let p = v * ppp;
        (p - p.round()).abs() < 1e-3
    }

    #[test]
    fn snap_lands_on_physical_pixels_for_fractional_dpi() {
        for ppp in [1.0, 1.25, 1.5, 1.75, 2.0, 2.25, 3.0] {
            for i in 0..200 {
                let v = i as f32 * 0.37 + 0.11;
                assert!(on_grid(snap(v, ppp), ppp), "ppp {ppp} v {v}");
                assert!((snap(v, ppp) - v).abs() <= 0.5 / ppp + 1e-4);
            }
        }
        assert_eq!(snap(10.4, 1.0), 10.0);
        assert_eq!(snap(10.5, 1.0), 11.0);
        assert_eq!(snap(5.0, 0.0), 5.0, "bad scale is a no-op");
        assert!(snap(f32::NAN, 1.0).is_nan());
    }

    #[test]
    fn snap_rect_has_integer_physical_edges_and_never_collapses() {
        let r = snap_rect(
            Rect::from_min_max(pos2(10.3, 20.7), pos2(110.6, 80.2)),
            1.25,
        );
        for v in [r.min.x, r.min.y, r.max.x, r.max.y] {
            assert!(on_grid(v, 1.25), "{v}");
        }
        let thin = snap_rect(Rect::from_min_max(pos2(5.2, 5.2), pos2(5.3, 5.3)), 2.0);
        assert!(thin.width() >= 0.5 - 1e-6 && thin.height() >= 0.5 - 1e-6);
    }

    #[test]
    fn fit_is_exactly_one_to_one_when_the_window_matches() {
        // 1920x1080 frame, 1920x1080 physical px available at 100% and 150% scaling.
        for ppp in [1.0, 1.5] {
            let a = avail(1920.0 / ppp, 1080.0 / ppp);
            let r = fit_rect_snapped(a, [1920, 1080], ppp);
            assert_eq!(physical_size(r, ppp), (1920.0, 1080.0), "ppp {ppp}");
            assert_eq!(sampling_for([1920, 1080], r, ppp), Sampling::Nearest);
        }
    }

    #[test]
    fn fit_near_one_to_one_snaps_to_exact_pixels() {
        // A window 0.3 physical px short of the frame is still shown 1:1.
        let r = fit_rect_snapped(avail(1919.7, 1080.0), [1920, 1080], 1.0);
        assert_eq!(physical_size(r, 1.0), (1920.0, 1080.0));
        // But 10 px short is a real downscale.
        let r = fit_rect_snapped(avail(1910.0, 1080.0), [1920, 1080], 1.0);
        assert_eq!(sampling_for([1920, 1080], r, 1.0), Sampling::Linear);
    }

    #[test]
    fn fit_has_integer_physical_size_and_origin_and_stays_inside() {
        for ppp in [1.0, 1.25, 1.5, 2.0] {
            for (aw, ah) in [(1000.3, 700.9), (833.3, 1200.1), (640.0, 360.0)] {
                let a = avail(aw, ah);
                let r = fit_rect_snapped(a, [1920, 1200], ppp);
                let (pw, ph) = physical_size(r, ppp);
                assert!((pw - pw.round()).abs() < 1e-3 && (ph - ph.round()).abs() < 1e-3);
                assert!(on_grid(r.min.x, ppp) && on_grid(r.min.y, ppp));
                assert!(r.width() <= a.width() + 1.0 / ppp && r.height() <= a.height() + 1.0 / ppp);
                // Aspect within one physical pixel.
                let want_h = pw * 1200.0 / 1920.0;
                assert!((ph - want_h).abs() <= 1.0, "{ph} vs {want_h}");
            }
        }
    }

    #[test]
    fn original_mode_is_one_texel_per_physical_pixel_at_any_dpi() {
        for ppp in [1.0, 1.25, 1.5, 2.0] {
            let r = original_rect(pos2(13.37, 4.2), [1920, 1080], ppp);
            assert_eq!(physical_size(r, ppp), (1920.0, 1080.0), "ppp {ppp}");
            assert!(on_grid(r.min.x, ppp) && on_grid(r.min.y, ppp));
            assert_eq!(sampling_for([1920, 1080], r, ppp), Sampling::Nearest);
        }
    }

    #[test]
    fn sampling_is_linear_for_up_and_down_scaling() {
        let r = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(3840.0, 2160.0));
        assert_eq!(sampling_for([1920, 1080], r, 1.0), Sampling::Linear);
        let r = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(960.0, 540.0));
        assert_eq!(sampling_for([1920, 1080], r, 1.0), Sampling::Linear);
    }

    #[test]
    fn prescale_applies_only_to_moderate_shrinks() {
        let shown = |w: f32, h: f32| Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(w, h));
        assert_eq!(
            prescale_target([1920, 1080], shown(1280.0, 720.0), 1.0),
            Some([1280, 720])
        );
        // The same point size is a larger physical size at 125% scaling.
        assert_eq!(
            prescale_target([1920, 1080], shown(1280.0, 720.0), 1.25),
            Some([1600, 900])
        );
        assert_eq!(
            prescale_target([1920, 1080], shown(1920.0, 1080.0), 1.0),
            None
        );
        assert_eq!(
            prescale_target([1920, 1080], shown(2560.0, 1440.0), 1.0),
            None
        );
        assert_eq!(
            prescale_target([1920, 1080], shown(200.0, 112.0), 1.0),
            None
        );
        assert_eq!(prescale_target([0, 0], shown(100.0, 100.0), 1.0), None);
    }

    #[test]
    fn downscale_rgba_keeps_size_and_opaque_flat_colour() {
        let img = ColorImage::new([40, 30], vec![Color32::from_rgb(10, 200, 90); 40 * 30]);
        let out = downscale_rgba(&img, [25, 19]);
        assert_eq!(out.size, [25, 19]);
        assert!(out
            .pixels
            .iter()
            .all(|p| *p == Color32::from_rgb(10, 200, 90)));
    }

    fn frame(w: usize, h: usize) -> ColorImage {
        let mut px = vec![Color32::from_gray(245); w * h];
        for (i, p) in px.iter_mut().enumerate() {
            if (i % w) % 6 == 0 {
                *p = Color32::from_gray(20); // 1-px vertical strokes
            }
        }
        ColorImage::new([w, h], px)
    }

    #[test]
    fn remote_view_swaps_in_the_prescaled_still_image_only_when_shrunk_and_still() {
        let ctx = egui::Context::default();
        let mut view = RemoteView::default();
        let mut texture = None;
        let t0 = Instant::now();
        view.set_frame(&ctx, &mut texture, frame(640, 360), t0);
        let mut tex = texture.clone().unwrap();
        let main = tex.id();

        // Shrunk to 480x270 (physical): main texture while frames are fresh ...
        let small = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(480.0, 270.0));
        assert_eq!(view.texture_for(&ctx, &mut tex, small, t0), main);
        // ... the Lanczos still image once the screen has been quiet.
        let later = t0 + STILL_AFTER + Duration::from_millis(5);
        let still = view.texture_for(&ctx, &mut tex, small, later);
        assert_ne!(still, main);
        assert_eq!(
            view.texture_for(&ctx, &mut tex, small, later),
            still,
            "cached"
        );

        // A new frame invalidates it until the screen is quiet again.
        view.set_frame(&ctx, &mut texture, frame(640, 360), later);
        assert_eq!(view.texture_for(&ctx, &mut tex, small, later), main);

        // Shown 1:1 or enlarged: never pre-scaled, and the filter follows the scale class.
        let exact = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(640.0, 360.0));
        let much_later = later + Duration::from_secs(5);
        assert_eq!(view.texture_for(&ctx, &mut tex, exact, much_later), main);
        assert_eq!(view.sampling, Some(Sampling::Nearest));
        let big = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(1280.0, 720.0));
        assert_eq!(view.texture_for(&ctx, &mut tex, big, much_later), main);
        assert_eq!(view.sampling, Some(Sampling::Linear));
    }

    /// The still-screen pre-scale is done on the UI thread once per settled picture; this prints
    /// how long that takes (run with `--nocapture --release`).
    #[test]
    fn prescale_timing_is_reported() {
        let img = frame(1920, 1080);
        let t = Instant::now();
        let out = downscale_rgba(&img, [1280, 720]);
        eprintln!("1920x1080 -> 1280x720 Lanczos-2: {:?}", t.elapsed());
        assert_eq!(out.size, [1280, 720]);
    }
}
