//! Fonts, colors, spacing and small layout helpers (including RTL mirroring).

use std::sync::Arc;

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Layout,
    Margin, RichText, Stroke, TextStyle, TextWrapMode, Theme, Ui,
};

use crate::i18n::Lang;

/// Noto Sans Arabic Regular, SIL Open Font License 1.1 (see
/// `assets/fonts/NotoSansArabic-OFL.txt`). Downloaded from the official Noto project
/// distribution (github.com/notofonts).
const ARABIC_FONT: &[u8] = include_bytes!("../../assets/fonts/NotoSansArabic-Regular.ttf");

/// Brand accent (teal-blue), tuned per theme for contrast.
pub fn accent(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(0x3D, 0xB5, 0xA6)
    } else {
        Color32::from_rgb(0x0E, 0x7C, 0x70)
    }
}

pub fn success(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(0x5C, 0xD0, 0x8A)
    } else {
        Color32::from_rgb(0x1F, 0x9D, 0x55)
    }
}

pub fn danger(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(0xE5, 0x6B, 0x6B)
    } else {
        Color32::from_rgb(0xC0, 0x39, 0x2B)
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

    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::proportional(22.0)),
            (TextStyle::Body, FontId::proportional(15.0)),
            (TextStyle::Button, FontId::proportional(15.0)),
            (TextStyle::Small, FontId::proportional(12.5)),
            (TextStyle::Monospace, FontId::monospace(14.0)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(14.0, 6.0);
        style.spacing.interact_size.y = 30.0;
        let r = CornerRadius::same(8);
        let w = &mut style.visuals.widgets;
        for ws in [
            &mut w.noninteractive,
            &mut w.inactive,
            &mut w.hovered,
            &mut w.active,
            &mut w.open,
        ] {
            ws.corner_radius = r;
        }
        style.visuals.window_corner_radius = CornerRadius::same(14);
    });
    for theme in [Theme::Light, Theme::Dark] {
        let dark = theme == Theme::Dark;
        ctx.style_mut_of(theme, |style| {
            let v = &mut style.visuals;
            v.selection.bg_fill = accent(dark);
            v.selection.stroke =
                Stroke::new(1.5, if dark { Color32::BLACK } else { Color32::WHITE });
            v.hyperlink_color = accent(dark);
            if dark {
                v.panel_fill = Color32::from_rgb(0x17, 0x1A, 0x1F);
                v.window_fill = Color32::from_rgb(0x20, 0x24, 0x2B);
                v.faint_bg_color = Color32::from_rgb(0x1F, 0x23, 0x2A);
                v.extreme_bg_color = Color32::from_rgb(0x11, 0x13, 0x17);
            } else {
                v.panel_fill = Color32::from_rgb(0xF3, 0xF5, 0xF7);
                v.window_fill = Color32::WHITE;
                v.faint_bg_color = Color32::WHITE;
                v.extreme_bg_color = Color32::from_rgb(0xF7, 0xF8, 0xFA);
            }
        });
    }
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

/// Rounded card container.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let dark = ui.visuals().dark_mode;
    egui::Frame::new()
        .fill(ui.visuals().window_fill)
        .stroke(Stroke::new(
            1.0,
            if dark {
                Color32::from_gray(0x33)
            } else {
                Color32::from_gray(0xE1)
            },
        ))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(Margin::same(18))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// Small, muted caption text.
pub fn caption(ui: &Ui, text: &str) -> RichText {
    RichText::new(text)
        .small()
        .color(ui.visuals().weak_text_color())
}

/// Filled call-to-action button.
pub fn primary_button(ui: &Ui, text: &str) -> egui::Button<'static> {
    let dark = ui.visuals().dark_mode;
    egui::Button::new(
        RichText::new(text.to_owned())
            .color(Color32::WHITE)
            .strong(),
    )
    .fill(accent(dark))
}

/// Paint a small status dot and reserve its space.
pub fn status_dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.5, color);
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
