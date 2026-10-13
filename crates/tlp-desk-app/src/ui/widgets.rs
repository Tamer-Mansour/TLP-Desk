//! Small reusable widgets that know about RTL.

use std::sync::Arc;

use eframe::egui::{self, text::LayoutJob, Align, Galley, Id, Response, TextBuffer, Ui};

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

/// Persisted-per-id "Copied" feedback.
pub fn copy_button(ui: &mut Ui, tx: Tx, id: Id, value_for_clipboard: &str) {
    let now = ui.input(|i| i.time);
    let copied_at: Option<f64> = ui.data(|d| d.get_temp(id));
    let recently = copied_at.is_some_and(|t| now - t < 1.5);
    let label = if recently {
        tx.t(S::Copied)
    } else {
        tx.t(S::Copy)
    };
    if ui.button(label).clicked() {
        ui.ctx().copy_text(value_for_clipboard.to_owned());
        ui.data_mut(|d| d.insert_temp(id, now));
    }
    if recently {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(300));
    }
}

/// Button with a direction chevron that mirrors with the UI direction: a "forward" chevron
/// sits at the reading end and points that way (right in English, left in Arabic); a "back"
/// chevron sits at the reading start and points against the reading direction. The chevron is
/// painted, not a font glyph, so it never depends on font coverage or glyph mirroring.
pub fn chevron_button(
    ui: &mut Ui,
    tx: Tx,
    text: &str,
    forward: bool,
    primary: bool,
    enabled: bool,
) -> Response {
    // Pad the arrow side with spaces to make room for the painted chevron.
    let points_right = forward != tx.rtl();
    // Forward sits at the reading end, back at the reading start: both on the side they point to.
    let at_right = points_right;
    let label = if at_right {
        format!("{text}    ")
    } else {
        format!("    {text}")
    };
    let button = if primary {
        theme::primary_button(ui, &label)
    } else {
        egui::Button::new(label)
    };
    let resp = ui.add_enabled(enabled, button);
    let color = if primary {
        egui::Color32::WHITE
    } else {
        ui.visuals().widgets.style(&resp).fg_stroke.color
    };
    let r = resp.rect;
    let pad = ui.spacing().button_padding.x;
    let cx = if at_right {
        r.right() - pad - 5.0
    } else {
        r.left() + pad + 5.0
    };
    let cy = r.center().y;
    let (dx, dy) = (if points_right { 3.5 } else { -3.5 }, 5.0);
    let stroke = egui::Stroke::new(1.8, color);
    let tip = egui::pos2(cx + dx, cy);
    ui.painter()
        .line_segment([egui::pos2(cx - dx, cy - dy), tip], stroke);
    ui.painter()
        .line_segment([egui::pos2(cx - dx, cy + dy), tip], stroke);
    resp
}
