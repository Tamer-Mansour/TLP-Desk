//! Settings screen: language, digit style, device name.

use eframe::egui::{self, RichText};

use super::icons::{self, Icon};
use super::{theme, widgets};
use crate::app::{App, Screen};
use crate::i18n::{DigitStyle, Lang, S};

impl App {
    pub fn settings_ui(&mut self, ui: &mut egui::Ui) {
        self.settings_top_bar(ui);
        let tx = self.tx;
        let mut new_lang = None;
        let mut new_digits = None;
        let mut save_name = false;
        let mut back = false;

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(20),
            )
            .show(ui, |ui| {
                theme::column(ui, tx.rtl(), |ui| {
                    theme::row(ui, tx.rtl(), |ui| {
                        if ui
                            .add(icons::icon_text_button(tx, Icon::Back, tx.t(S::Back)))
                            .clicked()
                        {
                            back = true;
                        }
                        ui.label(RichText::new(tx.t(S::Settings)).heading().strong());
                    });
                    ui.add_space(10.0);
                    ui.set_max_width(560.0_f32.min(ui.available_width()));

                    theme::card(ui, |ui| {
                        theme::column(ui, tx.rtl(), |ui| {
                            widgets::icon_heading(ui, tx, Icon::Language, tx.t(S::Appearance));
                            ui.add_space(6.0);
                            ui.label(theme::caption(ui, tx.t(S::Language)));
                            theme::row(ui, tx.rtl(), |ui| {
                                for l in Lang::ALL {
                                    if ui.selectable_label(tx.lang == l, l.native_name()).clicked()
                                        && tx.lang != l
                                    {
                                        new_lang = Some(l);
                                    }
                                }
                            });
                            ui.add_space(6.0);
                            ui.label(theme::caption(ui, tx.t(S::Digits)));
                            theme::row(ui, tx.rtl(), |ui| {
                                for d in DigitStyle::ALL {
                                    let key = match d {
                                        DigitStyle::Western => S::DigitsWestern,
                                        DigitStyle::ArabicIndic => S::DigitsArabicIndic,
                                    };
                                    if ui.selectable_label(tx.digits == d, tx.t(key)).clicked() {
                                        new_digits = Some(d);
                                    }
                                }
                            });
                            ui.label(theme::caption(ui, tx.t(S::DigitsHint)));
                        });
                    });
                    ui.add_space(12.0);
                    theme::card(ui, |ui| {
                        theme::column(ui, tx.rtl(), |ui| {
                            widgets::icon_heading(
                                ui,
                                tx,
                                Icon::Laptop,
                                tx.t(S::ThisDeviceSettings),
                            );
                            ui.add_space(6.0);
                            ui.label(theme::caption(ui, tx.t(S::DeviceName)));
                            let resp = theme::row(ui, tx.rtl(), |ui| {
                                let w = (ui.available_width() - 100.0).max(160.0);
                                let hint = tx.t(S::DeviceName);
                                let resp = widgets::text_field(
                                    ui,
                                    tx,
                                    "device-name",
                                    &mut self.name_edit,
                                    hint,
                                    w,
                                );
                                let enter = resp.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                let changed = self.name_edit.trim() != self.local.device_name
                                    && !self.name_edit.trim().is_empty();
                                if ui
                                    .add_enabled(
                                        changed,
                                        widgets::primary_icon_button(
                                            ui,
                                            tx,
                                            Icon::Save,
                                            tx.t(S::Save),
                                        ),
                                    )
                                    .clicked()
                                    || (enter && changed)
                                {
                                    save_name = true;
                                }
                                resp
                            });
                            widgets::edit_preview(ui, tx, &resp, &self.name_edit);
                            ui.label(theme::caption(ui, tx.t(S::DeviceNameHint)));
                            if self.renamed {
                                ui.label(theme::caption(ui, tx.t(S::DeviceNameRestart)));
                            }
                        });
                    });
                });
            });

        if let Some(l) = new_lang {
            self.set_lang(ui.ctx(), l);
        }
        if let Some(d) = new_digits {
            self.set_digits(d);
        }
        if save_name {
            let name = self.name_edit.trim().to_owned();
            self.settings.device_name = Some(name.clone());
            self.settings.save();
            self.renamed = true;
            self.backend.set_device_name(name);
        }
        if back {
            self.screen = Screen::Home;
        }
    }
}
