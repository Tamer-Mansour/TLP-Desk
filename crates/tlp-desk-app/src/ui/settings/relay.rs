//! Settings → Security → Relay server (TLP-relay): where the user's relay is, its token,
//! and whether this device registers with it. Address and token are edited in place and
//! applied with one button (a half-typed address must not be dialed); the switch applies
//! at once.

use eframe::egui::{self, Align, RichText};

use crate::app::App;
use crate::i18n::S;
use crate::ui::icons::{self, Icon};
use crate::ui::{theme, widgets};

impl App {
    pub(super) fn relay_card(&mut self, ui: &mut egui::Ui) {
        let tx = self.tx;
        let dark = ui.visuals().dark_mode;
        if !self.settings_ui.relay_loaded {
            self.settings_ui.relay_address = self.settings.relay.address.clone();
            self.settings_ui.relay_token = self.settings.relay.token.clone();
            self.settings_ui.relay_loaded = true;
        }
        let mut apply = false;
        let mut toggled = false;
        theme::card(ui, |ui| {
            theme::column(ui, tx.rtl(), |ui| {
                widgets::icon_heading(ui, tx, Icon::Network, tx.t(S::SettingsRelay));
                ui.label(theme::caption(ui, tx.t(S::RelayHint)));
                ui.add_space(8.0);
                let w = (ui.available_width() - 20.0).max(200.0);
                ui.label(theme::caption(ui, tx.t(S::RelayAddress)));
                widgets::text_field(
                    ui,
                    tx,
                    "relay-address",
                    &mut self.settings_ui.relay_address,
                    tx.t(S::RelayAddressHint),
                    w,
                );
                ui.add_space(6.0);
                ui.label(theme::caption(ui, tx.t(S::RelayToken)));
                theme::row(ui, tx.rtl(), |ui| {
                    let field_w = (w - 44.0).max(150.0);
                    if self.settings_ui.relay_reveal {
                        widgets::text_field(
                            ui,
                            tx,
                            "relay-token",
                            &mut self.settings_ui.relay_token,
                            tx.t(S::RelayTokenHint),
                            field_w,
                        );
                    } else {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.settings_ui.relay_token)
                                .id(ui.make_persistent_id("relay-token"))
                                .password(true)
                                .hint_text(tx.t(S::RelayTokenHint))
                                .desired_width(field_w)
                                .horizontal_align(if tx.rtl() { Align::Max } else { Align::Min }),
                        );
                    }
                    let (icon, tip) = if self.settings_ui.relay_reveal {
                        (Icon::Hidden, S::HidePassword)
                    } else {
                        (Icon::ViewOnly, S::ShowPassword)
                    };
                    if widgets::ghost_scope(ui, |ui| icons::icon_button(ui, tx, icon, tx.t(tip)))
                        .clicked()
                    {
                        self.settings_ui.relay_reveal = !self.settings_ui.relay_reveal;
                    }
                });
                ui.label(theme::caption(ui, tx.t(S::RelayTokenHint)));
                ui.add_space(8.0);
                let dirty = self.settings_ui.relay_address.trim() != self.settings.relay.address
                    || self.settings_ui.relay_token.trim() != self.settings.relay.token;
                theme::row(ui, tx.rtl(), |ui| {
                    let b = widgets::primary_icon_button(ui, tx, Icon::Save, tx.t(S::RelaySave));
                    if ui.add_enabled(dirty, b).clicked() {
                        apply = true;
                    }
                    let status = if self.settings.relay.is_configured() {
                        tx.f(
                            S::RelayConfigured,
                            &[("addr", &tx.digits(&self.settings.relay.address))],
                        )
                    } else {
                        tx.t(S::RelayNotConfigured).to_owned()
                    };
                    ui.label(theme::caption(ui, &status));
                });
                ui.add_space(8.0);
                let configured = self.settings.relay.is_configured();
                let mut reachable = self.settings.relay.reachable;
                let resp = ui.add_enabled_ui(configured, |ui| {
                    widgets::switch(ui, tx, &mut reachable, tx.t(S::RelayReachable))
                });
                if resp.inner.changed() && reachable != self.settings.relay.reachable {
                    self.settings.relay.reachable = reachable;
                    toggled = true;
                }
                ui.label(theme::caption(ui, tx.t(S::RelayReachableHint)));
                if configured && self.settings.relay.reachable {
                    ui.label(
                        RichText::new(tx.t(S::RelayReachable))
                            .small()
                            .color(theme::success(dark)),
                    );
                }
            });
        });
        if apply {
            self.settings.relay.address = self.settings_ui.relay_address.trim().to_owned();
            self.settings.relay.token = self.settings_ui.relay_token.trim().to_owned();
            self.settings.relay.sanitize();
            self.settings_ui.relay_address = self.settings.relay.address.clone();
            self.settings_ui.relay_token = self.settings.relay.token.clone();
        }
        if apply || toggled {
            self.settings.save();
            self.backend.apply_relay_settings(&self.settings.relay);
        }
    }
}
