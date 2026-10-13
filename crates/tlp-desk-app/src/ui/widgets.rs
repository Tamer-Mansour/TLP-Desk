//! Small reusable widgets that know about RTL.

use std::sync::Arc;

use eframe::egui::{self, text::LayoutJob, Align, Galley, Id, Response, TextBuffer, Ui};

use super::icons::{self, Icon};
use super::theme;
use crate::i18n::{has_rtl, shape_for_edit, BaseDir, Tx, S};

/// Single-line text field that stores **logical-order UTF-8** and handles Arabic as well as
/// egui allows:
///
/// * not focused: the text is shown in correct visual order via a custom layouter (the display
///   string is a permutation of the input, so egui's cursor indices stay valid);
/// * focused: egui edits the raw logical text. egui still shapes each Arabic word (joined
///   letters), but words are laid out left-to-right and the caret moves left-to-right — egui
///   has no bidi caret. Call [`edit_preview`] to show the correctly ordered text underneath.
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
    let focused = ui.memory(|m| m.has_focus(id));
    let mut layouter = |ui: &Ui, buf: &dyn TextBuffer, wrap_width: f32| -> Arc<Galley> {
        let raw = buf.as_str();
        let text = if focused {
            None
        } else {
            shape_for_edit(raw, BaseDir::Auto)
        };
        let font = egui::TextStyle::Body.resolve(ui.style());
        let mut job = LayoutJob::simple_singleline(
            text.unwrap_or_else(|| raw.to_owned()),
            font,
            ui.visuals().text_color(),
        );
        job.wrap.max_width = wrap_width;
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let resp = ui.add(
        egui::TextEdit::singleline(value)
            .id(id)
            .hint_text(hint)
            .desired_width(width)
            .horizontal_align(if tx.rtl() { Align::Max } else { Align::Min })
            .layouter(&mut layouter),
    );
    resp
}

/// Under a focused [`text_field`] containing Arabic, show how the text will look (egui edits
/// it in raw logical order, see [`text_field`]).
pub fn edit_preview(ui: &mut Ui, tx: Tx, field: &Response, value: &str) {
    if field.has_focus() && has_rtl(value) {
        let preview = format!("{} {}", tx.raw(S::InputPreview), value);
        theme::column(ui, tx.rtl(), |ui| {
            ui.label(theme::caption(ui, &tx.p(&preview)))
        });
    }
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
