//! The TLP-Desk design system: color tokens for both themes, the spacing / radius / elevation
//! scales, the type scale, the egui style built from them, and small layout helpers (including
//! RTL mirroring).
//!
//! Screens never hard-code colors or sizes: they ask [`palette`] for a token (or one of the
//! shorthand functions such as [`accent`] and [`danger`]) and use [`space`] / [`radius`] for
//! paddings and corners. `docs/src/dev/design.md` lists every token and when to use it.

use std::sync::Arc;

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId,
    Layout, Margin, RichText, Shadow, Stroke, TextStyle, TextWrapMode, Theme, Ui,
};

use crate::i18n::Lang;

/// Noto Sans Arabic Regular, SIL Open Font License 1.1 (see
/// `assets/fonts/NotoSansArabic-OFL.txt`). Downloaded from the official Noto project
/// distribution (github.com/notofonts).
const ARABIC_FONT: &[u8] = include_bytes!("../../assets/fonts/NotoSansArabic-Regular.ttf");
/// Inter 4.1 (SIL Open Font License 1.1, `assets/fonts/Inter-OFL.txt`), the Latin UI face:
/// Regular for text, Medium for buttons and labels, SemiBold for the device ID and titles.
/// Downloaded from the official release (github.com/rsms/inter).
const INTER_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Inter-Regular.ttf");
const INTER_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/Inter-Medium.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../../assets/fonts/Inter-SemiBold.ttf");

/// Font family names registered by [`font_definitions`]: use with `FontId::new(size,
/// FontFamily::Name(..))` for the weights egui has no text style for.
pub mod family {
    /// Inter Medium: buttons, eyebrows, navigation labels.
    pub const MEDIUM: &str = "inter-medium";
    /// Inter SemiBold: the device ID, screen titles, the hero numbers.
    pub const SEMIBOLD: &str = "inter-semibold";
}

/// `FontId` for Inter Medium at `size` (Arabic falls back to Noto Sans Arabic).
pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(family::MEDIUM.into()))
}

/// `FontId` for Inter SemiBold at `size` (Arabic falls back to Noto Sans Arabic).
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(family::SEMIBOLD.into()))
}

/// Spacing scale in points. Use these instead of ad-hoc numbers so screens line up.
pub mod space {
    /// Hairline gaps: icon to its label, chip padding.
    pub const XS: f32 = 4.0;
    /// Between related items in a row or a column.
    pub const S: f32 = 8.0;
    /// Between a caption and the value it labels, inside list rows.
    pub const M: f32 = 12.0;
    /// Between groups inside a card, card to card.
    pub const L: f32 = 16.0;
    /// Card padding, screen margins, between sections.
    pub const XL: f32 = 24.0;
}

/// Corner radii.
pub mod radius {
    /// Chips, badges, small toggles.
    pub const SM: u8 = 6;
    /// Buttons, inputs, list rows.
    pub const MD: u8 = 8;
    /// Cards and toasts.
    pub const LG: u8 = 12;
    /// Windows and dialogs.
    pub const XL: u8 = 16;
    /// Pills (fully rounded ends at our control heights).
    pub const PILL: u8 = 255;
}

/// Type scale (points). Body text follows egui's text styles set in [`install`].
pub mod font {
    /// Screen and dialog titles.
    pub const TITLE: f32 = 22.0;
    /// Card headers.
    pub const HEADING: f32 = 17.0;
    /// Regular text and buttons.
    pub const BODY: f32 = 14.0;
    /// Captions, hints, meta lines.
    pub const CAPTION: f32 = 12.0;
    /// The big device ID on the home screen.
    pub const DISPLAY: f32 = 44.0;
    /// Eyebrow section labels (small, tracked, muted).
    pub const EYEBROW: f32 = 11.0;
    /// The one-time code next to it.
    pub const CODE: f32 = 26.0;
    /// IDs and IP addresses inside text.
    pub const MONO: f32 = 13.5;
}

/// Every color token, for one theme. Text tokens are guaranteed (by a unit test) to keep a
/// contrast of at least 4.5:1 on every surface token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    /// Window background behind cards.
    pub bg: Color32,
    /// Cards, dialogs, the top bar.
    pub surface: Color32,
    /// Raised-inside-a-card areas: list rows, segmented control track, secondary buttons.
    pub surface_alt: Color32,
    /// Hover color of `surface_alt` things.
    pub surface_hover: Color32,
    /// Pressed color of `surface_alt` things.
    pub surface_pressed: Color32,
    /// Text inputs.
    pub input: Color32,
    /// Hairlines: card borders, dividers, input borders.
    pub border: Color32,
    /// Hover borders and stronger dividers.
    pub border_strong: Color32,
    /// Primary text.
    pub text: Color32,
    /// Secondary text: captions, hints, meta lines.
    pub text_muted: Color32,
    /// Brand accent for text, icons and focus rings.
    pub accent: Color32,
    /// Filled accent surfaces (primary buttons, selected tabs); carries `on_fill` text.
    pub accent_fill: Color32,
    /// `accent_fill` on hover.
    pub accent_hover: Color32,
    /// `accent_fill` while pressed.
    pub accent_pressed: Color32,
    /// Tinted accent background (selected rows, accent chips).
    pub accent_soft: Color32,
    /// Text and icons on filled accent / danger surfaces.
    pub on_fill: Color32,
    /// Success text and icons (online, done).
    pub success: Color32,
    /// Tinted success background.
    pub success_soft: Color32,
    /// Warning text and icons (reconnecting, attention).
    pub warning: Color32,
    /// Tinted warning background.
    pub warning_soft: Color32,
    /// Danger text and icons (errors, destructive).
    pub danger: Color32,
    /// Filled danger surfaces (disconnect, delete).
    pub danger_fill: Color32,
    /// `danger_fill` on hover.
    pub danger_hover: Color32,
    /// Tinted danger background.
    pub danger_soft: Color32,
    /// Informational text and icons.
    pub info: Color32,
    /// Tinted info background.
    pub info_soft: Color32,
    /// Shadow color for elevation (alpha carries the strength).
    pub shadow: Color32,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// Light theme tokens: the same structure as the dark theme, lifted. A cool off-white canvas,
/// paper-white panels, and the teal kept deep enough to carry white text.
pub const LIGHT: Palette = Palette {
    bg: rgb(0xEDF0F4),
    surface: rgb(0xFAFBFC),
    surface_alt: rgb(0xF0F3F6),
    surface_hover: rgb(0xEAEEF2),
    surface_pressed: rgb(0xDBE1E8),
    input: rgb(0xFFFFFF),
    border: rgb(0xE1E6EB),
    border_strong: rgb(0xC1C9D2),
    text: rgb(0x121820),
    text_muted: rgb(0x5A6574),
    accent: rgb(0x0A7266),
    accent_fill: rgb(0x0D8275),
    accent_hover: rgb(0x0B7467),
    accent_pressed: rgb(0x09655A),
    accent_soft: rgb(0xDAF1EC),
    on_fill: rgb(0xFFFFFF),
    success: rgb(0x167A4B),
    success_soft: rgb(0xDFF5E8),
    warning: rgb(0x8C5200),
    warning_soft: rgb(0xFCEFD9),
    danger: rgb(0xB42318),
    danger_fill: rgb(0xC2291E),
    danger_hover: rgb(0xA62217),
    danger_soft: rgb(0xFDE8E6),
    info: rgb(0x1F5CAA),
    info_soft: rgb(0xE3ECFA),
    shadow: Color32::from_rgba_premultiplied(0x10, 0x18, 0x28, 0x1C),
};

/// Dark theme tokens, the hero theme: graphite with a faint blue cast (never pure grey, never
/// pure black), four steps from canvas to pressed, and a teal light accent bright enough to
/// carry text on all of them.
pub const DARK: Palette = Palette {
    bg: rgb(0x0C0F13),
    surface: rgb(0x14181E),
    surface_alt: rgb(0x1C2129),
    surface_hover: rgb(0x242A33),
    surface_pressed: rgb(0x2D343E),
    input: rgb(0x0F1318),
    border: rgb(0x232931),
    border_strong: rgb(0x364049),
    text: rgb(0xECEFF3),
    text_muted: rgb(0x98A3B0),
    accent: rgb(0x52D6C3),
    accent_fill: rgb(0x128072),
    accent_hover: rgb(0x0F7267),
    accent_pressed: rgb(0x0D6157),
    accent_soft: rgb(0x133D38),
    on_fill: rgb(0xFFFFFF),
    success: rgb(0x5DD093),
    success_soft: rgb(0x16382A),
    warning: rgb(0xEEB950),
    warning_soft: rgb(0x3B3016),
    danger: rgb(0xF58D84),
    danger_fill: rgb(0xB5362C),
    danger_hover: rgb(0xA03027),
    danger_soft: rgb(0x3F1F1C),
    info: rgb(0x82B8F7),
    info_soft: rgb(0x1A2D46),
    shadow: Color32::from_rgba_premultiplied(0, 0, 0, 0x88),
};

/// The tokens of the light (`dark == false`) or dark theme.
pub fn palette(dark: bool) -> &'static Palette {
    if dark {
        &DARK
    } else {
        &LIGHT
    }
}

/// Tokens for the theme `ui` is drawn in.
pub fn tokens(ui: &Ui) -> &'static Palette {
    palette(ui.visuals().dark_mode)
}

/// Brand accent (teal) for text and icons, tuned per theme for contrast.
pub fn accent(dark: bool) -> Color32 {
    palette(dark).accent
}

/// Filled accent surface (white text on it keeps 4.5:1).
pub fn accent_fill(dark: bool) -> Color32 {
    palette(dark).accent_fill
}

/// Success text/icon color.
pub fn success(dark: bool) -> Color32 {
    palette(dark).success
}

/// Warning text/icon color.
pub fn warning(dark: bool) -> Color32 {
    palette(dark).warning
}

/// Danger text/icon color. Filled danger surfaces use [`Palette::danger_fill`].
pub fn danger(dark: bool) -> Color32 {
    palette(dark).danger
}

/// Informational text/icon color.
pub fn info(dark: bool) -> Color32 {
    palette(dark).info
}

/// Semantic tone of a chip, toast or banner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Plain information without emphasis.
    Neutral,
    /// Brand emphasis.
    Accent,
    /// Something worked / is available.
    Success,
    /// Needs attention but nothing is broken.
    Warning,
    /// Error or destructive.
    Danger,
    /// Informational.
    Info,
}

impl Tone {
    /// `(foreground, tinted background)` of this tone.
    pub fn colors(self, dark: bool) -> (Color32, Color32) {
        let p = palette(dark);
        match self {
            Tone::Neutral => (p.text_muted, p.surface_alt),
            Tone::Accent => (p.accent, p.accent_soft),
            Tone::Success => (p.success, p.success_soft),
            Tone::Warning => (p.warning, p.warning_soft),
            Tone::Danger => (p.danger, p.danger_soft),
            Tone::Info => (p.info, p.info_soft),
        }
    }
}

/// Soft shadow under panels (level 1): one point down, a wide faint blur.
pub fn card_shadow(dark: bool) -> Shadow {
    Shadow {
        offset: [0, 2],
        blur: 10,
        spread: 0,
        color: if dark {
            Color32::from_black_alpha(0x40)
        } else {
            Color32::from_rgba_premultiplied(0x0A, 0x10, 0x1A, 0x12)
        },
    }
}

/// Shadow under floating things: toolbars, toasts, popups (level 2).
pub fn float_shadow(dark: bool) -> Shadow {
    Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: palette(dark).shadow,
    }
}

/// Shadow under windows and modal dialogs (level 3).
pub fn window_shadow(dark: bool) -> Shadow {
    Shadow {
        offset: [0, 10],
        blur: 32,
        spread: 0,
        color: palette(dark).shadow,
    }
}

/// Inter as the proportional face (Regular, with Medium and SemiBold as named families), the
/// Arabic font as a fallback in every family, and egui's own fonts after that for emoji and
/// the monospace face. egui (epaint 0.36) shapes each run with harfrust, so Arabic gets real
/// OpenType shaping from Noto Sans Arabic.
pub fn font_definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let arabic = Arc::new(FontData::from_static(ARABIC_FONT).tweak(
        egui::epaint::text::FontTweak {
            scale: 1.08,
            ..Default::default()
        },
    ));
    fonts
        .font_data
        .insert("noto-sans-arabic".to_owned(), arabic);
    // Inter sits slightly low on the line in egui's layout; nudge it up a hair so Latin and
    // Arabic share a baseline in mixed rows.
    let inter = |bytes: &'static [u8]| {
        Arc::new(
            FontData::from_static(bytes).tweak(egui::epaint::text::FontTweak {
                y_offset_factor: -0.04,
                ..Default::default()
            }),
        )
    };
    fonts
        .font_data
        .insert("inter-regular".to_owned(), inter(INTER_REGULAR));
    fonts
        .font_data
        .insert(family::MEDIUM.to_owned(), inter(INTER_MEDIUM));
    fonts
        .font_data
        .insert(family::SEMIBOLD.to_owned(), inter(INTER_SEMIBOLD));

    // Proportional: Inter first, Arabic next, then egui's defaults (emoji) stay behind.
    let prop = fonts.families.entry(FontFamily::Proportional).or_default();
    prop.insert(0, "inter-regular".to_owned());
    prop.insert(1, "noto-sans-arabic".to_owned());
    // Monospace keeps egui's face (IDs and addresses), with Arabic as a fallback.
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .push("noto-sans-arabic".to_owned());
    // The named weights: that weight, then the Arabic fallback, then the emoji fallback.
    for name in [family::MEDIUM, family::SEMIBOLD] {
        let mut chain = vec![name.to_owned(), "noto-sans-arabic".to_owned()];
        chain.extend(
            fonts
                .families
                .get(&FontFamily::Proportional)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|f| f != "inter-regular" && f != "noto-sans-arabic"),
        );
        fonts.families.insert(FontFamily::Name(name.into()), chain);
    }
    fonts
}

/// Install fonts and the visual style for both themes.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());
    super::icons::install(ctx);

    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, semibold(font::TITLE)),
            (TextStyle::Body, FontId::proportional(font::BODY)),
            (TextStyle::Button, medium(font::BODY)),
            (TextStyle::Small, FontId::proportional(font::CAPTION)),
            (TextStyle::Monospace, FontId::monospace(font::MONO)),
        ]
        .into();
        let s = &mut style.spacing;
        s.item_spacing = egui::vec2(space::S + 2.0, space::S);
        s.button_padding = egui::vec2(space::L, 9.0);
        s.interact_size.y = 36.0;
        s.menu_margin = Margin::same(6);
        s.window_margin = Margin::same(20);
        s.icon_width = 18.0;
        s.icon_width_inner = 10.0;
        s.icon_spacing = space::S;
        style.visuals.window_corner_radius = CornerRadius::same(radius::XL);
        style.visuals.menu_corner_radius = CornerRadius::same(radius::LG);
    });
    for theme in [Theme::Light, Theme::Dark] {
        let dark = theme == Theme::Dark;
        ctx.style_mut_of(theme, |style| apply_palette(style, dark));
    }
}

/// Fill egui's visuals from the tokens of one theme.
fn apply_palette(style: &mut egui::Style, dark: bool) {
    let p = palette(dark);
    let v = &mut style.visuals;
    v.dark_mode = dark;
    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_shadow = window_shadow(dark);
    v.popup_shadow = float_shadow(dark);
    v.faint_bg_color = p.surface_alt;
    v.extreme_bg_color = p.surface_alt;
    v.text_edit_bg_color = Some(p.input);
    v.code_bg_color = p.surface_alt;
    v.weak_text_color = Some(p.text_muted);
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.danger;
    v.selection.bg_fill = p.accent_fill;
    v.selection.stroke = Stroke::new(1.5, p.on_fill);
    v.text_cursor.stroke = Stroke::new(2.0, p.accent);
    v.striped = false;

    let r = CornerRadius::same(radius::MD + 4);
    let w = &mut v.widgets;
    // Labels, separators, card hairlines.
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface_alt;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r;
    // Resting buttons and inputs: a tinted fill, no outline (outlines are for inputs, drawn
    // by the text field itself).
    w.inactive.bg_fill = p.surface_alt;
    w.inactive.weak_bg_fill = p.surface_alt;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = r;
    w.inactive.expansion = 0.0;
    // Hovered.
    w.hovered.bg_fill = p.surface_hover;
    w.hovered.weak_bg_fill = p.surface_hover;
    w.hovered.bg_stroke = Stroke::NONE;
    w.hovered.fg_stroke = Stroke::new(1.5, p.text);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;
    // Pressed.
    w.active.bg_fill = p.surface_pressed;
    w.active.weak_bg_fill = p.surface_pressed;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(2.0, p.text);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;
    // Open combo boxes and menus.
    w.open.bg_fill = p.surface_hover;
    w.open.weak_bg_fill = p.surface_hover;
    w.open.bg_stroke = Stroke::new(1.0, p.border_strong);
    w.open.fg_stroke = Stroke::new(1.0, p.text);
    w.open.corner_radius = r;
}

/// Per-language style tweaks (called on language switch).
pub fn apply_lang(ctx: &egui::Context, lang: Lang) {
    ctx.all_styles_mut(|style| {
        // Pre-shaped RTL text is in visual order per line; letting egui soft-wrap it would put
        // the end of the sentence on the first line. Keep RTL labels on one line.
        style.wrap_mode = if lang.is_rtl() {
            Some(TextWrapMode::Extend)
        } else {
            None
        };
    });
}

/// Horizontal row that flows from the reading-start side.
pub fn row<R>(ui: &mut Ui, rtl: bool, add: impl FnOnce(&mut Ui) -> R) -> R {
    let layout = if rtl {
        Layout::right_to_left(Align::Center)
    } else {
        Layout::left_to_right(Align::Center)
    };
    // Like `ui.horizontal`: start one widget tall so Align::Center does not grab all height.
    let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
    ui.allocate_ui_with_layout(size, layout, add).inner
}

/// Remaining space of a row, flowing from the reading-end side (for trailing buttons).
pub fn row_end<R>(ui: &mut Ui, rtl: bool, add: impl FnOnce(&mut Ui) -> R) -> R {
    row(ui, !rtl, add)
}

/// Vertical column aligned to the reading-start side.
pub fn column<R>(ui: &mut Ui, rtl: bool, add: impl FnOnce(&mut Ui) -> R) -> R {
    let align = if rtl { Align::Max } else { Align::Min };
    ui.with_layout(Layout::top_down(align), add).inner
}

/// The panel frame without showing it: a flat surface one step above the canvas, rounded,
/// with no hairline. In the light theme a faint shadow separates it from the canvas; in the
/// dark theme the grey step is enough and a shadow would only muddy the edge.
pub fn card_frame(ui: &Ui) -> egui::Frame {
    let dark = ui.visuals().dark_mode;
    let p = palette(dark);
    let mut f = egui::Frame::new()
        .fill(p.surface)
        .corner_radius(CornerRadius::same(radius::XL))
        .inner_margin(Margin::same(22));
    if !dark {
        f = f.shadow(card_shadow(dark));
    }
    f
}

/// Panel container (level-1 elevation): see [`card_frame`].
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    card_frame(ui)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// The glass panel: a translucent surface with a one-pixel inner highlight along its top
/// edge and a soft shadow under it, so it reads as a sheet of frosted glass lifted off the
/// canvas. Used for the workspace panels, the floating session toolbar and the HUD.
///
/// Draw order: shadow, fill, content, then the highlight stroke on top of the fill's edge
/// (the stroke is drawn by [`glass`] after the content, inside the same frame rect).
pub fn glass_frame(ui: &Ui) -> egui::Frame {
    let dark = ui.visuals().dark_mode;
    let p = palette(dark);
    let fill = if dark {
        // Lighter than the canvas and a little translucent: whatever is behind (another
        // panel, the video) shows through faintly.
        Color32::from_rgba_unmultiplied(p.surface.r(), p.surface.g(), p.surface.b(), 0xEC)
    } else {
        Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 0xF2)
    };
    egui::Frame::new()
        .fill(fill)
        .corner_radius(CornerRadius::same(radius::XL))
        .inner_margin(Margin::same(22))
        .shadow(if dark {
            glass_shadow_dark()
        } else {
            card_shadow(false)
        })
}

/// Glass panel container: [`glass_frame`] plus the edge highlight. See [`glass_frame`].
pub fn glass<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let dark = ui.visuals().dark_mode;
    let inner = glass_frame(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui)
    });
    paint_glass_edge(ui.painter(), inner.response.rect, dark);
    inner.inner
}

/// The highlight that makes a glass panel: a hairline that is brightest along the top edge
/// and fades down the sides (light catching the rim), drawn inside the rounded rect.
pub fn paint_glass_edge(painter: &egui::Painter, rect: egui::Rect, dark: bool) {
    let r = CornerRadius::same(radius::XL);
    let rim = if dark {
        Color32::from_white_alpha(0x16)
    } else {
        Color32::from_white_alpha(0xC0)
    };
    painter.rect_stroke(rect, r, Stroke::new(1.0, rim), egui::StrokeKind::Inside);
    // A second, brighter pass along the top only.
    let top = if dark {
        Color32::from_white_alpha(0x2A)
    } else {
        Color32::from_white_alpha(0xFF)
    };
    let inset = f32::from(radius::XL);
    painter.hline(
        (rect.min.x + inset)..=(rect.max.x - inset),
        rect.min.y + 0.5,
        Stroke::new(1.0, top),
    );
    if !dark {
        // In the light theme the bottom edge gets a faint hairline so the panel does not
        // melt into the canvas.
        painter.rect_stroke(
            rect,
            r,
            Stroke::new(1.0, Color32::from_black_alpha(0x0E)),
            egui::StrokeKind::Outside,
        );
    }
}

/// Dark-theme glass shadow: deep and wide, the panel floats over the canvas.
fn glass_shadow_dark() -> Shadow {
    Shadow {
        offset: [0, 10],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(0x66),
    }
}

/// Small uppercase-feeling section label ("eyebrow") above a group: tracked out a little and
/// muted, so it labels without competing with the content. Latin text is upper-cased; Arabic
/// has no case and is left as is.
pub fn eyebrow(ui: &Ui, text: &str) -> RichText {
    let shown: String = if text.chars().any(|c| c.is_ascii_lowercase()) {
        text.to_uppercase()
    } else {
        text.to_owned()
    };
    RichText::new(shown)
        .size(font::EYEBROW)
        .strong()
        .color(tokens(ui).text_muted)
}

/// Small, muted caption text.
pub fn caption(ui: &Ui, text: &str) -> RichText {
    RichText::new(text)
        .size(font::CAPTION)
        .color(tokens(ui).text_muted)
}

/// Screen or dialog title.
pub fn title(ui: &Ui, text: &str) -> RichText {
    RichText::new(text).size(font::TITLE).color(tokens(ui).text)
}

/// Monospace value (IDs, codes, IP addresses).
pub fn mono(text: &str, size: f32) -> RichText {
    RichText::new(text).monospace().size(size)
}

/// Filled call-to-action button.
pub fn primary_button(ui: &Ui, text: &str) -> egui::Button<'static> {
    let p = tokens(ui);
    egui::Button::new(RichText::new(text.to_owned()).color(p.on_fill).strong())
        .fill(p.accent_fill)
        .stroke(Stroke::NONE)
}

/// Paint a small status dot (with a soft halo) and reserve its space.
pub fn status_dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.circle_filled(rect.center(), 5.5, color.gamma_multiply(0.22));
    painter.circle_filled(rect.center(), 3.5, color);
}

/// Application icon: original design, generated in code (rounded tile with a screen glyph).
pub fn app_icon() -> egui::IconData {
    const N: u32 = 64;
    let mut rgba = vec![0u8; (N * N * 4) as usize];
    let tile = [0x0E, 0x7C, 0x70];
    for y in 0..N {
        for x in 0..N {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // Rounded square, radius 14.
            let r = 14.0;
            let cx = fx.clamp(r, N as f32 - r);
            let cy = fy.clamp(r, N as f32 - r);
            let inside = (fx - cx).powi(2) + (fy - cy).powi(2) <= r * r;
            let o = ((y * N + x) * 4) as usize;
            if !inside {
                continue;
            }
            let screen = (14..=50).contains(&x) && (16..=40).contains(&y);
            let screen_inner = (18..=46).contains(&x) && (20..=36).contains(&y);
            let stand = (28..=36).contains(&x) && (41..=46).contains(&y)
                || (22..=42).contains(&x) && (47..=49).contains(&y);
            let c = if screen_inner {
                [0x0E, 0x7C, 0x70]
            } else if screen || stand {
                [0xFF, 0xFF, 0xFF]
            } else {
                tile
            };
            rgba[o..o + 3].copy_from_slice(&c);
            rgba[o + 3] = 255;
        }
    }
    egui::IconData {
        rgba,
        width: N,
        height: N,
    }
}

/// Paint the brand mark (the app icon's shape, drawn as vectors) into `rect`.
pub fn paint_brand_mark(painter: &egui::Painter, rect: egui::Rect, dark: bool) {
    let p = palette(dark);
    let s = rect.width().min(rect.height());
    let rect = egui::Rect::from_center_size(rect.center(), egui::vec2(s, s));
    let r = |v: f32| CornerRadius::same(v.clamp(0.0, 255.0) as u8);
    painter.rect_filled(rect, r(s * 0.28), p.accent_fill);
    let u = s / 64.0;
    let at = |x: f32, y: f32| rect.min + egui::vec2(x * u, y * u);
    painter.rect_stroke(
        egui::Rect::from_min_max(at(14.0, 16.0), at(50.0, 40.0)),
        r(3.0 * u),
        Stroke::new(3.0 * u, p.on_fill),
        egui::StrokeKind::Inside,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(at(29.0, 40.0), at(35.0, 46.0)),
        0.0,
        p.on_fill,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(at(22.0, 46.0), at(42.0, 49.0)),
        1.0,
        p.on_fill,
    );
}

/// The hero tile fill (the device-ID tile on the home screen): the one place the brand teal
/// is used as a large surface. Two stops of the same hue, top-left lighter, so the tile reads
/// as lit rather than flat; `on_fill` text keeps its contrast on both stops (unit-tested).
pub fn hero_fill(dark: bool) -> (Color32, Color32) {
    if dark {
        (rgb(0x118074), rgb(0x0C5A51))
    } else {
        (rgb(0x0E8276), rgb(0x0A6C60))
    }
}

/// A soft radial glow (the "light" of the design) centered at `center`, fading to nothing
/// at `radius`. A mesh with the alpha set per vertex: the GPU interpolates it, so the glow
/// is smooth at any size (concentric discs would show rings on a dark canvas). Used behind
/// the identity card and under the active rail item.
pub fn glow_shape(center: egui::Pos2, radius: f32, color: Color32, strength: f32) -> egui::Shape {
    // (fraction of `radius`, fraction of `strength`): a quadratic-looking falloff, dense at
    // the center with a long soft tail.
    const RINGS: [(f32, f32); 4] = [(0.0, 1.0), (0.3, 0.45), (0.65, 0.12), (1.0, 0.0)];
    const SEGS: u32 = 48;
    let col = |a: f32| {
        let alpha = (strength * a * 255.0).round().clamp(0.0, 255.0) as u8;
        Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
    };
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(center, col(RINGS[0].1));
    for (f, a) in RINGS.iter().skip(1) {
        for k in 0..SEGS {
            let t = std::f32::consts::TAU * k as f32 / SEGS as f32;
            let p = center + egui::vec2(t.cos(), t.sin()) * (radius * f);
            mesh.colored_vertex(p, col(*a));
        }
    }
    // Vertex index of point `k` on ring `ring` (1-based; ring 0 is the center).
    let idx = |ring: usize, k: u32| 1 + (ring as u32 - 1) * SEGS + (k % SEGS);
    for k in 0..SEGS {
        mesh.add_triangle(0, idx(1, k), idx(1, k + 1));
    }
    for ring in 1..RINGS.len() - 1 {
        for k in 0..SEGS {
            let (a, b) = (idx(ring, k), idx(ring, k + 1));
            let (c, d) = (idx(ring + 1, k), idx(ring + 1, k + 1));
            mesh.add_triangle(a, b, c);
            mesh.add_triangle(b, d, c);
        }
    }
    egui::Shape::mesh(mesh)
}

/// The hero tile background for `rect` as one shape (so a caller can reserve a paint slot
/// before laying out the content and set the background underneath it afterwards): a
/// vertical two-stop gradient of [`hero_fill`] with the panel radius.
pub fn hero_shape(rect: egui::Rect, dark: bool) -> egui::Shape {
    let (top, bottom) = hero_fill(dark);
    let r = CornerRadius::same(radius::XL);
    // egui has no gradient fill for rounded rects, so this is a mesh: the rounded outline
    // triangulated as a fan from the center, each vertex colored by its y position. The GPU
    // interpolates the colors, which gives an exact, band-free gradient at any size.
    let _ = r;
    let mut mesh = egui::Mesh::default();
    let pts = rounded_outline(rect, f32::from(radius::XL));
    let color_at = |y: f32| {
        let t = ((y - rect.min.y) / rect.height().max(1.0)).clamp(0.0, 1.0);
        lerp_rgb(top, bottom, t)
    };
    let center = rect.center();
    mesh.colored_vertex(center, color_at(center.y));
    for p in &pts {
        mesh.colored_vertex(*p, color_at(p.y));
    }
    let n = pts.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    egui::Shape::mesh(mesh)
}

/// Points of a rounded rectangle outline (clockwise), 8 segments per corner.
fn rounded_outline(rect: egui::Rect, r: f32) -> Vec<egui::Pos2> {
    let r = r.min(rect.width() / 2.0).min(rect.height() / 2.0);
    let segs = 8;
    let mut out = Vec::with_capacity(4 * (segs + 1));
    let corners = [
        (egui::pos2(rect.max.x - r, rect.min.y + r), -90.0f32),
        (egui::pos2(rect.max.x - r, rect.max.y - r), 0.0),
        (egui::pos2(rect.min.x + r, rect.max.y - r), 90.0),
        (egui::pos2(rect.min.x + r, rect.min.y + r), 180.0),
    ];
    for (c, start_deg) in corners {
        for k in 0..=segs {
            let a = (start_deg + 90.0 * k as f32 / segs as f32).to_radians();
            out.push(egui::pos2(c.x + r * a.cos(), c.y + r * a.sin()));
        }
    }
    out
}

/// Linear mix of two colors (alpha included), `t` in `0..=1`.
pub fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| {
        let v = f32::from(x) + (f32::from(y) - f32::from(x)) * t;
        v.round().clamp(0.0, 255.0) as u8
    };
    Color32::from_rgba_unmultiplied(
        mix(a.r(), b.r()),
        mix(a.g(), b.g()),
        mix(a.b(), b.b()),
        mix(a.a(), b.a()),
    )
}

fn lerp_rgb(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |x: u8, y: u8| {
        let v = f32::from(x) + (f32::from(y) - f32::from(x)) * t;
        v.round().clamp(0.0, 255.0) as u8
    };
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

/// A round "avatar" with the first letter of `name` (device lists, the session title): the
/// disc color is picked from the name so the same device always gets the same color.
pub fn paint_avatar(painter: &egui::Painter, rect: egui::Rect, name: &str, dark: bool) {
    let initial: String = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "?".to_owned());
    let hash = name
        .bytes()
        .fold(5381u32, |h, b| h.wrapping_mul(33) ^ u32::from(b));
    let (fg, bg) = avatar_colors(hash, dark);
    let r = rect.width().min(rect.height()) / 2.0;
    painter.circle_filled(rect.center(), r, bg);
    // Arabic letters have no ascender line: scale them up so the initial fills the disc
    // like a Latin capital does.
    let arabic = initial
        .chars()
        .any(|c| ('\u{0600}'..='\u{06FF}').contains(&c));
    let size = if arabic { r * 1.35 } else { r * 1.05 };
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        initial,
        FontId::proportional(size),
        fg,
    );
}

/// Six muted disc tints with text that keeps AA contrast on them (unit-tested).
fn avatar_colors(hash: u32, dark: bool) -> (Color32, Color32) {
    const LIGHT_DISCS: [(u32, u32); 6] = [
        (0x0A6B60, 0xD7F0EB),
        (0x1F4FA3, 0xDDE8FA),
        (0x7A3E9E, 0xEEE1F8),
        (0x8A4A00, 0xFBE9D3),
        (0x1D6F3E, 0xDDF3E4),
        (0x8A2E3B, 0xFAE1E5),
    ];
    const DARK_DISCS: [(u32, u32); 6] = [
        (0x9FE6DA, 0x14443E),
        (0xA9C9FA, 0x1B3358),
        (0xD9BFF2, 0x3B2650),
        (0xF2CD9A, 0x4B3414),
        (0xA6E6BD, 0x173C27),
        (0xF5B7C1, 0x4A2229),
    ];
    let i = (hash % 6) as usize;
    let (fg, bg) = if dark { DARK_DISCS[i] } else { LIGHT_DISCS[i] };
    (rgb(fg), rgb(bg))
}

/// WCAG relative luminance of an opaque color.
pub fn luminance(c: Color32) -> f32 {
    fn channel(v: u8) -> f32 {
        let s = f32::from(v) / 255.0;
        if s <= 0.040_45 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
}

/// WCAG contrast ratio between two opaque colors (1.0 ..= 21.0).
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(p: &Palette) -> Vec<(&'static str, Color32)> {
        vec![
            ("bg", p.bg),
            ("surface", p.surface),
            ("surface_alt", p.surface_alt),
            ("surface_hover", p.surface_hover),
            ("surface_pressed", p.surface_pressed),
            ("input", p.input),
            ("border", p.border),
            ("border_strong", p.border_strong),
            ("text", p.text),
            ("text_muted", p.text_muted),
            ("accent", p.accent),
            ("accent_fill", p.accent_fill),
            ("accent_hover", p.accent_hover),
            ("accent_pressed", p.accent_pressed),
            ("accent_soft", p.accent_soft),
            ("on_fill", p.on_fill),
            ("success", p.success),
            ("success_soft", p.success_soft),
            ("warning", p.warning),
            ("warning_soft", p.warning_soft),
            ("danger", p.danger),
            ("danger_fill", p.danger_fill),
            ("danger_hover", p.danger_hover),
            ("danger_soft", p.danger_soft),
            ("info", p.info),
            ("info_soft", p.info_soft),
        ]
    }

    #[test]
    fn every_token_is_opaque_and_the_themes_differ() {
        for (dark, p) in [(false, &LIGHT), (true, &DARK)] {
            for (name, c) in named(p) {
                assert_eq!(c.a(), 255, "{name} (dark={dark}) must be opaque");
            }
            assert!(p.shadow.a() > 0, "shadow (dark={dark}) must be visible");
        }
        for ((name, l), (_, d)) in named(&LIGHT).into_iter().zip(named(&DARK)) {
            if name != "on_fill" {
                assert_ne!(l, d, "{name} must be tuned per theme");
            }
        }
        // Light really is light and dark really is dark.
        assert!(luminance(LIGHT.bg) > 0.8);
        assert!(luminance(DARK.bg) < 0.02);
    }

    #[test]
    fn text_tokens_keep_aa_contrast_on_every_surface() {
        for (dark, p) in [(false, &LIGHT), (true, &DARK)] {
            let surfaces = [
                ("bg", p.bg),
                ("surface", p.surface),
                ("surface_alt", p.surface_alt),
                ("surface_hover", p.surface_hover),
                ("input", p.input),
            ];
            let texts = [
                ("text", p.text),
                ("text_muted", p.text_muted),
                ("accent", p.accent),
                ("success", p.success),
                ("warning", p.warning),
                ("danger", p.danger),
                ("info", p.info),
            ];
            for (sn, s) in surfaces {
                for (tn, t) in texts {
                    let c = contrast(t, s);
                    assert!(c >= 4.5, "{tn} on {sn} (dark={dark}): {c:.2} < 4.5");
                }
            }
        }
    }

    #[test]
    fn tone_text_keeps_aa_contrast_on_its_own_tint_and_fills_carry_on_fill() {
        let tones = [
            Tone::Neutral,
            Tone::Accent,
            Tone::Success,
            Tone::Warning,
            Tone::Danger,
            Tone::Info,
        ];
        for dark in [false, true] {
            let p = palette(dark);
            for tone in tones {
                let (fg, tint) = tone.colors(dark);
                let c = contrast(fg, tint);
                assert!(c >= 4.5, "{tone:?} on its tint (dark={dark}): {c:.2}");
            }
            for (name, fill) in [
                ("accent_fill", p.accent_fill),
                ("accent_hover", p.accent_hover),
                ("accent_pressed", p.accent_pressed),
                ("danger_fill", p.danger_fill),
                ("danger_hover", p.danger_hover),
            ] {
                let c = contrast(p.on_fill, fill);
                assert!(c >= 4.5, "on_fill on {name} (dark={dark}): {c:.2}");
            }
        }
    }

    #[test]
    fn hero_and_avatar_colors_keep_aa_contrast() {
        for dark in [false, true] {
            let p = palette(dark);
            let (top, bottom) = hero_fill(dark);
            for (name, c) in [("hero top", top), ("hero bottom", bottom)] {
                let ratio = contrast(p.on_fill, c);
                assert!(ratio >= 4.5, "on_fill on {name} (dark={dark}): {ratio:.2}");
            }
            for h in 0..6u32 {
                let (fg, bg) = avatar_colors(h, dark);
                let ratio = contrast(fg, bg);
                assert!(ratio >= 4.5, "avatar {h} (dark={dark}): {ratio:.2}");
            }
        }
        // The initial is the first letter, upper-cased, Arabic kept as is.
        let first = |n: &str| {
            n.chars()
                .find(|c| c.is_alphanumeric())
                .map(|c| c.to_uppercase().collect::<String>())
        };
        assert_eq!(first("office-pc").as_deref(), Some("O"));
        assert_eq!(first("حاسوب المكتبة").as_deref(), Some("ح"));
        assert_eq!(first("  -- ").as_deref(), None);
    }

    #[test]
    fn contrast_matches_known_values() {
        let c = contrast(Color32::BLACK, Color32::WHITE);
        assert!((c - 21.0).abs() < 0.01, "{c}");
        assert!((contrast(Color32::WHITE, Color32::WHITE) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn spacing_scale_is_increasing() {
        let scale = [space::XS, space::S, space::M, space::L, space::XL];
        assert!(scale.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(scale, [4.0, 8.0, 12.0, 16.0, 24.0]);
    }
}
