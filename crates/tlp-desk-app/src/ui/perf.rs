//! Session toolbar pieces for the performance lane: the quality menu and the "all monitors"
//! button.

use eframe::egui::{self, Popup, PopupCloseBehavior};

use super::icons::{self, Icon};
use super::settings::quality_key;
use crate::i18n::{Tx, S};
use crate::settings::QualityChoice;

/// The monitor index the host reports while it streams the whole virtual desktop.
pub const ALL_MONITORS: u32 = u32::MAX;

/// Icon button that opens the quality preset menu; returns the preset the user picked, if it
/// differs from `current`.
pub fn quality_menu(ui: &mut egui::Ui, tx: Tx, current: QualityChoice) -> Option<QualityChoice> {
    let button = icons::icon_button(ui, tx, Icon::Quality, tx.t(S::SettingsQuality));
    let mut picked = None;
    Popup::menu(&button)
        .close_behavior(PopupCloseBehavior::CloseOnClick)
        .show(|ui| {
            for q in QualityChoice::ALL {
                if ui
                    .selectable_label(q == current, tx.t(quality_key(q)))
                    .clicked()
                    && q != current
                {
                    picked = Some(q);
                }
            }
        });
    picked
}

/// The "All" toggle of the monitor picker (shows every monitor as one picture). Returns true
/// when the user asked for the all-monitors view.
pub fn all_monitors_button(ui: &mut egui::Ui, tx: Tx, active: bool) -> bool {
    icons::icon_toggle(ui, tx, active, Icon::AllMonitors, tx.t(S::AllScreens)).clicked() && !active
}
