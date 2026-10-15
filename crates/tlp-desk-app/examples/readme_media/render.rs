//! Headless rendering: the real `App` in an `egui_kittest` harness, drawn by `egui_wgpu`
//! into an offscreen texture and read back.
//!
//! Adapter choice is what makes the output reproducible across machines: a software
//! rasterizer (WARP on Windows, lavapipe/llvmpipe elsewhere) is preferred over any GPU, so a
//! developer's PC and the CI runner produce the same pixels. Set `TLP_MEDIA_GPU=1` to allow a
//! hardware adapter (faster, but the images will differ slightly from CI's).

use std::path::Path;
use std::sync::{Arc, Once};

use anyhow::{anyhow, Context as _};
use eframe::egui::{self, Theme, Vec2};
use eframe::{egui_wgpu, wgpu};
use egui_kittest::wgpu::WgpuTestRenderer;
use egui_kittest::Harness;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ImageEncoder, RgbImage};

use tlp_desk_app::app::{App, StartOptions};
use tlp_desk_app::i18n::{DigitStyle, Lang};
use tlp_desk_app::settings::Settings;

use crate::backend::MediaBackend;

/// Logical window size of every shot (points).
pub const SIZE: Vec2 = egui::vec2(1120.0, 700.0);

/// How one image is rendered.
#[derive(Debug, Clone, Copy)]
pub struct Look {
    pub lang: Lang,
    pub dark: bool,
    pub pixels_per_point: f32,
    /// Simulated time per `step()`, seconds (spinners, timers).
    pub step_dt: f32,
}

impl Look {
    pub fn theme_name(&self) -> &'static str {
        if self.dark {
            "dark"
        } else {
            "light"
        }
    }
}

/// wgpu setup that prefers a software adapter (see the module docs).
fn wgpu_setup() -> egui_wgpu::WgpuSetup {
    let allow_gpu = std::env::var_os("TLP_MEDIA_GPU").is_some_and(|v| v == "1");
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup
        .instance_descriptor
        .backends
        .remove(wgpu::Backends::BROWSER_WEBGPU);
    setup.native_adapter_selector = Some(Arc::new(move |adapters, _surface| {
        let mut list: Vec<&wgpu::Adapter> = adapters.iter().collect();
        list.sort_by_key(|a| {
            let info = a.get_info();
            let kind = match info.device_type {
                wgpu::DeviceType::Cpu => 0,
                _ if allow_gpu => 0,
                _ => 1,
            };
            let backend = match info.backend {
                wgpu::Backend::Dx12 => 0,
                wgpu::Backend::Vulkan => 1,
                wgpu::Backend::Metal => 2,
                _ => 3,
            };
            (kind, backend)
        });
        let chosen = list
            .first()
            .map(|a| (*a).clone())
            .ok_or_else(|| "no wgpu adapter found".to_owned())?;
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let i = chosen.get_info();
            eprintln!(
                "readme_media: rendering with {} ({:?}, {:?})",
                i.name, i.backend, i.device_type
            );
        });
        Ok(chosen)
    }));
    egui_wgpu::WgpuSetup::CreateNew(setup)
}

/// A harness running the real app on the scripted backend, on the home screen.
pub fn harness(look: Look) -> Harness<'static, App> {
    let settings = Settings {
        lang: look.lang,
        digits: DigitStyle::Western,
        device_name: None,
    };
    let opts = StartOptions {
        lang: Some(look.lang),
        // Never read or write the real address book.
        address_book_in_memory: true,
        ..StartOptions::default()
    };
    Harness::builder()
        .with_size(SIZE)
        .with_pixels_per_point(look.pixels_per_point)
        .with_theme(if look.dark { Theme::Dark } else { Theme::Light })
        .with_step_dt(look.step_dt)
        .renderer(WgpuTestRenderer::from_setup(wgpu_setup()))
        .build_eframe(move |cc| {
            let (backend, events) = MediaBackend::start(look.lang);
            App::new(cc, backend, events, settings, opts)
        })
}

/// Render the current frame as opaque RGB.
pub fn capture(h: &mut Harness<'static, App>) -> anyhow::Result<RgbImage> {
    let rgba = h.render().map_err(|e| anyhow!("render failed: {e}"))?;
    Ok(image::DynamicImage::ImageRgba8(rgba).to_rgb8())
}

/// Encode as PNG. `best` = smallest file (stills); otherwise fast (video frames).
pub fn encode_png(img: &RgbImage, best: bool) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    let (compression, filter) = if best {
        (CompressionType::Best, FilterType::Adaptive)
    } else {
        (CompressionType::Fast, FilterType::Sub)
    };
    PngEncoder::new_with_quality(&mut out, compression, filter)
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgb8,
        )
        .context("encoding PNG")?;
    Ok(out)
}

/// Write `bytes` unless the file already has exactly this content (keeps mtimes and git
/// quiet). Returns whether the file changed.
pub fn write_if_changed(path: &Path, bytes: &[u8]) -> anyhow::Result<bool> {
    if std::fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}
