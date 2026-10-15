//! The banner window: an immediate egui viewport, topmost, undecorated, without a taskbar
//! button and not activated when it appears (it never steals the host user's focus).

use eframe::egui::{self, RichText, ViewportBuilder, ViewportClass, ViewportId, WindowLevel};

use super::BannerModel;
use crate::backend::SessionId;
use crate::i18n::{Tx, S};
use crate::ui::icons::{self, Icon};
use crate::ui::theme;

/// Window width in points.
const WIDTH: f32 = 460.0;
/// Height of one row in points.
const ROW_HEIGHT: f32 = 34.0;
/// Gap between the banner and the top edge of the screen.
const TOP_GAP: f32 = 6.0;

/// Window size for `model`.
pub fn size(model: &BannerModel) -> egui::Vec2 {
    let lines = model.lines().max(1) as f32;
    egui::vec2(WIDTH, lines * ROW_HEIGHT + 12.0)
}

/// Top-center of a monitor of `monitor` points (or near the top-left when unknown).
pub fn position(monitor: Option<egui::Vec2>, size: egui::Vec2) -> egui::Pos2 {
    match monitor {
        Some(m) if m.x > size.x => egui::pos2(((m.x - size.x) / 2.0).round(), TOP_GAP),
        _ => egui::pos2(TOP_GAP, TOP_GAP),
    }
}

/// The text of one row: "<name> (<ID>) is connected to this device", or the ID alone when the
/// peer sent no name.
fn row_text(tx: Tx, name: &str, device_id: &str) -> String {
    let id = tx.digits(device_id);
    let who = if name.is_empty() {
        id
    } else {
        format!("{name} ({id})")
    };
    tx.f(S::HostBanner, &[("name", &who)])
}

/// Show the banner for `model`. Returns the session whose End button was clicked.
pub fn show(ctx: &egui::Context, tx: Tx, model: &BannerModel) -> Option<SessionId> {
    let size = size(model);
    let monitor = ctx.input(|i| i.viewport().monitor_size);
    let title = tx.raw(S::AppTitle);
    let builder = ViewportBuilder::default()
        .with_title(title)
        .with_decorations(false)
        .with_resizable(false)
        .with_taskbar(false)
        .with_active(false)
        .with_window_level(WindowLevel::AlwaysOnTop)
        .with_inner_size(size)
        .with_position(position(monitor, size));
    ctx.show_viewport_immediate(
        ViewportId::from_hash_of("tlp-host-banner"),
        builder,
        |ui, class| {
            if class == ViewportClass::EmbeddedWindow {
                // No native multi-window support: draw it as an in-app window instead.
                let mut end = None;
                egui::Window::new(title)
                    .title_bar(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, TOP_GAP))
                    .show(ui.ctx(), |ui| end = contents(ui, tx, model));
                return end;
            }
            let dark = ui.visuals().dark_mode;
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::new()
                        .fill(theme::accent(dark))
                        .inner_margin(egui::Margin::symmetric(12, 6)),
                )
                .show(ui, |ui| contents(ui, tx, model))
                .inner
        },
    )
}

fn contents(ui: &mut egui::Ui, tx: Tx, model: &BannerModel) -> Option<SessionId> {
    let mut end = None;
    for row in &model.rows {
        theme::row(ui, tx.rtl(), |ui| {
            ui.set_min_height(ROW_HEIGHT - 6.0);
            icons::icon(ui, tx, Icon::ViewOnly, 18.0, egui::Color32::WHITE);
            ui.label(
                RichText::new(row_text(tx, &row.name, &row.device_id))
                    .color(egui::Color32::WHITE)
                    .strong(),
            );
            theme::row_end(ui, tx.rtl(), |ui| {
                let b = icons::icon_text_button(tx, Icon::EndSession, tx.t(S::EndSession));
                if ui.add(b).clicked() {
                    end = Some(row.session);
                }
            });
        });
    }
    if model.more > 0 {
        let more = tx.f(S::BannerMore, &[("n", &tx.digits(&model.more.to_string()))]);
        ui.label(RichText::new(more).color(egui::Color32::WHITE));
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::banner::BannerRow;
    use crate::i18n::{DigitStyle, Lang};

    fn model(rows: usize, more: usize) -> BannerModel {
        BannerModel {
            rows: (0..rows)
                .map(|i| BannerRow {
                    session: SessionId(i as u64),
                    name: "سارة".into(),
                    device_id: "482 913 057".into(),
                })
                .collect(),
            more,
        }
    }

    #[test]
    fn banner_is_top_center_and_grows_per_row() {
        let one = size(&model(1, 0));
        let three = size(&model(2, 1));
        assert!(three.y > one.y);
        let p = position(Some(egui::vec2(1920.0, 1080.0)), one);
        assert_eq!(p, egui::pos2(((1920.0 - WIDTH) / 2.0).round(), TOP_GAP));
        assert_eq!(position(None, one), egui::pos2(TOP_GAP, TOP_GAP));
        assert_eq!(
            position(Some(egui::vec2(300.0, 200.0)), one),
            egui::pos2(TOP_GAP, TOP_GAP),
            "tiny screen"
        );
    }

    #[test]
    fn row_text_names_the_viewer_with_its_id() {
        let en = Tx::new(Lang::En, DigitStyle::Western);
        assert_eq!(
            row_text(en, "Office", "482 913 057"),
            "Office (482 913 057) is connected to this device"
        );
        assert!(row_text(en, "", "482 913 057").starts_with("482 913 057"));
        let ar = Tx::new(Lang::Ar, DigitStyle::ArabicIndic);
        let t = row_text(ar, "سارة", "482 913 057");
        // Display order is egui's (see i18n::shaping); check content, not order.
        assert!(
            t.contains("سارة") && t.contains('٤') && !t.contains('4'),
            "{t:?}"
        );
    }
}
