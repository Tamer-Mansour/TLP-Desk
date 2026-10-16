//! "Incoming connections" card: on/off, default permissions, accept timeout, one-time code,
//! local-input grace and the remote pointer marker.

use eframe::egui;
use tlp_desk_proto::Permissions;

use super::check_row;
use crate::app::App;
use crate::i18n::format::to_western_digits;
use crate::i18n::{Tx, S};
use crate::settings::host::{ACCEPT_TIMEOUT_RANGE, GRACE_RANGE};
use crate::ui::icons::Icon;
use crate::ui::{theme, widgets};

/// Every permission bit with its label, in display order.
pub(crate) const PERMISSION_LABELS: [(Permissions, S); 12] = [
    (Permissions::VIEW, S::PermView),
    (Permissions::CONTROL, S::PermControl),
    (Permissions::KEYBOARD, S::PermKeyboard),
    (Permissions::CLIPBOARD, S::PermClipboard),
    (Permissions::FILES, S::PermFiles),
    (Permissions::AUDIO, S::PermAudio),
    (Permissions::CHAT, S::PermChat),
    (Permissions::WHITEBOARD, S::PermWhiteboard),
    (Permissions::TUNNEL, S::PermTunnel),
    (Permissions::PRIVACY_MODE, S::PermPrivacy),
    (Permissions::RESTART, S::PermRestart),
    (Permissions::SYSTEM, S::PermSystem),
];

/// A slider over `range` showing numbers in the user's digit style (accepts Arabic-Indic
/// digits when typed). Returns `true` when the value changed.
fn number_slider(
    ui: &mut egui::Ui,
    tx: Tx,
    value: &mut u32,
    range: std::ops::RangeInclusive<u32>,
    step: f64,
    unit: S,
) -> bool {
    let unit = tx.t(unit).to_owned();
    ui.add(
        egui::Slider::new(value, range)
            .step_by(step)
            .custom_formatter(move |v, _| tx.n(&format!("{v:.0}")))
            .custom_parser(|s| to_western_digits(s).trim().parse::<f64>().ok())
            .suffix(format!(" {unit}")),
    )
    .changed()
}

impl App {
    pub(super) fn incoming_card(&mut self, ui: &mut egui::Ui) {
        let tx = self.tx;
        let mut changed = false;
        let host = &mut self.settings.host;
        theme::card(ui, |ui| {
            theme::column(ui, tx.rtl(), |ui| {
                widgets::icon_heading(ui, tx, Icon::Shield, tx.t(S::SettingsIncoming));
                ui.add_space(6.0);
                changed |= check_row(ui, tx, &mut host.incoming_enabled, tx.t(S::IncomingEnabled));
                if !host.incoming_enabled {
                    let dark = ui.visuals().dark_mode;
                    ui.label(
                        egui::RichText::new(tx.t(S::IncomingPaused)).color(theme::danger(dark)),
                    );
                }
                ui.add_space(8.0);

                ui.label(egui::RichText::new(tx.t(S::DefaultPermissions)).strong());
                ui.label(theme::caption(ui, tx.t(S::DefaultPermissionsHint)));
                let half = PERMISSION_LABELS.len().div_ceil(2);
                ui.columns(2, |cols| {
                    let (first, second) = if tx.rtl() { (1, 0) } else { (0, 1) };
                    for (i, (bit, key)) in PERMISSION_LABELS.iter().enumerate() {
                        let col = if i < half { first } else { second };
                        let Some(ui) = cols.get_mut(col) else {
                            continue;
                        };
                        let mut on = host.default_permissions.0.contains(*bit);
                        // Viewing is what a session is; it cannot be switched off here.
                        let enabled = *bit != Permissions::VIEW;
                        ui.add_enabled_ui(enabled, |ui| {
                            if check_row(ui, tx, &mut on, tx.t(*key)) {
                                host.default_permissions.set(*bit, on);
                                changed = true;
                            }
                        });
                    }
                });
                ui.add_space(8.0);

                ui.label(theme::caption(ui, tx.t(S::AcceptTimeout)));
                theme::row(ui, tx.rtl(), |ui| {
                    changed |= number_slider(
                        ui,
                        tx,
                        &mut host.accept_timeout_secs,
                        ACCEPT_TIMEOUT_RANGE,
                        5.0,
                        S::UnitSeconds,
                    );
                });
                changed |= check_row(
                    ui,
                    tx,
                    &mut host.allow_one_time_code,
                    tx.t(S::AllowOneTimeCode),
                );
                // Accept window (TLP-accept-popup).
                let popup = &mut self.settings.accept_popup;
                changed |= check_row(ui, tx, &mut popup.enabled, tx.t(S::PopupEnabled));
                ui.add_enabled_ui(popup.enabled, |ui| {
                    changed |= check_row(ui, tx, &mut popup.sound, tx.t(S::PopupSound));
                });
                ui.add_space(6.0);
                ui.label(theme::caption(ui, tx.t(S::LocalGrace)));
                theme::row(ui, tx.rtl(), |ui| {
                    changed |= number_slider(
                        ui,
                        tx,
                        &mut host.local_input_grace_ms,
                        GRACE_RANGE,
                        100.0,
                        S::UnitMs,
                    );
                });
                changed |= check_row(
                    ui,
                    tx,
                    &mut host.remote_pointer_marker,
                    tx.t(S::PointerMarker),
                );
                changed |= check_row(
                    ui,
                    tx,
                    &mut host.ask_on_upload_conflict,
                    tx.t(S::HostAskOnConflict),
                );
                ui.label(theme::caption(ui, tx.t(S::HostAskOnConflictHint)));
            });
        });
        if changed {
            self.host_settings_changed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_permission_bit_has_a_label() {
        let all = PERMISSION_LABELS
            .iter()
            .fold(Permissions::NONE, |acc, (p, _)| acc.union(*p));
        assert_eq!(all, Permissions::ALL);
        for (p, _) in PERMISSION_LABELS {
            assert_eq!(p.0.count_ones(), 1);
        }
    }
}
