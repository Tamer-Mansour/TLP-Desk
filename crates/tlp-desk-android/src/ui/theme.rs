//! Fonts and touch-friendly spacing.

use std::sync::Arc;

use eframe::egui;

/// egui's default fonts plus the desktop app's Noto Sans Arabic as a fallback in every family
/// (egui shapes Arabic with its own harfrust pass; the bidi reordering is `i18n`'s job).
pub fn font_definitions() -> egui::FontDefinitions {
    const ARABIC: &[u8] =
        include_bytes!("../../../tlp-desk-app/assets/fonts/NotoSansArabic-Regular.ttf");
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "noto-sans-arabic".to_owned(),
        Arc::new(egui::FontData::from_static(ARABIC)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("noto-sans-arabic".to_owned());
    }
    fonts
}

/// Installs the fonts and big touch targets.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());
    ctx.global_style_mut(|style| {
        style.spacing.button_padding = egui::vec2(18.0, 12.0);
        style.spacing.item_spacing = egui::vec2(12.0, 12.0);
        style.spacing.interact_size.y = 44.0;
    });
}
