//! Small reusable widgets that know about RTL and use the design tokens of [`theme`]:
//! button variants, text fields with a focus ring, switches, chips, a segmented control,
//! hoverable list rows, section headers, toasts and empty states.

use eframe::egui::{
    self, Align, Color32, CornerRadius, Id, Margin, Response, RichText, Sense, Stroke, Ui,
};

use super::bidi_edit::BidiEdit;
use super::icons::{self, Icon};
use super::theme::{self, font, radius, space, Tone};
use crate::i18n::{has_rtl, Tx, S};

/// Single-line text field that stores **logical-order UTF-8** and handles Arabic properly.
///
/// Text without right-to-left characters is edited by egui's own `TextEdit`. As soon as the
/// text contains Arabic (or Hebrew, ...) the field is edited by [`BidiEdit`] instead: words in
/// visual order, a caret that moves with the arrow keys on screen, selection, Backspace/Delete
/// by letter. The caret and focus carry over when the field switches between the two.
///
/// The field is right-aligned in RTL UI and shows an accent focus ring while focused.
pub fn text_field(
    ui: &mut Ui,
    tx: Tx,
    id_salt: &str,
    value: &mut String,
    hint: &str,
    width: f32,
) -> Response {
    let id = ui.make_persistent_id(id_salt);
    let resp = if has_rtl(value) {
        BidiEdit::new(value, id)
            .width(width)
            .default_rtl(tx.rtl())
            .show(ui)
    } else {
        ui.add(
            egui::TextEdit::singleline(value)
                .id(id)
                .hint_text(hint)
                .desired_width(width)
                .margin(Margin::symmetric(10, 6))
                .horizontal_align(if tx.rtl() { Align::Max } else { Align::Min }),
        )
    };
    focus_ring(ui, &resp);
    resp
}

/// Paint the accent focus ring around a focused input.
pub fn focus_ring(ui: &Ui, resp: &Response) {
    if resp.has_focus() {
        let p = theme::tokens(ui);
        ui.painter().rect_stroke(
            resp.rect.expand(1.0),
            CornerRadius::same(radius::MD),
            Stroke::new(2.0, p.accent),
            egui::StrokeKind::Outside,
        );
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
        let resp = ui.add(egui::Button::image(image).frame(false));
        let label = tx.t(S::Copied);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
        resp.on_hover_text(label)
    } else {
        ghost_scope(ui, |ui| {
            icons::icon_button(ui, tx, Icon::Copy, tx.t(S::Copy))
        })
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

/// Button emphasis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    /// The one main action of a view: filled accent.
    Primary,
    /// Everything else: quiet filled surface with a hairline.
    Secondary,
    /// Toolbar and inline actions: no frame until hovered.
    Ghost,
    /// Destructive: filled danger.
    Danger,
}

/// Hover / pressed state of the widget that will get the next auto id, from the last frame.
/// Lets a filled button change its own color on hover even though egui's `Button::fill`
/// overrides the state colors.
fn next_state(ui: &Ui) -> (bool, bool) {
    let id = ui.next_auto_id();
    ui.ctx()
        .read_response(id)
        .map(|r| (r.hovered(), r.is_pointer_button_down_on()))
        .unwrap_or_default()
}

/// A filled button in `fill` with hover / pressed variants and `on_fill` text.
fn filled_button<'a>(
    ui: &Ui,
    tx: Tx,
    icon: Icon,
    text: &str,
    fills: [Color32; 3],
) -> egui::Button<'a> {
    let p = theme::tokens(ui);
    let (hovered, pressed) = next_state(ui);
    let [base, hover, down] = fills;
    let fill = if pressed {
        down
    } else if hovered {
        hover
    } else {
        base
    };
    let text = egui::RichText::new(text.to_owned())
        .color(p.on_fill)
        .strong();
    icons::icon_text_colored(tx, icon, text, p.on_fill)
        .fill(fill)
        .stroke(Stroke::NONE)
}

/// Filled call-to-action button with an icon at the reading start of its text.
pub fn primary_icon_button<'a>(ui: &Ui, tx: Tx, icon: Icon, text: &str) -> egui::Button<'a> {
    let p = theme::tokens(ui);
    filled_button(
        ui,
        tx,
        icon,
        text,
        [p.accent_fill, p.accent_hover, p.accent_pressed],
    )
}

/// Filled red button (disconnect, dangerous confirmation) with an icon.
pub fn danger_icon_button<'a>(ui: &Ui, tx: Tx, icon: Icon, text: &str) -> egui::Button<'a> {
    let p = theme::tokens(ui);
    filled_button(
        ui,
        tx,
        icon,
        text,
        [p.danger_fill, p.danger_hover, p.danger_hover],
    )
}

/// A button of any [`ButtonKind`] with an icon at the reading start of its text.
pub fn button(ui: &mut Ui, tx: Tx, kind: ButtonKind, icon: Icon, text: &str) -> Response {
    match kind {
        ButtonKind::Primary => {
            let b = primary_icon_button(ui, tx, icon, text);
            ui.add(b)
        }
        ButtonKind::Danger => {
            let b = danger_icon_button(ui, tx, icon, text);
            ui.add(b)
        }
        ButtonKind::Secondary => ui.add(icons::icon_text_button(tx, icon, text)),
        ButtonKind::Ghost => ghost_scope(ui, |ui| ui.add(icons::icon_text_button(tx, icon, text))),
    }
}

/// Run `add` with frameless-until-hovered button visuals (ghost buttons).
pub fn ghost_scope<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.scope(|ui| {
        let w = &mut ui.visuals_mut().widgets;
        w.inactive.weak_bg_fill = Color32::TRANSPARENT;
        w.inactive.bg_stroke = Stroke::NONE;
        w.hovered.bg_stroke = Stroke::NONE;
        w.active.bg_stroke = Stroke::NONE;
        add(ui)
    })
    .inner
}

/// A heading with a small accent icon at its reading start.
pub fn icon_heading(ui: &mut Ui, tx: Tx, icon: Icon, text: &str) {
    theme::row(ui, tx.rtl(), |ui| {
        icon_badge(ui, tx, icon, Tone::Accent);
        ui.label(
            egui::RichText::new(text)
                .size(font::HEADING)
                .color(theme::tokens(ui).text),
        );
    });
}

/// Section header of a panel: a small tracked eyebrow label, an optional caption under it.
/// The icon is kept in the signature for call sites but no longer drawn: eyebrows label the
/// group; icons went to the rows and buttons where they mean something.
pub fn section_header(ui: &mut Ui, tx: Tx, _icon: Icon, title: &str, subtitle: Option<&str>) {
    theme::row(ui, tx.rtl(), |ui| {
        ui.label(theme::eyebrow(ui, title));
    });
    if let Some(sub) = subtitle {
        ui.label(theme::caption(ui, sub));
    }
    ui.add_space(space::S);
}

/// A round avatar with the device's initial (see [`theme::paint_avatar`]).
pub fn avatar(ui: &mut Ui, name: &str, size: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), Sense::hover());
    if ui.is_rect_visible(rect) {
        theme::paint_avatar(ui.painter(), rect, name, ui.visuals().dark_mode);
    }
    resp
}

/// Icon-only button meant for the hero tile: white glyph, frameless until hovered, hover
/// shows as a translucent white pill so it works on the teal fill.
pub fn hero_icon_button(ui: &mut Ui, tx: Tx, icon: Icon, tooltip: &str) -> Response {
    let image = icon.image_dir(tx.rtl(), icons::TOOL_ICON, Color32::WHITE);
    let resp = ui
        .scope(|ui| {
            let w = &mut ui.visuals_mut().widgets;
            w.inactive.weak_bg_fill = Color32::TRANSPARENT;
            w.inactive.bg_stroke = Stroke::NONE;
            w.hovered.weak_bg_fill = Color32::from_white_alpha(0x30);
            w.hovered.bg_fill = Color32::from_white_alpha(0x30);
            w.hovered.bg_stroke = Stroke::NONE;
            w.active.weak_bg_fill = Color32::from_white_alpha(0x48);
            w.active.bg_fill = Color32::from_white_alpha(0x48);
            w.active.bg_stroke = Stroke::NONE;
            ui.add(egui::Button::image(image).corner_radius(CornerRadius::same(radius::PILL)))
        })
        .inner;
    let enabled = resp.enabled();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, tooltip));
    resp.on_hover_text(tooltip)
}

/// Copy button for the hero tile ([`copy_button`] in white, with the same "Copied" feedback).
pub fn hero_copy_button(ui: &mut Ui, tx: Tx, id: Id, value_for_clipboard: &str) {
    let now = ui.input(|i| i.time);
    let copied_at: Option<f64> = ui.data(|d| d.get_temp(id));
    let recently = copied_at.is_some_and(|t| now - t < 1.5);
    let (icon, label) = if recently {
        (Icon::Copied, tx.t(S::Copied))
    } else {
        (Icon::Copy, tx.t(S::Copy))
    };
    let resp = hero_icon_button(ui, tx, icon, label);
    if resp.clicked() {
        ui.ctx().copy_text(value_for_clipboard.to_owned());
        ui.data_mut(|d| d.insert_temp(id, now));
    }
    if recently {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(300));
    }
}

/// An icon on a small tinted rounded square (used before headings and in empty states).
pub fn icon_badge(ui: &mut Ui, tx: Tx, icon: Icon, tone: Tone) {
    let dark = ui.visuals().dark_mode;
    let (fg, bg) = tone.colors(dark);
    egui::Frame::new()
        .fill(bg)
        .corner_radius(CornerRadius::same(radius::MD))
        .inner_margin(Margin::same(5))
        .show(ui, |ui| {
            icons::icon(ui, tx, icon, 18.0, fg);
        });
}

/// Small rounded label with a tinted background (status, counts, tags).
pub fn chip(ui: &mut Ui, text: &str, tone: Tone) -> Response {
    let dark = ui.visuals().dark_mode;
    let (fg, bg) = tone.colors(dark);
    // Painted by hand (not a Frame) so the chip keeps its own height inside tall rows.
    let galley = egui::WidgetText::from(egui::RichText::new(text).size(font::CAPTION)).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Small,
    );
    let pad = egui::vec2(9.0, 3.0);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + 2.0 * pad, Sense::hover());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    if ui.is_rect_visible(rect) {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(radius::PILL), bg);
        ui.painter().galley(rect.min + pad, galley, fg);
    }
    resp
}

/// iOS-style switch with its label at the reading start. Reports itself as a checkbox to
/// accessibility (and to the headless UI tests), so `get_by_label(..).click()` toggles it.
pub fn switch(ui: &mut Ui, tx: Tx, on: &mut bool, label: &str) -> Response {
    let p = theme::tokens(ui);
    let track = egui::vec2(36.0, 20.0);
    let galley = egui::WidgetText::from(label).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Body,
    );
    let gap = space::M - 2.0;
    let size = egui::vec2(
        track.x + gap + galley.size().x,
        track
            .y
            .max(galley.size().y)
            .max(ui.spacing().interact_size.y - 6.0),
    );
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let enabled = ui.is_enabled();
    let value = *on;
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, value, label)
    });
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(resp.id, *on);
        let (track_rect, text_pos) = if tx.rtl() {
            let tr = egui::Rect::from_min_size(
                egui::pos2(rect.right() - track.x, rect.center().y - track.y / 2.0),
                track,
            );
            let tp = egui::pos2(
                tr.left() - gap - galley.size().x,
                rect.center().y - galley.size().y / 2.0,
            );
            (tr, tp)
        } else {
            let tr = egui::Rect::from_min_size(
                egui::pos2(rect.left(), rect.center().y - track.y / 2.0),
                track,
            );
            let tp = egui::pos2(tr.right() + gap, rect.center().y - galley.size().y / 2.0);
            (tr, tp)
        };
        let off = if resp.hovered() {
            p.border_strong
        } else {
            p.surface_pressed
        };
        let fill = lerp_color(off, p.accent_fill, t);
        let painter = ui.painter();
        painter.rect_filled(track_rect, CornerRadius::same(radius::PILL), fill);
        if t < 0.5 {
            painter.rect_stroke(
                track_rect,
                CornerRadius::same(radius::PILL),
                Stroke::new(1.0, p.border_strong),
                egui::StrokeKind::Inside,
            );
        }
        // In RTL the knob travels the other way: "on" is towards the reading end (left).
        let t_dir = if tx.rtl() { 1.0 - t } else { t };
        let knob_r = track.y / 2.0 - 3.0;
        let x = egui::lerp(
            (track_rect.left() + knob_r + 3.0)..=(track_rect.right() - knob_r - 3.0),
            t_dir,
        );
        let center = egui::pos2(x, track_rect.center().y);
        painter.circle_filled(center + egui::vec2(0.0, 0.5), knob_r + 0.5, p.shadow);
        painter.circle_filled(center, knob_r, Color32::WHITE);
        let color = if enabled { p.text } else { p.text_muted };
        painter.galley(text_pos, galley, color);
    }
    resp
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| {
        let v = f32::from(x) + (f32::from(y) - f32::from(x)) * t;
        v.round().clamp(0.0, 255.0) as u8
    };
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

/// Segmented control: a tinted track with one raised "pill" for the selected option.
/// Options flow from the reading start. Returns `true` when the selection changed.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    tx: Tx,
    value: &mut T,
    options: &[(T, Option<Icon>, &str)],
) -> bool {
    let p = theme::tokens(ui);
    let mut changed = false;
    egui::Frame::new()
        .fill(p.surface_alt)
        .corner_radius(CornerRadius::same(radius::MD + 4))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            theme::row(ui, tx.rtl(), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.spacing_mut().interact_size.y = 28.0;
                let v = ui.visuals_mut();
                v.selection.bg_fill = p.surface;
                v.selection.stroke = Stroke::new(1.0, p.accent);
                v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
                v.widgets.inactive.bg_stroke = Stroke::NONE;
                v.widgets.hovered.bg_stroke = Stroke::NONE;
                v.widgets.inactive.fg_stroke.color = p.text_muted;
                for (opt, icon, label) in options {
                    let selected = *value == *opt;
                    let b = match icon {
                        Some(icon) => icons::icon_text_selectable(tx, selected, *icon, *label),
                        None => egui::Button::selectable(selected, *label),
                    };
                    let b = if selected {
                        b.stroke(Stroke::NONE)
                            .corner_radius(CornerRadius::same(radius::MD + 1))
                    } else {
                        b.corner_radius(CornerRadius::same(radius::MD + 1))
                    };
                    let resp = ui.add(b);
                    if resp.clicked() && !selected {
                        *value = *opt;
                        changed = true;
                    }
                }
            });
        });
    changed
}

/// A list row: a quiet raised strip that lifts on hover (lighter fill, a hairline), full
/// width. `add` lays out the row content.
pub fn list_row<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> (R, Response) {
    let p = theme::tokens(ui);
    let bg = ui.painter().add(egui::Shape::Noop);
    let inner = egui::Frame::new()
        .inner_margin(Margin::symmetric(14, 9))
        .outer_margin(Margin::symmetric(0, 3))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        });
    let rect = inner.response.rect;
    let hovered = ui.rect_contains_pointer(rect);
    let r = CornerRadius::same(radius::LG);
    let shape = if hovered {
        egui::Shape::rect_filled(rect, r, p.surface_hover)
    } else {
        egui::Shape::rect_filled(rect, r, p.surface_alt)
    };
    ui.painter().set(bg, shape);
    if hovered {
        ui.painter().rect_stroke(
            rect,
            r,
            Stroke::new(1.0, p.border_strong),
            egui::StrokeKind::Inside,
        );
    }
    let resp = ui.interact(rect, inner.response.id.with("row"), Sense::hover());
    (inner.inner, resp)
}

/// A thin hairline divider with breathing room around it.
pub fn divider(ui: &mut Ui) {
    ui.add_space(space::S);
    let p = theme::tokens(ui);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, Stroke::new(1.0, p.border));
    ui.add_space(space::S);
}

/// Centered empty state: a tinted icon, a short title and a muted hint.
pub fn empty_state(ui: &mut Ui, tx: Tx, icon: Icon, title: &str, hint: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space(space::L);
        icon_badge(ui, tx, icon, Tone::Neutral);
        ui.add_space(space::S);
        ui.label(egui::RichText::new(title).color(theme::tokens(ui).text));
        if !hint.is_empty() {
            ui.label(theme::caption(ui, hint));
        }
        ui.add_space(space::L);
    });
}

/// Toast / banner frame in a semantic tone: tinted background, tone-colored hairline, float
/// shadow. Content is laid out by `add`; use [`toast_row`] for the common icon + text case.
pub fn toast_frame(ui: &Ui, tone: Tone) -> egui::Frame {
    let dark = ui.visuals().dark_mode;
    let p = theme::palette(dark);
    let (fg, tint) = tone.colors(dark);
    // Opaque: a toast floats over video and other content.
    let fill = if tone == Tone::Neutral {
        p.surface
    } else {
        tint
    };
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, fg.gamma_multiply(0.35)))
        .corner_radius(CornerRadius::same(radius::LG))
        .inner_margin(Margin::symmetric(14, 10))
        .shadow(theme::float_shadow(dark))
}

/// Icon in the tone color followed by the text, in reading order.
pub fn toast_row(ui: &mut Ui, tx: Tx, tone: Tone, icon: Icon, text: &str) {
    let dark = ui.visuals().dark_mode;
    let (fg, _) = tone.colors(dark);
    let text_color = theme::palette(dark).text;
    theme::row(ui, tx.rtl(), |ui| {
        icons::icon(ui, tx, icon, 18.0, fg);
        ui.label(egui::RichText::new(text).color(text_color));
    });
}

/// Shorten `s` to at most `max` characters (not bytes: Arabic stays intact), ending in "…".
/// Works on logical order, so the reading end is what gets cut in both directions.
pub fn elide(s: &str, max: usize) -> std::borrow::Cow<'_, str> {
    if s.chars().count() <= max {
        return s.into();
    }
    let keep: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{keep}…").into()
}

/// Elide `text` (in logical order) until `style(text)` fits in `max_w` points, so Arabic is
/// cut at its reading end too (egui's own truncation cuts the visual end of shaped text).
pub fn fit(ui: &egui::Ui, text: &str, max_w: f32, style: impl Fn(&str) -> RichText) -> String {
    let width = |s: &str| {
        egui::WidgetText::from(style(s))
            .into_galley(
                ui,
                Some(egui::TextWrapMode::Extend),
                f32::INFINITY,
                egui::TextStyle::Body,
            )
            .size()
            .x
    };
    if width(text) <= max_w {
        return text.to_owned();
    }
    let total = text.chars().count();
    // Largest prefix that fits (binary search: a handful of layouts per row at most).
    let (mut lo, mut hi) = (1usize, total);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if width(&elide(text, mid)) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    elide(text, lo).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elide_counts_characters_not_bytes() {
        assert_eq!(elide("Office-PC", 28), "Office-PC");
        assert_eq!(elide("abcdef", 4), "abc…");
        let ar = "حاسوب-المكتبة-الرئيسية-في-الطابق-الثاني";
        let short = elide(ar, 10);
        assert_eq!(short.chars().count(), 10);
        assert!(short.starts_with("حاسوب"));
        assert!(short.ends_with('…'));
    }

    #[test]
    fn lerp_color_hits_both_ends_and_clamps() {
        let a = Color32::from_rgb(0, 100, 200);
        let b = Color32::from_rgb(200, 100, 0);
        assert_eq!(lerp_color(a, b, 0.0), a);
        assert_eq!(lerp_color(a, b, 1.0), b);
        assert_eq!(lerp_color(a, b, 7.0), b);
        assert_eq!(lerp_color(a, b, 0.5), Color32::from_rgb(100, 100, 100));
    }
}
