//! "Allowed networks" card: the CIDR allow-list editor. Entries are validated with the same
//! parser the endpoint uses; ranges reaching beyond the LAN need a second click.

use eframe::egui::{self, RichText};

use crate::app::App;
use crate::i18n::S;
use crate::settings::allow_list::{check_entry, is_local_range, CidrError};
use crate::settings::host::default_allow_list;
use crate::ui::icons::{self, Icon};
use crate::ui::{theme, widgets};

enum Edit {
    Add(String),
    Remove(usize),
    Reset,
}

impl App {
    pub(super) fn allow_list_card(&mut self, ui: &mut egui::Ui) {
        let tx = self.tx;
        let dark = ui.visuals().dark_mode;
        let mut edit = None;
        let mut try_add = false;
        let mut confirm_public = false;
        theme::card(ui, |ui| {
            theme::column(ui, tx.rtl(), |ui| {
                widgets::icon_heading(ui, tx, Icon::Network, tx.t(S::SettingsAllowList));
                ui.label(theme::caption(ui, tx.t(S::AllowListHint)));
                ui.add_space(6.0);
                let only_one = self.settings.host.allow_list.len() <= 1;
                for (i, range) in self.settings.host.allow_list.iter().enumerate() {
                    theme::row(ui, tx.rtl(), |ui| {
                        let text = RichText::new(tx.n(range)).monospace();
                        let text = if is_local_range(range) {
                            text
                        } else {
                            text.color(theme::danger(dark))
                        };
                        ui.label(text);
                        if !is_local_range(range) {
                            icons::icon(ui, tx, Icon::Warning, 14.0, theme::danger(dark));
                        }
                        theme::row_end(ui, tx.rtl(), |ui| {
                            let remove =
                                icons::icon_text_button(tx, Icon::Trash, tx.t(S::AllowListRemove));
                            let resp = ui.add_enabled(!only_one, remove);
                            let resp = if only_one {
                                resp.on_disabled_hover_text(tx.t(S::AllowListLast))
                            } else {
                                resp
                            };
                            if resp.clicked() {
                                edit = Some(Edit::Remove(i));
                            }
                        });
                    });
                }
                ui.add_space(6.0);
                theme::row(ui, tx.rtl(), |ui| {
                    let w = (ui.available_width() - 120.0).max(160.0);
                    let resp = widgets::text_field(
                        ui,
                        tx,
                        "allow-list-input",
                        &mut self.settings_ui.allow_input,
                        tx.t(S::AllowListPlaceholder),
                        w,
                    );
                    if resp.changed() {
                        self.settings_ui.allow_error = None;
                        self.settings_ui.pending_public = None;
                    }
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let add = icons::icon_text_button(tx, Icon::Plus, tx.t(S::AllowListAdd));
                    if ui.add(add).clicked() || enter {
                        try_add = true;
                    }
                });
                if let Some(err) = self.settings_ui.allow_error {
                    let key = match err {
                        CidrError::Duplicate => S::AllowListDuplicate,
                        CidrError::Empty | CidrError::Invalid => S::AllowListInvalid,
                    };
                    ui.label(RichText::new(tx.t(key)).color(theme::danger(dark)));
                }
                if let Some(range) = &self.settings_ui.pending_public {
                    ui.label(
                        RichText::new(tx.f(S::AllowListPublic, &[("range", &tx.digits(range))]))
                            .color(theme::danger(dark)),
                    );
                    theme::row(ui, tx.rtl(), |ui| {
                        let b = widgets::danger_icon_button(
                            ui,
                            tx,
                            Icon::Warning,
                            tx.t(S::AllowListAddAnyway),
                        );
                        if ui.add(b).clicked() {
                            confirm_public = true;
                        }
                    });
                }
                ui.add_space(6.0);
                let is_default = self.settings.host.allow_list == default_allow_list();
                let reset = icons::icon_text_button(tx, Icon::Undo, tx.t(S::AllowListReset));
                if ui.add_enabled(!is_default, reset).clicked() {
                    edit = Some(Edit::Reset);
                }
            });
        });

        if try_add {
            match check_entry(
                &self.settings_ui.allow_input,
                &self.settings.host.allow_list,
            ) {
                Ok(entry) if entry.public => {
                    self.settings_ui.pending_public = Some(entry.text);
                    self.settings_ui.allow_error = None;
                }
                Ok(entry) => edit = Some(Edit::Add(entry.text)),
                Err(e) => {
                    self.settings_ui.allow_error = Some(e);
                    self.settings_ui.pending_public = None;
                }
            }
        }
        if confirm_public {
            if let Some(range) = self.settings_ui.pending_public.take() {
                tracing::warn!(%range, "user added a range outside the LAN to the allow-list");
                edit = Some(Edit::Add(range));
            }
        }
        if let Some(edit) = edit {
            let list = &mut self.settings.host.allow_list;
            match edit {
                Edit::Add(range) => {
                    list.push(range);
                    self.settings_ui.allow_input.clear();
                    self.settings_ui.allow_error = None;
                }
                Edit::Remove(i) if list.len() > 1 && i < list.len() => {
                    list.remove(i);
                }
                Edit::Remove(_) => {}
                Edit::Reset => *list = default_allow_list(),
            }
            self.host_settings_changed();
        }
    }
}
