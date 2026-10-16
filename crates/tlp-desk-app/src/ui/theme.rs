//! The TLP-Desk design system: color tokens for both themes, the spacing / radius / elevation
//! scales, the type scale, the egui style built from them, and small layout helpers (including
//! RTL mirroring).
//!
//! Screens never hard-code colors or sizes: they ask [`palette`] for a token (or one of the
//! shorthand functions such as [`accent`] and [`danger`]) and use [`space`] / [`radius`] for
//! paddings and corners. `docs/src/dev/design.md` lists every token and when to use it.

use std::sync::Arc;

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Layout,
    Margin, RichText, Shadow, Stroke, TextStyle, TextWrapMode, Theme, Ui,
};

use crate::i18n::Lang;

/// Noto Sans Arabic Regular, SIL Open Font License 1.1 (see
/// `assets/fonts/NotoSansArabic-OFL.txt`). Downloaded from the official Noto project
/// distribution (github.com/notofonts).
const ARABIC_FONT: &[u8] = include_bytes!("../../assets/fonts/NotoSansArabic-Regular.ttf");

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
    pub const TITLE: f32 = 20.0;
    /// Card headers.
    pub const HEADING: f32 = 17.0;
    /// Regular text and buttons.
    pub const BODY: f32 = 14.5;
    /// Captions, hints, meta lines.
    pub const CAPTION: f32 = 12.5;
    /// The big device ID on the home screen.
    pub const DISPLAY: f32 = 32.0;
    /// The one-time code next to it.
    pub const CODE: f32 = 24.0;
    /// IDs and IP addresses inside text.
    pub const MONO: f32 = 14.0;
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

/// Light theme tokens.
pub const LIGHT: Palette = Palette {
    bg: rgb(0xF2F4F7),
    surface: rgb(0xFFFFFF),
    surface_alt: rgb(0xF5F7FA),
    surface_hover: rgb(0xEBEFF3),
    surface_pressed: rgb(0xE1E6EC),
    input: rgb(0xFFFFFF),
    border: rgb(0xE1E5EA),
    border_strong: rgb(0xC6CED7),
    text: rgb(0x17202A),
    text_muted: rgb(0x586374),
    accent: rgb(0x0B7A6E),
    accent_fill: rgb(0x0B7A6E),
    accent_hover: rgb(0x096A60),
    accent_pressed: rgb(0x075A51),
    accent_soft: rgb(0xE0F2EF),
    on_fill: rgb(0xFFFFFF),
    success: rgb(0x16794A),
    success_soft: rgb(0xE2F4EA),
    warning: rgb(0x8F5300),
    warning_soft: rgb(0xFCF0DC),
    danger: rgb(0xB42318),
    danger_fill: rgb(0xC0291D),
    danger_hover: rgb(0xA52216),
    danger_soft: rgb(0xFDE9E7),
    info: rgb(0x1D5BAA),
    info_soft: rgb(0xE4EDFA),
    shadow: Color32::from_rgba_premultiplied(0x10, 0x18, 0x28, 0x1C),
};

/// Dark theme tokens.
pub const DARK: Palette = Palette {
    bg: rgb(0x111418),
    surface: rgb(0x1A1E24),
    surface_alt: rgb(0x22272F),
    surface_hover: rgb(0x2A3039),
    surface_pressed: rgb(0x323944),
    input: rgb(0x14171C),
    border: rgb(0x2B3139),
    border_strong: rgb(0x3D4551),
    text: rgb(0xE7EAEE),
    text_muted: rgb(0x9EA7B4),
    accent: rgb(0x4CC2B1),
    accent_fill: rgb(0x147A6E),
    accent_hover: rgb(0x117166),
    accent_pressed: rgb(0x0D5F56),
    accent_soft: rgb(0x163B37),
    on_fill: rgb(0xFFFFFF),
    success: rgb(0x5BCB8F),
    success_soft: rgb(0x173628),
    warning: rgb(0xEBB650),
    warning_soft: rgb(0x3A3018),
    danger: rgb(0xF28B82),
    danger_fill: rgb(0xB3362C),
    danger_hover: rgb(0xA03027),
    danger_soft: rgb(0x3E1F1D),
    info: rgb(0x7DB3F5),
    info_soft: rgb(0x1A2C44),
    shadow: Color32::from_rgba_premultiplied(0, 0, 0, 0x70),
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

/// Soft shadow under cards (level 1).
pub fn card_shadow(dark: bool) -> Shadow {
    Shadow {
        offset: [0, 1],
        blur: 6,
        spread: 0,
        color: if dark {
            Color32::from_black_alpha(0x40)
        } else {
            Color32::from_rgba_premultiplied(0x08, 0x0C, 0x14, 0x0E)
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

/// egui's default fonts plus the Arabic font as a fallback in every family. egui (epaint 0.36)
/// shapes each run with harfrust, so Arabic gets real OpenType shaping from this font.
pub fn font_definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "noto-sans-arabic".to_owned(),
        Arc::new(
            FontData::from_static(ARABIC_FONT).tweak(egui::epaint::text::FontTweak {
                scale: 1.08,
                ..Default::default()
            }),
        ),
    );
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("noto-sans-arabic".to_owned());
    }
    fonts
}

/// Install fonts and the visual style for both themes.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());
    super::icons::install(ctx);

    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::proportional(font::TITLE)),
            (TextStyle::Body, FontId::proportional(font::BODY)),
            (TextStyle::Button, FontId::proportional(font::BODY)),
            (TextStyle::Small, FontId::proportional(font::CAPTION)),
            (TextStyle::Monospace, FontId::monospace(font::MONO)),
        ]
        .into();
        let s = &mut style.spacing;
        s.item_spacing = egui::vec2(space::S + 2.0, space::S);
        s.button_padding = egui::vec2(space::M + 2.0, 7.0);
        s.interact_size.y = 32.0;
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

    let r = CornerRadius::same(radius::MD);
    let w = &mut v.widgets;
    // Labels, separators, card hairlines.
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface_alt;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r;
    // Resting buttons and inputs.
    w.inactive.bg_fill = p.surface_alt;
    w.inactive.weak_bg_fill = p.surface_alt;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = r;
    w.inactive.expansion = 0.0;
    // Hovered.
    w.hovered.bg_fill = p.surface_hover;
    w.hovered.weak_bg_fill = p.surface_hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.border_strong);
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

/// The card frame (surface, hairline border, soft shadow) without showing it.
pub fn card_frame(ui: &Ui) -> egui::Frame {
    let dark = ui.visuals().dark_mode;
    let p = palette(dark);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(radius::LG))
        .inner_margin(Margin::same(20))
        .shadow(card_shadow(dark))
}

/// Rounded card container (level-1 elevation).
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    card_frame(ui)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
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
