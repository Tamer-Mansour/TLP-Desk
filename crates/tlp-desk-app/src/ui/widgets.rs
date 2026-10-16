//! Small reusable widgets that know about RTL.

use eframe::egui::{self, Align, Id, Response, Ui};

use super::bidi_edit::BidiEdit;
use super::icons::{self, Icon};
use super::theme;
use crate::i18n::{has_rtl, Tx, S};

/// Single-line text field that stores **logical-order UTF-8** and handles Arabic properly.
///
/// Text without right-to-left characters is edited by egui's own `TextEdit`. As soon as the
/// text contains Arabic (or Hebrew, ...) the field is edited by [`BidiEdit`] instead: words in
/// visual order, a caret that moves with the arrow keys on screen, selection, Backspace/Delete
/// by letter. The caret and focus carry over when the field switches between the two.
///
/// The field is right-aligned in RTL UI.
pub fn text_field(
    ui: &mut Ui,
    tx: Tx,
    id_salt: &str,
    value: &mut String,
    hint: &str,
    width: f32,
) -> Response {
    let id = ui.make_persistent_id(id_salt);
    if has_rtl(value) {
        return BidiEdit::new(value, id)
            .width(width)
            .default_rtl(tx.rtl())
            .show(ui);
    }
    ui.add(
        egui::TextEdit::singleline(value)
            .id(id)
            .hint_text(hint)
            .desired_width(width)
            .horizontal_align(if tx.rtl() { Align::Max } else { Align::Min }),
    )
}

/// Icon-only copy button with persisted-per-id "Copied" feedback: the icon turns into a check
/// mark in the success color and the tooltip says "Copied" for a moment.
pub fn copy_button(ui: &mut Ui, tx: Tx, id: Id, value_for_clipboard: &str) {
    let now = ui.input(|i| i.time);
    let copied_at: Option<f64> = ui.data(|d| d.get_temp(id));
    let recently = copied_at.is_some_and(|t| now - t < 1.5);
    let resp = if recently {
        let dark = ui.visuals().dark_mode;
        let image = Icon::Copied.image(icons::TOOL_ICON, theme::success(dark));
        let resp = ui.add(egui::Button::image(image));
        let label = tx.t(S::Copied);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
        resp.on_hover_text(label)
    } else {
        icons::icon_button(ui, tx, Icon::Copy, tx.t(S::Copy))
    };
    if resp.clicked() {
        ui.ctx().copy_text(value_for_clipboard.to_owned());
        ui.data_mut(|d| d.insert_temp(id, now));
    }
    if recently {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(300));
    }
}

/// Filled call-to-action button with an icon at the reading start of its text.
pub fn primary_icon_button<'a>(ui: &Ui, tx: Tx, icon: Icon, text: &str) -> egui::Button<'a> {
    let dark = ui.visuals().dark_mode;
    let text = egui::RichText::new(text.to_owned())
        .color(egui::Color32::WHITE)
        .strong();
    icons::icon_text_colored(tx, icon, text, egui::Color32::WHITE).fill(theme::accent(dark))
}

/// Filled red button (disconnect, dangerous confirmation) with an icon.
pub fn danger_icon_button<'a>(ui: &Ui, tx: Tx, icon: Icon, text: &str) -> egui::Button<'a> {
    let dark = ui.visuals().dark_mode;
    let text = egui::RichText::new(text.to_owned())
        .color(egui::Color32::WHITE)
        .strong();
    icons::icon_text_colored(tx, icon, text, egui::Color32::WHITE).fill(theme::danger(dark))
}

/// A heading with a small accent icon at its reading start.
pub fn icon_heading(ui: &mut Ui, tx: Tx, icon: Icon, text: &str) {
    theme::row(ui, tx.rtl(), |ui| {
        let accent = theme::accent(ui.visuals().dark_mode);
        icons::icon(ui, tx, icon, 20.0, accent);
        ui.label(egui::RichText::new(text).heading());
    });
}
