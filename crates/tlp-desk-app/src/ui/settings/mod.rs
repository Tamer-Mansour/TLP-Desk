//! Settings screen, one card per section: language and display, this device, incoming
//! connections, allowed networks, picture quality, start-up and notification area.
//!
//! Every change is saved to `settings.json` at once and applied live where possible: host
//! options and the allow-list go to the backend ([`crate::backend::Backend::apply_host_settings`]),
//! the quality preset to a running viewer session, start-up options to the `Run` key.

mod allow_list;
mod incoming;
mod startup;

pub(crate) use incoming::PERMISSION_LABELS;
pub(crate) use startup::quality_key;

use eframe::egui::{self, RichText};

use super::icons::{self, Icon};
use super::{theme, widgets};
use crate::app::{App, Screen};
use crate::i18n::{DigitStyle, Lang, Tx, S};
use crate::settings::allow_list::CidrError;

/// Transient state of the settings screen (not persisted).
#[derive(Debug, Default)]
pub struct SettingsUi {
    /// Text typed into the allow-list editor.
    pub allow_input: String,
    /// Why the last entry was refused.
    pub allow_error: Option<CidrError>,
    /// A public range waiting for "Add anyway".
    pub pending_public: Option<String>,
    /// The last start-with-Windows change failed (display-ready).
    pub autostart_error: Option<String>,
    /// Scroll the screen to this offset on the next frame (README media only).
    pub scroll_offset: Option<f32>,
}

/// A checkbox whose label sits on the reading side (RTL-aware); returns `true` if toggled.
pub(crate) fn check_row(ui: &mut egui::Ui, tx: Tx, value: &mut bool, text: &str) -> bool {
    theme::row(ui, tx.rtl(), |ui| {
        let mut changed = ui.checkbox(value, "").changed();
        let label = ui.add(egui::Label::new(text).sense(egui::Sense::click()));
        if label.clicked() {
            *value = !*value;
            changed = true;
        }
        changed
    })
}

impl App {
    /// The whole settings screen.
    pub fn settings_ui(&mut self, ui: &mut egui::Ui) {
        self.settings_top_bar(ui);
        let tx = self.tx;
        let mut back = false;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(20),
            )
            .show(ui, |ui| {
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
                let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
                if let Some(y) = self.settings_ui.scroll_offset.take() {
                    area = area.vertical_scroll_offset(y);
                }
                area.show(ui, |ui| {
                    theme::column(ui, tx.rtl(), |ui| {
                        ui.set_max_width(620.0_f32.min(ui.available_width()));
                        self.appearance_card(ui);
                        ui.add_space(12.0);
                        self.device_card_settings(ui);
                        ui.add_space(12.0);
                        self.incoming_card(ui);
                        ui.add_space(12.0);
                        // Permanent password for unattended access (TLP-unattended).
                        self.unattended.settings_card(ui, tx, self.backend.as_ref());
                        ui.add_space(12.0);
                        self.allow_list_card(ui);
                        ui.add_space(12.0);
                        self.quality_card(ui);
                        ui.add_space(12.0);
                        self.startup_card(ui);
                        ui.add_space(12.0);
                        crate::ui::clipboard::settings_card(
                            ui,
                            tx,
                            &mut self.settings,
                            self.backend.as_ref(),
                        );
                        ui.add_space(12.0);
                        crate::ui::typing::settings_card(ui, tx, &mut self.settings);
                        if let Some(view) = self.session.as_mut() {
                            view.typing.set_route(
                                crate::text_route::TextRoute::from_layout_setting(
                                    self.settings.type_with_my_layout,
                                ),
                            );
                        }
                        ui.add_space(12.0);
                        crate::ui::audio::settings_card(
                            ui,
                            tx,
                            &mut self.settings,
                            self.backend.as_ref(),
                        );
                    });
                });
            });
        if back {
            self.screen = Screen::Home;
        }
    }

    /// Persist the settings and push the host part to the backend.
    pub(crate) fn host_settings_changed(&mut self) {
        self.settings.host.sanitize();
        self.settings.save();
        self.backend.apply_host_settings(&self.settings.host);
    }

    fn appearance_card(&mut self, ui: &mut egui::Ui) {
        let tx = self.tx;
        let mut new_lang = None;
        let mut new_digits = None;
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
        if let Some(l) = new_lang {
            self.set_lang(ui.ctx(), l);
        }
        if let Some(d) = new_digits {
            self.set_digits(d);
        }
    }

    fn device_card_settings(&mut self, ui: &mut egui::Ui) {
        let tx = self.tx;
        let mut save_name = false;
        theme::card(ui, |ui| {
            theme::column(ui, tx.rtl(), |ui| {
                widgets::icon_heading(ui, tx, Icon::Laptop, tx.t(S::ThisDeviceSettings));
                ui.add_space(6.0);
                ui.label(theme::caption(ui, tx.t(S::DeviceName)));
                theme::row(ui, tx.rtl(), |ui| {
                    let w = (ui.available_width() - 100.0).max(160.0);
                    let hint = tx.t(S::DeviceName);
                    let resp =
                        widgets::text_field(ui, tx, "device-name", &mut self.name_edit, hint, w);
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let changed = self.name_edit.trim() != self.local.device_name
                        && !self.name_edit.trim().is_empty();
                    if ui
                        .add_enabled(
                            changed,
                            widgets::primary_icon_button(ui, tx, Icon::Save, tx.t(S::Save)),
                        )
                        .clicked()
                        || (enter && changed)
                    {
                        save_name = true;
                    }
                    resp
                });
                ui.label(theme::caption(ui, tx.t(S::DeviceNameHint)));
                if self.renamed {
                    ui.label(theme::caption(ui, tx.t(S::DeviceNameRestart)));
                }
            });
        });
        if save_name {
            let name = self.name_edit.trim().to_owned();
            self.settings.device_name = Some(name.clone());
            self.settings.save();
            self.renamed = true;
            self.backend.set_device_name(name);
        }
    }
}
