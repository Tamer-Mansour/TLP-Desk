//! Icons from the [tlp-icons](https://github.com/Tamer-Mansour/tlp-icons) set (MIT; Lucide ISC
//! geometry), vendored as static SVG files in `assets/icons/` by `scripts/vendor-icons.ps1`.
//!
//! The SVGs are embedded in the binary and rasterized on demand by an egui [`ImageLoader`]
//! ([`install`]) with resvg, **white**, at the exact pixel size the widget asks for (size in
//! points x `pixels_per_point`), so they stay crisp at 1x-3x. Color comes from the image tint, so one
//! raster serves every theme color; buttons can use egui's
//! [`egui::Button::image_tint_follows_text_color`] to follow the widget text color (hover,
//! disabled, light/dark).
//!
//! RTL: [`Icon::is_directional`] icons (arrows that mean forward/back, connect, send, undo, ...)
//! are mirrored horizontally in Arabic by [`Icon::image_dir`]; all others never are.
//!
//! ```ignore
//! use crate::ui::icons::{self, Icon};
//! if icons::icon_button(ui, Icon::Settings, tx.t(S::Settings)).clicked() { /* ... */ }
//! ui.add(icons::icon_text_button(tx, Icon::Connect, tx.t(S::Connect)));
//! ui.add(Icon::Warning.image(16.0, theme::danger(dark)));
//! ```

use std::sync::Arc;

use eframe::egui::{
    self,
    load::{ImageLoadResult, ImageLoader, ImagePoll, LoadError, SizeHint},
    Color32, ColorImage, Context, Image, ImageSource, IntoAtoms, Response, Ui, WidgetInfo,
    WidgetText, WidgetType,
};
use resvg::{tiny_skia, usvg};

use crate::i18n::Tx;

/// Default icon size next to button text, in points.
pub const BUTTON_ICON: f32 = 16.0;
/// Default size of an icon-only button's glyph, in points.
pub const TOOL_ICON: f32 = 18.0;

/// URI scheme served by the icon loader: `tlp-icon://<name>.svg` or `tlp-icon://rtl/<name>.svg`.
const SCHEME: &str = "tlp-icon://";
const MIRRORED: &str = "rtl/";
/// Raster size limit (pixels per side); a larger request is clamped instead of allocating.
const MAX_PX: u32 = 512;
/// Every tlp-icons icon is drawn on a 24x24 grid.
const GRID: f32 = 24.0;

macro_rules! icons {
    ($($(#[$doc:meta])* $variant:ident => $file:literal,)*) => {
        /// An icon of the vendored set. See `assets/icons/README.md` for what each one is used for.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Icon {
            $($(#[$doc])* $variant,)*
        }

        impl Icon {
            /// Every icon, in allow-list order.
            pub const ALL: &'static [Icon] = &[$(Icon::$variant,)*];

            /// The tlp-icons name (also the SVG file name without `.svg`).
            pub fn name(self) -> &'static str {
                match self {
                    $(Icon::$variant => $file,)*
                }
            }

            /// The embedded SVG source.
            pub fn svg(self) -> &'static [u8] {
                match self {
                    $(Icon::$variant => include_bytes!(concat!("../../assets/icons/", $file, ".svg")),)*
                }
            }

            fn from_name(name: &str) -> Option<Icon> {
                match name {
                    $($file => Some(Icon::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

icons! {
    /// Connect to a device (directional).
    Connect => "log-in",
    /// End a viewer session (directional).
    Disconnect => "log-out",
    /// Host banner: end an incoming session.
    EndSession => "unplug",
    Monitor => "monitor",
    AllMonitors => "layout-grid",
    MonitorOff => "monitor-off",
    Fullscreen => "maximize",
    ExitFullscreen => "minimize",
    FitToWindow => "shrink",
    ActualSize => "scan",
    ViewOnly => "eye",
    Hidden => "eye-off",
    Stats => "activity",
    Gauge => "gauge",
    Latency => "timer",
    Quality => "sliders-horizontal",
    Keyboard => "keyboard",
    MousePointer => "mouse-pointer-2",
    CtrlAltDel => "command",
    /// Privacy mode (a tlp-icons original).
    Privacy => "privacy-eye-shield",
    Record => "circle-dot",
    Playback => "circle-play",
    Camera => "camera",
    Whiteboard => "pen-tool",
    Chat => "message-square",
    /// Send a chat message (directional).
    Send => "send-horizontal",
    Volume => "volume-2",
    Mute => "volume-x",
    Power => "power",
    Restart => "rotate-ccw",
    WakeOnLan => "zap",
    PortForward => "arrow-right-left",
    Plug => "plug",
    Settings => "settings",
    Language => "languages",
    Globe => "globe",
    Sun => "sun",
    Moon => "moon",
    Copy => "copy",
    Copied => "copy-check",
    Refresh => "refresh-cw",
    Check => "check",
    Close => "x",
    Accept => "circle-check",
    Reject => "circle-x",
    Lock => "lock",
    Unlock => "lock-open",
    Shield => "shield",
    ShieldCheck => "shield-check",
    Key => "key-round",
    Fingerprint => "fingerprint-pattern",
    Hash => "hash",
    Warning => "triangle-alert",
    Error => "circle-alert",
    Info => "info",
    Help => "circle-question-mark",
    Wifi => "wifi",
    Network => "network",
    Lan => "ethernet-port",
    Radar => "radar",
    Search => "search",
    Laptop => "laptop",
    Server => "server",
    DeviceInfo => "cpu",
    HardDrive => "hard-drive",
    Home => "house",
    User => "user",
    Users => "users",
    AddressBook => "book-user",
    Star => "star",
    Tag => "tag",
    Bell => "bell",
    Log => "scroll-text",
    List => "list",
    History => "rotate-ccw-clock",
    Clock => "clock",
    Tray => "app-window",
    Minimize => "minimize-2",
    Menu => "menu",
    More => "ellipsis",
    Plus => "plus",
    Minus => "minus",
    Save => "save",
    Upload => "upload",
    Download => "download",
    Folder => "folder",
    FolderOpen => "folder-open",
    NewFolder => "folder-plus",
    File => "file",
    Trash => "trash",
    Rename => "pencil",
    Pause => "pause",
    Play => "play",
    Stop => "square",
    Clipboard => "clipboard",
    /// Back (directional).
    Back => "arrow-left",
    /// Forward (directional).
    Forward => "arrow-right",
    ChevronLeft => "chevron-left",
    ChevronRight => "chevron-right",
    ChevronUp => "chevron-up",
    ChevronDown => "chevron-down",
    Undo => "undo-2",
    Redo => "redo-2",
}

impl Icon {
    /// Whether the icon points along the reading direction and must be mirrored in RTL.
    /// Media "play" and symmetric arrows (`PortForward`) are deliberately not mirrored.
    pub fn is_directional(self) -> bool {
        matches!(
            self,
            Icon::Connect
                | Icon::Disconnect
                | Icon::Send
                | Icon::Back
                | Icon::Forward
                | Icon::ChevronLeft
                | Icon::ChevronRight
                | Icon::Undo
                | Icon::Redo
        )
    }

    /// The loader URI of this icon, optionally mirrored.
    pub fn uri(self, mirrored: bool) -> String {
        let dir = if mirrored { MIRRORED } else { "" };
        format!("{SCHEME}{dir}{}.svg", self.name())
    }

    /// The icon as an image of `size` x `size` points tinted `color`, never mirrored.
    pub fn image(self, size: f32, color: Color32) -> Image<'static> {
        self.make_image(false, size, color)
    }

    /// Like [`Icon::image`], mirrored when `rtl` is set and the icon [is
    /// directional](Icon::is_directional).
    pub fn image_dir(self, rtl: bool, size: f32, color: Color32) -> Image<'static> {
        self.make_image(rtl && self.is_directional(), size, color)
    }

    fn make_image(self, mirrored: bool, size: f32, color: Color32) -> Image<'static> {
        Image::new(ImageSource::Uri(self.uri(mirrored).into()))
            .fit_to_exact_size(egui::vec2(size, size))
            .tint(color)
    }
}

/// Register the icon loader on `ctx` (idempotent). Call once at startup.
pub fn install(ctx: &Context) {
    let loader = IconLoader;
    if !ctx.is_loader_installed(loader.id()) {
        ctx.add_image_loader(Arc::new(loader));
    }
}

/// Rasterize `icon` white on transparent at `px` x `px` pixels (premultiplied), mirrored
/// horizontally if asked. `None` only if the embedded SVG failed to parse (a test guards that) or
/// `px` is 0.
pub fn rasterize(icon: Icon, px: u32, mirrored: bool) -> Option<ColorImage> {
    let px = px.min(MAX_PX);
    let source = std::str::from_utf8(icon.svg()).ok()?;
    // White strokes; the widget tints them.
    let source = source.replace("currentColor", "#ffffff");
    let tree = usvg::Tree::from_str(&source, &usvg::Options::default()).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(px, px)?;
    let s = px as f32 / GRID;
    let transform = if mirrored {
        tiny_skia::Transform::from_row(-s, 0.0, 0.0, s, px as f32, 0.0)
    } else {
        tiny_skia::Transform::from_scale(s, s)
    };
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let side = px as usize;
    let mut image = ColorImage::from_rgba_premultiplied([side, side], pixmap.data());
    image.source_size = egui::vec2(GRID, GRID);
    Some(image)
}

/// Pixel size for an egui size hint (an icon is square).
fn hint_px(hint: SizeHint) -> u32 {
    let px = match hint {
        SizeHint::Size { width, height, .. } => width.max(height),
        SizeHint::Width(w) => w,
        SizeHint::Height(h) => h,
        SizeHint::Scale(s) => (GRID * s.into_inner()).round().clamp(1.0, MAX_PX as f32) as u32,
    };
    px.clamp(1, MAX_PX)
}

/// Parse `tlp-icon://[rtl/]<name>.svg`.
fn parse_uri(uri: &str) -> Option<(Icon, bool)> {
    let rest = uri.strip_prefix(SCHEME)?.strip_suffix(".svg")?;
    let (name, mirrored) = match rest.strip_prefix(MIRRORED) {
        Some(name) => (name, true),
        None => (rest, false),
    };
    Some((Icon::from_name(name)?, mirrored))
}

/// Serves `tlp-icon://` URIs. Stateless: egui's texture loader caches the uploaded texture per
/// URI and size hint, and a re-raster of a 16-32 px icon costs microseconds.
struct IconLoader;

impl ImageLoader for IconLoader {
    fn id(&self) -> &str {
        concat!(module_path!(), "::IconLoader")
    }

    fn load(&self, _ctx: &Context, uri: &str, size_hint: SizeHint) -> ImageLoadResult {
        let Some((icon, mirrored)) = parse_uri(uri) else {
            return Err(LoadError::NotSupported);
        };
        match rasterize(icon, hint_px(size_hint), mirrored) {
            Some(image) => Ok(ImagePoll::Ready {
                image: Arc::new(image),
            }),
            None => Err(LoadError::Loading(format!(
                "icon {} failed to render",
                icon.name()
            ))),
        }
    }

    fn forget(&self, _uri: &str) {}

    fn forget_all(&self) {}

    fn byte_size(&self) -> usize {
        0
    }
}

/// Icon-only button with a tooltip (also its accessible name). The glyph follows the widget
/// text color and is mirrored in RTL when directional.
pub fn icon_button(ui: &mut Ui, tx: Tx, icon: Icon, tooltip: &str) -> Response {
    let image = icon.image_dir(tx.rtl(), TOOL_ICON, Color32::WHITE);
    let resp = ui.add(egui::Button::image(image).image_tint_follows_text_color(true));
    let enabled = resp.enabled();
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, tooltip));
    resp.on_hover_text(tooltip)
}

/// Icon-only toggle (selectable look) with a tooltip that is also its accessible name, for
/// compact toolbars.
pub fn icon_toggle(ui: &mut Ui, tx: Tx, selected: bool, icon: Icon, tooltip: &str) -> Response {
    icon_toggle_enabled(ui, tx, true, selected, icon, tooltip)
}

/// [`icon_toggle`] that can be disabled.
pub fn icon_toggle_enabled(
    ui: &mut Ui,
    tx: Tx,
    enabled: bool,
    selected: bool,
    icon: Icon,
    tooltip: &str,
) -> Response {
    let image = icon.image_dir(tx.rtl(), TOOL_ICON, Color32::WHITE);
    let button = egui::Button::selectable(selected, image).image_tint_follows_text_color(true);
    let resp = ui.add_enabled(enabled, button);
    resp.widget_info(|| {
        WidgetInfo::selected(WidgetType::SelectableLabel, enabled, selected, tooltip)
    });
    resp.on_hover_text(tooltip)
}

/// Button with an icon at the reading start of its text (left in English, right in Arabic).
/// The icon follows the widget text color.
pub fn icon_text_button<'a>(tx: Tx, icon: Icon, text: impl Into<WidgetText>) -> egui::Button<'a> {
    egui::Button::new(atoms(tx, icon, text, Color32::WHITE)).image_tint_follows_text_color(true)
}

/// Selectable (toggle-looking) variant of [`icon_text_button`].
pub fn icon_text_selectable<'a>(
    tx: Tx,
    selected: bool,
    icon: Icon,
    text: impl Into<WidgetText>,
) -> egui::Button<'a> {
    egui::Button::selectable(selected, atoms(tx, icon, text, Color32::WHITE))
        .image_tint_follows_text_color(true)
}

/// Icon + text in a fixed `color`, for filled buttons (e.g. white on the accent color, where the
/// text color is set on the `RichText`).
pub fn icon_text_colored<'a>(
    tx: Tx,
    icon: Icon,
    text: impl Into<WidgetText>,
    color: Color32,
) -> egui::Button<'a> {
    egui::Button::new(atoms(tx, icon, text, color))
}

/// Icon at the reading start: before the text in LTR, after it (on the right) in RTL.
fn atoms<'a>(tx: Tx, icon: Icon, text: impl Into<WidgetText>, color: Color32) -> egui::Atoms<'a> {
    let image = icon.image_dir(tx.rtl(), BUTTON_ICON, color);
    let text = text.into();
    if tx.rtl() {
        (text, image).into_atoms()
    } else {
        (image, text).into_atoms()
    }
}

/// A standalone icon (no interaction), e.g. before a label or in a banner.
pub fn icon(ui: &mut Ui, tx: Tx, icon: Icon, size: f32, color: Color32) -> Response {
    ui.add(icon.image_dir(tx.rtl(), size, color))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{DigitStyle, Lang};

    fn alpha(img: &ColorImage) -> Vec<u8> {
        img.pixels.iter().map(|p| p.a()).collect()
    }

    fn mirror(img: &ColorImage) -> Vec<u8> {
        let [w, h] = img.size;
        let a = alpha(img);
        let mut out = vec![0; w * h];
        for y in 0..h {
            for x in 0..w {
                out[y * w + x] = a[y * w + (w - 1 - x)];
            }
        }
        out
    }

    #[test]
    fn every_icon_parses_and_renders_at_common_sizes() {
        for &icon in Icon::ALL {
            for px in [16, 24, 32] {
                let img = rasterize(icon, px, false)
                    .unwrap_or_else(|| panic!("{icon:?} failed to render at {px}px"));
                assert_eq!(img.size, [px as usize, px as usize]);
                let total = img.pixels.len();
                let inked = img.pixels.iter().filter(|p| p.a() > 32).count();
                assert!(
                    inked >= px as usize,
                    "{icon:?} at {px}px is (almost) empty: {inked}"
                );
                assert!(
                    inked < total * 3 / 4,
                    "{icon:?} at {px}px is a filled block"
                );
                // White glyph: color channels equal alpha (premultiplied white).
                assert!(img.pixels.iter().all(|p| p.r() == p.a() && p.g() == p.a()));
            }
        }
    }

    #[test]
    fn allowlist_matches_the_enum() {
        let list = include_str!("../../assets/icons/allowlist.txt");
        let rows: Vec<(String, String)> = list
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| {
                let mut parts = l.split_whitespace();
                let variant = parts.next().unwrap_or_default().to_owned();
                let name = parts.next().unwrap_or_default().to_owned();
                (variant, name)
            })
            .collect();
        let ours: Vec<(String, String)> = Icon::ALL
            .iter()
            .map(|i| (format!("{i:?}"), i.name().to_owned()))
            .collect();
        assert_eq!(rows, ours);
    }

    #[test]
    fn directional_icons_are_flagged() {
        let directional: Vec<Icon> = Icon::ALL
            .iter()
            .copied()
            .filter(|i| i.is_directional())
            .collect();
        assert_eq!(
            directional,
            vec![
                Icon::Connect,
                Icon::Disconnect,
                Icon::Send,
                Icon::Back,
                Icon::Forward,
                Icon::ChevronLeft,
                Icon::ChevronRight,
                Icon::Undo,
                Icon::Redo,
            ]
        );
        for icon in [Icon::Play, Icon::PortForward, Icon::Settings, Icon::Copy] {
            assert!(!icon.is_directional(), "{icon:?}");
        }
    }

    #[test]
    fn rtl_mirroring_flips_the_image() {
        for &icon in Icon::ALL {
            let plain = rasterize(icon, 24, false).expect("renders");
            let flipped = rasterize(icon, 24, true).expect("renders");
            let expected = mirror(&plain);
            let got = alpha(&flipped);
            let max_diff = expected
                .iter()
                .zip(&got)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap_or(0);
            // Anti-aliasing is not perfectly symmetric: allow small per-pixel differences.
            let sum: u32 = expected
                .iter()
                .zip(&got)
                .map(|(a, b)| u32::from(a.abs_diff(*b)))
                .sum();
            let mean = sum as f32 / got.len() as f32;
            assert!(
                max_diff <= 128 && mean < 3.0,
                "{icon:?}: mirrored raster differs by {max_diff} (mean {mean})"
            );
            if icon.is_directional() {
                // A directional glyph is asymmetric: mirroring really changes it.
                let changed = alpha(&plain)
                    .iter()
                    .zip(&got)
                    .filter(|(a, b)| a.abs_diff(**b) > 64)
                    .count();
                assert!(changed > 10, "{icon:?} looks symmetric");
            }
        }
    }

    #[test]
    fn image_dir_mirrors_only_directional_icons_in_rtl() {
        let uri = |img: Image<'static>| match img.source(&Context::default()) {
            ImageSource::Uri(u) => u.to_string(),
            _ => String::new(),
        };
        let mirrored = Icon::Connect.uri(true);
        assert_eq!(
            uri(Icon::Connect.image_dir(true, 16.0, Color32::WHITE)),
            mirrored
        );
        assert_eq!(
            uri(Icon::Connect.image_dir(false, 16.0, Color32::WHITE)),
            Icon::Connect.uri(false)
        );
        assert_eq!(
            uri(Icon::Settings.image_dir(true, 16.0, Color32::WHITE)),
            Icon::Settings.uri(false)
        );
        assert_eq!(parse_uri(&mirrored), Some((Icon::Connect, true)));
        assert_eq!(parse_uri("tlp-icon://nope.svg"), None);
        assert_eq!(parse_uri("file://log-in.svg"), None);
    }

    #[test]
    fn hint_sizes_follow_dpi_and_are_bounded() {
        assert_eq!(
            hint_px(SizeHint::Size {
                width: 40,
                height: 40,
                maintain_aspect_ratio: true
            }),
            40
        );
        assert_eq!(hint_px(SizeHint::Width(100_000)), MAX_PX);
        assert_eq!(hint_px(SizeHint::Height(0)), 1);
        assert_eq!(hint_px(SizeHint::Scale(2.0_f32.into())), 48);
    }

    /// The loader serves real textures through egui's image pipeline, in both languages.
    #[test]
    fn icons_load_through_egui() {
        let ctx = Context::default();
        install(&ctx);
        install(&ctx); // idempotent
        for lang in [Lang::En, Lang::Ar] {
            let tx = Tx::new(lang, DigitStyle::Western);
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                icon_button(ui, tx, Icon::Settings, "الإعدادات");
                ui.add(icon_text_button(tx, Icon::Connect, "اتصال"));
            });
            out.textures_delta.clear();
        }
        let poll = ctx
            .try_load_texture(
                &Icon::Connect.uri(true),
                egui::TextureOptions::LINEAR,
                SizeHint::Size {
                    width: 32,
                    height: 32,
                    maintain_aspect_ratio: true,
                },
            )
            .expect("loads");
        assert!(matches!(poll, egui::load::TexturePoll::Ready { .. }));
    }
}
