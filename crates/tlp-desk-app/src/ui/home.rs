//! Home screen: this device's identity + connect box + discovered devices.

use eframe::egui::{self, Id, RichText};

use super::{theme, widgets};
use crate::app::{App, Screen};
use crate::backend::{ConnectTarget, DiscoveredDevice};
use crate::i18n::{Lang, Tx, S};

enum Action {
    Connect(ConnectTarget),
    ConnectAddress,
    NewCode,
    OpenSettings,
    SetLang(Lang),
}

/// Top bar shared by home and settings: brand at the start, settings + language at the end.
fn top_bar(ui: &mut egui::Ui, tx: Tx, actions: &mut Vec<Action>, show_settings_button: bool) {
    egui::Panel::top("top-bar")
        .frame(
            egui::Frame::new()
                .fill(ui.visuals().window_fill)
                .inner_margin(egui::Margin::symmetric(20, 10)),
        )
        .show(ui, |ui| {
            theme::row(ui, tx.rtl(), |ui| {
                let accent = theme::accent(ui.visuals().dark_mode);
                theme::status_dot(ui, accent);
                ui.label(
                    RichText::new(tx.t(S::AppTitle))
                        .heading()
                        .strong()
                        .color(accent),
                );
                theme::row_end(ui, tx.rtl(), |ui| {
                    let mut lang = tx.lang;
                    egui::ComboBox::from_id_salt("lang-picker")
                        .selected_text(lang.native_name())
                        .show_ui(ui, |ui| {
                            for l in Lang::ALL {
                                ui.selectable_value(&mut lang, l, l.native_name());
                            }
                        });
                    if lang != tx.lang {
                        actions.push(Action::SetLang(lang));
                    }
                    if show_settings_button && ui.button(tx.t(S::Settings)).clicked() {
                        actions.push(Action::OpenSettings);
                    }
                });
            });
        });
}

impl App {
    pub fn home_ui(&mut self, ui: &mut egui::Ui) {
        let tx = self.tx;
        let mut actions = Vec::new();
        top_bar(ui, tx, &mut actions, true);

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(20),
            )
            .show(ui, |ui| {
                ui.columns(2, |cols| {
                    let (first, second) = if tx.rtl() { (1, 0) } else { (0, 1) };
                    self.device_card(&mut cols[first], &mut actions);
                    self.connect_card(&mut cols[second], &mut actions);
                });
            });

        for action in actions {
            match action {
                Action::Connect(t) => self.connect(t),
                Action::ConnectAddress => self.connect_from_address(),
                Action::NewCode => self.backend.refresh_one_time_code(),
                Action::OpenSettings => self.screen = Screen::Settings,
                Action::SetLang(l) => self.set_lang(ui.ctx(), l),
            }
        }
    }

    pub fn settings_top_bar(&mut self, ui: &mut egui::Ui) {
        let mut actions = Vec::new();
        top_bar(ui, self.tx, &mut actions, false);
        for action in actions {
            if let Action::SetLang(l) = action {
                self.set_lang(ui.ctx(), l);
            }
        }
    }

    fn device_card(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let tx = self.tx;
        theme::card(ui, |ui| {
            theme::column(ui, tx.rtl(), |ui| {
                ui.label(RichText::new(tx.t(S::ThisDevice)).heading());
                ui.add_space(8.0);

                ui.label(theme::caption(ui, tx.t(S::DeviceId)));
                let id = self.local.device_id.to_string();
                theme::row(ui, tx.rtl(), |ui| {
                    ui.label(RichText::new(tx.n(&id)).size(30.0).monospace().strong());
                    // Clipboard always gets Western digits.
                    widgets::copy_button(ui, tx, Id::new("copy-id"), &id);
                });
                ui.add_space(10.0);

                ui.label(theme::caption(ui, tx.t(S::OneTimeCode)));
                let code = &self.local.one_time_code;
                let grouped = if code.len() == 6 {
                    format!("{} {}", &code[..3], &code[3..])
                } else {
                    code.clone()
                };
                theme::row(ui, tx.rtl(), |ui| {
                    ui.label(
                        RichText::new(tx.n(&grouped))
                            .size(24.0)
                            .monospace()
                            .color(theme::accent(ui.visuals().dark_mode)),
                    );
                    widgets::copy_button(ui, tx, Id::new("copy-code"), code);
                    if ui.button(tx.t(S::NewCode)).clicked() {
                        actions.push(Action::NewCode);
                    }
                });
                ui.label(theme::caption(ui, tx.t(S::OneTimeCodeHint)));
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(4.0);

                theme::row(ui, tx.rtl(), |ui| {
                    ui.label(theme::caption(ui, tx.t(S::DeviceName)));
                    ui.add_space(6.0);
                    ui.label(RichText::new(tx.d(&self.local.device_name)).strong());
                });
                ui.label(theme::caption(ui, tx.t(S::LocalAddress)));
                for addr in &self.local.addresses {
                    let a = addr.to_string();
                    theme::row(ui, tx.rtl(), |ui| {
                        ui.label(RichText::new(tx.n(&a)).monospace());
                        widgets::copy_button(ui, tx, Id::new(("copy-ip", &a)), &a);
                    });
                }
            });
        });
    }

    fn connect_card(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let tx = self.tx;
        theme::card(ui, |ui| {
            theme::column(ui, tx.rtl(), |ui| {
                ui.label(RichText::new(tx.t(S::ConnectTitle)).heading());
                ui.add_space(8.0);
                let resp = theme::row(ui, tx.rtl(), |ui| {
                    let button_w = 120.0;
                    let field_w = (ui.available_width() - button_w - 12.0).max(120.0);
                    // In RTL the row flows right-to-left: field first (rightmost), then button.
                    let resp = widgets::text_field(
                        ui,
                        tx,
                        "address",
                        &mut self.address,
                        tx.t(S::AddressHint),
                        field_w,
                    );
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let has_text = !self.address.trim().is_empty();
                    let clicked =
                        widgets::chevron_button(ui, tx, tx.t(S::Connect), true, true, has_text)
                            .clicked();
                    if (enter || clicked) && has_text {
                        actions.push(Action::ConnectAddress);
                    }
                    resp
                });
                widgets::edit_preview(ui, tx, &resp, &self.address);
                ui.add_space(6.0);
                theme::row(ui, tx.rtl(), |ui| {
                    let resp = widgets::text_field(
                        ui,
                        tx,
                        "one-time-code",
                        &mut self.code,
                        tx.t(S::CodeHint),
                        200.0,
                    );
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if enter && !self.address.trim().is_empty() {
                        actions.push(Action::ConnectAddress);
                    }
                });
                ui.label(theme::caption(ui, tx.t(S::CodeFieldHint)));
            });
        });
        ui.add_space(12.0);
        theme::card(ui, |ui| {
            theme::column(ui, tx.rtl(), |ui| {
                ui.label(RichText::new(tx.t(S::NearbyDevices)).heading());
                ui.add_space(6.0);
                match &self.devices {
                    None => {
                        theme::row(ui, tx.rtl(), |ui| {
                            ui.spinner();
                            ui.label(theme::caption(ui, tx.t(S::Searching)));
                        });
                    }
                    Some(list) => {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for dev in list {
                                    device_row(ui, tx, dev, actions);
                                }
                            });
                    }
                }
            });
        });
    }
}

fn device_row(ui: &mut egui::Ui, tx: Tx, dev: &DiscoveredDevice, actions: &mut Vec<Action>) {
    let dark = ui.visuals().dark_mode;
    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .outer_margin(egui::Margin::symmetric(0, 3))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::row(ui, tx.rtl(), |ui| {
                let color = if dev.online {
                    theme::success(dark)
                } else {
                    ui.visuals().weak_text_color()
                };
                theme::status_dot(ui, color);
                theme::column(ui, tx.rtl(), |ui| {
                    ui.label(RichText::new(tx.d(&dev.name)).strong());
                    let status = if dev.online { S::Online } else { S::Offline };
                    let id = tx.digits(&dev.device_id.to_string());
                    let ip = tx.digits(&dev.address.to_string());
                    // Build the meta line in logical order, then shape once.
                    let meta = format!("{id} · {ip} · {}", tx.raw(status));
                    ui.label(theme::caption(ui, &tx.p(&meta)));
                });
                theme::row_end(ui, tx.rtl(), |ui| {
                    if ui
                        .add_enabled(dev.online, egui::Button::new(tx.t(S::Connect)))
                        .clicked()
                    {
                        actions.push(Action::Connect(ConnectTarget::Id(dev.device_id)));
                    }
                });
            });
        });
}
