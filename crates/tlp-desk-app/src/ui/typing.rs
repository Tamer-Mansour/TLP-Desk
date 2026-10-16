//! Settings card for how typed characters reach the remote PC (see `crate::text_route`).

use eframe::egui;

use super::icons::Icon;
use super::{theme, widgets};
use crate::i18n::{Tx, S};
use crate::settings::Settings;

/// "Type text using my keyboard layout" switch; saved at once.
pub fn settings_card(ui: &mut egui::Ui, tx: Tx, settings: &mut Settings) {
    theme::card(ui, |ui| {
        theme::column(ui, tx.rtl(), |ui| {
            widgets::icon_heading(ui, tx, Icon::Keyboard, tx.t(S::TypingSettings));
            ui.add_space(6.0);
            if super::settings::check_row(
                ui,
                tx,
                &mut settings.type_with_my_layout,
                tx.t(S::TypeWithMyLayout),
            ) {
                settings.save();
            }
            ui.label(theme::caption(ui, tx.t(S::TypeWithMyLayoutHint)));
        });
    });
}
