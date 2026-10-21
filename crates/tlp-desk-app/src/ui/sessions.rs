//! The Sessions page: who is connected to this device right now (with End), then the
//! connection log (search, filter, export) from `connection_log.rs`.

use eframe::egui::{self, RichText};

use super::icons::{self, Icon};
use super::{shell, theme, widgets};
use crate::app::App;
use crate::i18n::S;

impl App {
    pub fn sessions_ui(&mut self, ui: &mut egui::Ui) {
        let tx = self.tx;
        let mut end = None;
        shell::canvas(ui, |ui| {
            shell::page_title(ui, tx, tx.t(S::RailSessions), Some(tx.t(S::SessionsLede)));
            theme::glass(ui, |ui| {
                theme::column(ui, tx.rtl(), |ui| {
                    ui.label(theme::eyebrow(ui, tx.t(S::ActiveSessions)));
                    ui.add_space(theme::space::M);
                    if self.host_sessions.is_empty() {
                        ui.label(theme::caption(ui, tx.t(S::NoActiveSessions)));
                    }
                    for (session, peer) in &self.host_sessions {
                        let (_, _) = widgets::list_row(ui, |ui| {
                            theme::row(ui, tx.rtl(), |ui| {
                                widgets::avatar(ui, &peer.name, 34.0);
                                ui.add_space(theme::space::XS);
                                theme::column(ui, tx.rtl(), |ui| {
                                    ui.label(RichText::new(tx.d(&peer.name)).strong());
                                    let meta = format!(
                                        "{} · {}",
                                        tx.digits(&peer.device_id.to_string()),
                                        tx.digits(&peer.address.to_string())
                                    );
                                    ui.label(theme::caption(ui, &tx.p(&meta)));
                                });
                                theme::row_end(ui, tx.rtl(), |ui| {
                                    let b = icons::icon_text_button(
                                        tx,
                                        Icon::EndSession,
                                        tx.t(S::EndSession),
                                    );
                                    if ui.add(b).clicked() {
                                        end = Some(*session);
                                    }
                                    super::home::status_pill(
                                        ui,
                                        tx,
                                        tx.t(S::Online),
                                        theme::Tone::Success,
                                    );
                                });
                            });
                        });
                    }
                });
            });
            ui.add_space(theme::space::L);
            theme::glass(ui, |ui| {
                theme::column(ui, tx.rtl(), |ui| {
                    ui.label(theme::eyebrow(ui, tx.t(S::ConnectionLog)));
                    ui.add_space(theme::space::M);
                    self.connection_log_body(ui);
                });
            });
        });
        if let Some(s) = end {
            self.backend.disconnect(s);
        }
    }
}
