//! The Devices page: everything the app knows about other devices in one place. A segmented
//! switch picks the LAN list or the address book; the address book keeps its own search,
//! filters and dialogs (`address_book.rs`), the LAN list reuses the home screen's rows.

use eframe::egui;

use super::address_book::{BookCommand, HomeTab};
use super::icons::Icon;
use super::{shell, theme, widgets};
use crate::app::App;
use crate::i18n::S;

impl App {
    pub fn devices_ui(&mut self, ui: &mut egui::Ui) {
        let tx = self.tx;
        let mut connect = None;
        let mut save = None;
        shell::canvas(ui, |ui| {
            shell::page_title(ui, tx, tx.t(S::MenuDevices), Some(tx.t(S::DevicesLede)));
            theme::glass(ui, |ui| {
                theme::column(ui, tx.rtl(), |ui| {
                    widgets::segmented(
                        ui,
                        tx,
                        &mut self.address_book.tab,
                        &[
                            (HomeTab::Nearby, Some(Icon::Network), tx.t(S::NearbyDevices)),
                            (
                                HomeTab::AddressBook,
                                Some(Icon::AddressBook),
                                tx.t(S::AbTitle),
                            ),
                        ],
                    );
                    ui.add_space(theme::space::L);
                    if self.address_book.tab == HomeTab::AddressBook {
                        let backend = self.backend.clone();
                        if let Some(BookCommand::Connect(t)) =
                            self.address_book.list_ui(ui, tx, backend.as_ref())
                        {
                            connect = Some(t);
                        }
                        return;
                    }
                    match &self.devices {
                        None => {
                            theme::row(ui, tx.rtl(), |ui| {
                                ui.spinner();
                                ui.label(theme::caption(ui, tx.t(S::Searching)));
                            });
                        }
                        Some(list) if list.is_empty() => {
                            widgets::empty_state(
                                ui,
                                tx,
                                Icon::Radar,
                                tx.t(S::NoDevicesTitle),
                                tx.t(S::NoDevicesHint),
                            );
                        }
                        Some(list) => {
                            let max_h = (ui.available_height() - 8.0).max(160.0);
                            egui::ScrollArea::vertical()
                                .auto_shrink([false, true])
                                .max_height(max_h)
                                .show(ui, |ui| {
                                    let mut actions = Vec::new();
                                    for dev in list {
                                        let saved = self.address_book.book.contains(dev.device_id);
                                        super::home::device_row(ui, tx, dev, saved, &mut actions);
                                    }
                                    for a in actions {
                                        match a {
                                            super::home::Action::Connect(t) => connect = Some(t),
                                            super::home::Action::SaveDevice(d) => save = Some(d),
                                            super::home::Action::ShowAddressBook => {
                                                self.address_book.tab = HomeTab::AddressBook;
                                            }
                                            _ => {}
                                        }
                                    }
                                });
                        }
                    }
                });
            });
        });
        self.address_book.dialogs_ui(ui.ctx(), tx);
        self.address_book.toast_ui(ui.ctx(), tx);
        if let Some(d) = save {
            self.address_book
                .save_discovered(&d, self.backend.as_ref(), tx);
        }
        if let Some(t) = connect {
            self.connect(t);
        }
    }
}
