//! "Picture quality" and "Start-up and notification area" cards.

use eframe::egui::{self, RichText};

use super::check_row;
use crate::app::App;
use crate::i18n::S;
use crate::settings::autostart;
use crate::settings::QualityChoice;
use crate::ui::icons::Icon;
use crate::ui::{theme, widgets};

/// i18n key of a quality preset.
pub(crate) fn quality_key(q: QualityChoice) -> S {
    match q {
        QualityChoice::Speed => S::QualitySpeed,
        QualityChoice::Balanced => S::QualityBalanced,
        QualityChoice::Quality => S::QualityBest,
    }
}

impl App {
    pub(super) fn quality_card(&mut self, ui: &mut egui::Ui) {
        let tx = self.tx;
        let mut picked = None;
        theme::card(ui, |ui| {
            theme::column(ui, tx.rtl(), |ui| {
                widgets::icon_heading(ui, tx, Icon::Quality, tx.t(S::SettingsQuality));
                ui.add_space(6.0);
                theme::row(ui, tx.rtl(), |ui| {
                    for q in QualityChoice::ALL {
                        if ui
                            .selectable_label(self.settings.quality == q, tx.t(quality_key(q)))
                            .clicked()
                            && self.settings.quality != q
                        {
                            picked = Some(q);
                        }
                    }
                });
                ui.label(theme::caption(ui, tx.t(S::QualityHint)));
            });
        });
        if let Some(q) = picked {
            self.set_quality(q);
        }
    }

    /// Persist the preset and ask the host of a running viewer session for it.
    pub(crate) fn set_quality(&mut self, q: QualityChoice) {
        self.settings.quality = q;
        self.settings.save();
        if let Some(s) = self.session.as_mut() {
            s.quality = q;
            s.send_quality(self.backend.as_ref());
        }
    }

    pub(super) fn startup_card(&mut self, ui: &mut egui::Ui) {
        let tx = self.tx;
        let dark = ui.visuals().dark_mode;
        let mut autostart_wanted = None;
        let mut changed = false;
        theme::card(ui, |ui| {
            theme::column(ui, tx.rtl(), |ui| {
                widgets::icon_heading(ui, tx, Icon::Tray, tx.t(S::SettingsStartup));
                ui.add_space(6.0);
                let mut on = self.settings.startup.start_with_windows;
                if check_row(ui, tx, &mut on, tx.t(S::StartWithWindows)) {
                    autostart_wanted = Some(on);
                }
                if let Some(err) = &self.settings_ui.autostart_error {
                    ui.label(
                        RichText::new(tx.f(S::AutostartFailed, &[("detail", err)]))
                            .color(theme::danger(dark)),
                    );
                }
                changed |= check_row(
                    ui,
                    tx,
                    &mut self.settings.startup.start_minimized,
                    tx.t(S::StartMinimized),
                );
                changed |= check_row(
                    ui,
                    tx,
                    &mut self.settings.startup.keep_in_tray_on_close,
                    tx.t(S::CloseToTray),
                );
            });
        });
        if let Some(on) = autostart_wanted {
            match autostart::apply(self.autostart.as_ref(), on) {
                Ok(now) => {
                    self.settings.startup.start_with_windows = now;
                    self.settings_ui.autostart_error = None;
                }
                Err(e) => self.settings_ui.autostart_error = Some(e),
            }
            changed = true;
        }
        if changed {
            self.settings.save();
        }
    }
}
