//! Address book dialogs: add / edit, delete confirmation, import / export.

use eframe::egui::{self, Align, Id, RichText, Ui};

use super::{ago_raw, AddressBookUi, Dialog, EditForm};
use crate::address_book::{Entry, MacSource, UnixTime};
use crate::backend::DeviceId;
use crate::i18n::{has_rtl, Tx, S};
use crate::ui::bidi_edit::{BidiEdit, NewlineKey};
use crate::ui::icons::{self, Icon};
use crate::ui::{theme, widgets};

impl AddressBookUi {
    /// Draw the open dialog. Returns whether one is open.
    pub fn dialogs_ui(&mut self, ctx: &egui::Context, tx: Tx) -> bool {
        let Some(dialog) = self.dialog.take() else {
            return false;
        };
        self.dialog = match dialog {
            Dialog::Edit(form) => self.edit_dialog(ctx, tx, form),
            Dialog::Delete(id) => self.delete_dialog(ctx, tx, id),
            Dialog::Import(path) => self.file_dialog(ctx, tx, path, true),
            Dialog::Export(path) => self.file_dialog(ctx, tx, path, false),
        };
        true
    }

    fn edit_dialog(&mut self, ctx: &egui::Context, tx: Tx, mut form: EditForm) -> Option<Dialog> {
        let mut decision = None;
        let existing = form.original.and_then(|id| self.book.get(id)).cloned();
        let groups = self.book.groups();
        let now = self.now();
        egui::Modal::new(Id::new("ab-edit")).show(ctx, |ui| {
            ui.set_width(460.0);
            theme::column(ui, tx.rtl(), |ui| {
                let title = if form.original.is_some() {
                    S::AbEditTitle
                } else {
                    S::AbAddTitle
                };
                widgets::icon_heading(ui, tx, Icon::AddressBook, tx.t(title));
                ui.add_space(6.0);
                let w = ui.available_width();
                ui.label(theme::caption(ui, tx.t(S::DeviceId)));
                if form.original.is_some() {
                    ui.label(RichText::new(tx.n(&form.id)).monospace().strong());
                } else {
                    widgets::text_field(ui, tx, "ab-edit-id", &mut form.id, "123 456 789", w);
                }
                labeled_field(
                    ui,
                    tx,
                    S::AbAlias,
                    "ab-edit-alias",
                    &mut form.alias,
                    tx.t(S::AbAliasHint),
                    w,
                );
                labeled_field(ui, tx, S::AbGroup, "ab-edit-group", &mut form.group, "", w);
                if !groups.is_empty() {
                    theme::row(ui, tx.rtl(), |ui| {
                        for g in &groups {
                            if ui.small_button(tx.d(g)).clicked() {
                                form.group = g.clone();
                            }
                        }
                    });
                }
                labeled_field(
                    ui,
                    tx,
                    S::AbTags,
                    "ab-edit-tags",
                    &mut form.tags,
                    tx.t(S::AbTagsHint),
                    w,
                );
                ui.label(theme::caption(ui, tx.t(S::AbNotes)));
                notes_field(ui, tx, &mut form.notes, w);
                labeled_field(
                    ui,
                    tx,
                    S::AbMac,
                    "ab-edit-mac",
                    &mut form.mac,
                    tx.t(S::AbMacHint),
                    w,
                );
                if let Some(e) = &existing {
                    if e.mac.is_some() && e.mac_source == MacSource::Learned {
                        ui.label(theme::caption(ui, tx.t(S::AbMacLearned)));
                    }
                    details(ui, tx, e, now);
                }
                if let Some(err) = form.error {
                    ui.label(RichText::new(tx.t(err)).color(theme::danger(ui.visuals().dark_mode)));
                }
                ui.add_space(8.0);
                theme::row(ui, tx.rtl(), |ui| {
                    let save = widgets::primary_icon_button(ui, tx, Icon::Save, tx.t(S::Save));
                    if ui.add(save).clicked() {
                        decision = Some(true);
                    }
                    if ui
                        .add(icons::icon_text_button(tx, Icon::Close, tx.t(S::Cancel)))
                        .clicked()
                    {
                        decision = Some(false);
                    }
                });
            });
        });
        match decision {
            Some(true) if self.apply_edit(&mut form, tx) => None,
            Some(false) => None,
            _ => Some(Dialog::Edit(form)),
        }
    }

    fn delete_dialog(&mut self, ctx: &egui::Context, tx: Tx, id: DeviceId) -> Option<Dialog> {
        let name = self.book.get(id).map(Entry::display_name)?;
        let mut decision = None;
        egui::Modal::new(Id::new("ab-delete")).show(ctx, |ui| {
            ui.set_width(400.0);
            theme::column(ui, tx.rtl(), |ui| {
                widgets::icon_heading(ui, tx, Icon::Trash, tx.t(S::AbDeleteTitle));
                ui.add_space(4.0);
                ui.label(tx.f(S::AbDeleteBody, &[("name", &name)]));
                ui.add_space(8.0);
                theme::row(ui, tx.rtl(), |ui| {
                    let del = widgets::danger_icon_button(ui, tx, Icon::Trash, tx.t(S::AbDelete));
                    if ui.add(del).clicked() {
                        decision = Some(true);
                    }
                    if ui
                        .add(icons::icon_text_button(tx, Icon::Close, tx.t(S::Cancel)))
                        .clicked()
                    {
                        decision = Some(false);
                    }
                });
            });
        });
        match decision {
            Some(true) => {
                self.delete(id, tx);
                None
            }
            Some(false) => None,
            None => Some(Dialog::Delete(id)),
        }
    }

    fn file_dialog(
        &mut self,
        ctx: &egui::Context,
        tx: Tx,
        mut path: String,
        import: bool,
    ) -> Option<Dialog> {
        let mut decision = None;
        let (title, icon, action) = if import {
            (S::AbImportTitle, Icon::Download, S::AbImport)
        } else {
            (S::AbExportTitle, Icon::Upload, S::AbExport)
        };
        egui::Modal::new(Id::new("ab-file")).show(ctx, |ui| {
            ui.set_width(520.0);
            theme::column(ui, tx.rtl(), |ui| {
                widgets::icon_heading(ui, tx, icon, tx.t(title));
                ui.add_space(4.0);
                let w = ui.available_width();
                labeled_field(ui, tx, S::AbFilePath, "ab-file-path", &mut path, "", w);
                ui.add_space(8.0);
                theme::row(ui, tx.rtl(), |ui| {
                    let ok = widgets::primary_icon_button(ui, tx, icon, tx.t(action));
                    if ui.add_enabled(!path.trim().is_empty(), ok).clicked() {
                        decision = Some(true);
                    }
                    if ui
                        .add(icons::icon_text_button(tx, Icon::Close, tx.t(S::Cancel)))
                        .clicked()
                    {
                        decision = Some(false);
                    }
                });
            });
        });
        let done = match decision {
            Some(true) if import => self.import(&path, tx),
            Some(true) => self.export(&path, tx),
            Some(false) => true,
            None => false,
        };
        if done {
            None
        } else if import {
            Some(Dialog::Import(path))
        } else {
            Some(Dialog::Export(path))
        }
    }
}

/// Read-only facts in the edit dialog.
fn details(ui: &mut Ui, tx: Tx, e: &Entry, now: UnixTime) {
    let when = |t: Option<UnixTime>| match t {
        Some(t) => tx.p(&ago_raw(tx, now, t)),
        None => tx.t(S::AbNever).to_owned(),
    };
    let ips = e
        .last_ips
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let mut rows = vec![];
    if !e.name.is_empty() {
        rows.push((S::AbAnnouncedName, tx.d(&e.name)));
    }
    if !ips.is_empty() {
        rows.push((S::AbLastAddresses, tx.n(&ips)));
    }
    rows.push((S::AbSortLastSeen, when(e.last_seen)));
    rows.push((S::AbLastConnected, when(e.last_connected)));
    if let Some(fp) = &e.pinned_fingerprint {
        rows.push((S::AbPinnedKey, tx.n(fp)));
    }
    ui.add_space(6.0);
    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .corner_radius(8)
        .inner_margin(10)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            for (k, v) in rows {
                theme::row(ui, tx.rtl(), |ui| {
                    ui.label(theme::caption(ui, tx.t(k)));
                    ui.add_space(6.0);
                    ui.label(RichText::new(v).strong());
                });
            }
        });
}

fn labeled_field(
    ui: &mut Ui,
    tx: Tx,
    label: S,
    id_salt: &str,
    value: &mut String,
    hint: &str,
    width: f32,
) -> egui::Response {
    ui.label(theme::caption(ui, tx.t(label)));
    widgets::text_field(ui, tx, id_salt, value, hint, width)
}

/// Multi-line notes. Text with Arabic is edited by [`BidiEdit`] (visual order, bidi caret).
fn notes_field(ui: &mut Ui, tx: Tx, value: &mut String, width: f32) -> egui::Response {
    let id = ui.make_persistent_id("ab-edit-notes");
    if has_rtl(value) {
        return BidiEdit::new(value, id)
            .width(width)
            .multiline(3, NewlineKey::Enter)
            .default_rtl(tx.rtl())
            .show(ui);
    }
    ui.add(
        egui::TextEdit::multiline(value)
            .id(id)
            .desired_width(width)
            .desired_rows(3)
            .horizontal_align(if tx.rtl() { Align::Max } else { Align::Min }),
    )
}
