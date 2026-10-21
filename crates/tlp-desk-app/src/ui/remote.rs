//! The Remote devices page (TLP-relay): the devices reached through the user's own relay
//! server, each as a card with a picture of its screen from the last session, its name and
//! ID, when it was last reached, and a Connect button. Kept apart from the LAN list: those
//! devices are found by discovery and stream at full quality; these are typed in by ID (or
//! land here after a session through the relay) and stream at up to 1080p.
//!
//! State changes go through [`RemoteUi`] methods (unit-tested without a window); the `*_ui`
//! methods only draw and collect clicks.

use std::collections::HashMap;

use eframe::egui::{self, Align, Color32, Id, Layout, RichText, TextureHandle, Ui};

use super::icons::{self, Icon};
use super::{shell, theme, widgets};
use crate::address_book::UnixTime;
use crate::app::{App, Screen};
use crate::backend::{ConnectTarget, DeviceId, PeerInfo};
use crate::i18n::{format::to_western_digits, Tx, S};
use crate::remote::{RemoteBook, RemoteEntry, Store, Thumb};

/// Card width (points); columns are as many as fit.
const CARD_W: f32 = 300.0;
/// Gap between cards.
const GAP: f32 = theme::space::L;

/// The add / edit dialog's fields, as typed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RemoteForm {
    /// `None` when adding a new device.
    pub original: Option<DeviceId>,
    /// Device ID text (only editable when adding).
    pub id: String,
    /// Alias.
    pub alias: String,
    /// Validation error to show.
    pub error: Option<S>,
}

impl RemoteForm {
    /// Form pre-filled from `e`.
    pub fn edit(e: &RemoteEntry) -> Self {
        Self {
            original: Some(e.device_id),
            id: e.device_id.to_string(),
            alias: e.alias.clone(),
            error: None,
        }
    }

    /// Validate against `book` and build the entry to store (`now` for a new entry's
    /// creation time). The error is the message key to show.
    pub fn build(&self, book: &RemoteBook, now: UnixTime) -> Result<RemoteEntry, S> {
        let id = match self.original {
            Some(id) => id,
            None => DeviceId::parse(&to_western_digits(&self.id)).ok_or(S::AbErrBadId)?,
        };
        if self.original.is_none() && book.contains(id) {
            return Err(S::RemoteErrDuplicate);
        }
        let mut e = book
            .get(id)
            .cloned()
            .unwrap_or_else(|| RemoteEntry::new(id, now));
        e.alias = self.alias.trim().to_owned();
        e.sanitize();
        Ok(e)
    }
}

/// An open dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteDialog {
    /// Add or edit a device.
    Edit(RemoteForm),
    /// Confirm removing a device.
    Delete(DeviceId),
}

/// What the page asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteCommand {
    /// Start connecting to this ID (the normal connect flow).
    Connect(DeviceId),
    /// Open Settings → Security (the relay card).
    OpenRelaySettings,
}

/// Remote devices state owned by the app.
pub struct RemoteUi {
    /// The devices.
    pub book: RemoteBook,
    store: Option<Store>,
    /// Open dialog.
    pub dialog: Option<RemoteDialog>,
    /// Thumbnails given directly (README scenes, tests) instead of read from the store.
    pub preview_thumbs: HashMap<DeviceId, Thumb>,
    /// Uploaded thumbnails; `None` = looked, there is none.
    textures: HashMap<DeviceId, Option<TextureHandle>>,
    /// Fixed "now" (deterministic screenshots); `None` = the system clock.
    pub fixed_now: Option<UnixTime>,
    /// Shown briefly after add / remove.
    pub toast: Option<(String, bool)>,
}

impl RemoteUi {
    /// Load the list from `store` (`None`: memory only).
    pub fn load(store: Option<Store>) -> Self {
        let book = match &store {
            Some(s) => {
                let (book, report) = s.load();
                if let Some(p) = &report.corrupt_backup {
                    tracing::warn!(path = %p.display(), "remote devices file was moved aside");
                }
                book
            }
            None => RemoteBook::default(),
        };
        Self {
            book,
            store,
            dialog: None,
            preview_thumbs: HashMap::new(),
            textures: HashMap::new(),
            fixed_now: None,
            toast: None,
        }
    }

    /// `remote_devices.json` in the per-user data folder.
    pub fn load_default() -> Self {
        Self::load(Store::default_location())
    }

    /// The current time, or the fixed one.
    pub fn now(&self) -> UnixTime {
        self.fixed_now.unwrap_or_else(crate::address_book::now)
    }

    fn persist(&self) {
        if let Some(s) = &self.store {
            if let Err(e) = s.save(&self.book) {
                tracing::warn!("cannot save the remote devices: {e:#}");
            }
        }
    }

    /// A session through the relay reached `peer`: list it (or refresh its name and time).
    pub fn on_relay_session_started(&mut self, peer: &PeerInfo) {
        let now = self.now();
        self.book.record_connection(peer.device_id, &peer.name, now);
        self.persist();
    }

    /// A session to a listed device ended with a last picture of its screen: keep it.
    pub fn store_thumb(&mut self, id: DeviceId, thumb: Thumb) {
        if !self.book.contains(id) {
            return;
        }
        if let Some(s) = &self.store {
            if let Err(e) = s.save_thumb(id, &thumb) {
                tracing::warn!("cannot save the screen picture: {e:#}");
            }
        }
        let now = self.now();
        self.book.record_thumb(id, now);
        self.preview_thumbs.insert(id, thumb);
        self.textures.remove(&id);
        self.persist();
    }

    /// Add or replace an entry from the dialog.
    pub fn save_entry(&mut self, e: RemoteEntry, tx: Tx) {
        let name = e.display_name();
        let new = !self.book.contains(e.device_id);
        self.book.upsert(e);
        self.persist();
        if new {
            self.toast = Some((tx.f(S::RemoteSaved, &[("name", &name)]), false));
        }
    }

    /// Remove `id` and its picture.
    pub fn delete(&mut self, id: DeviceId, tx: Tx) {
        if let Some(e) = self.book.remove(id) {
            if let Some(s) = &self.store {
                s.remove_thumb(id);
            }
            self.preview_thumbs.remove(&id);
            self.textures.remove(&id);
            self.persist();
            self.toast = Some((
                tx.f(S::RemoteRemoved, &[("name", &e.display_name())]),
                false,
            ));
        }
    }

    fn texture(&mut self, ctx: &egui::Context, id: DeviceId) -> Option<TextureHandle> {
        if let Some(t) = self.textures.get(&id) {
            return t.clone();
        }
        let thumb = self
            .preview_thumbs
            .get(&id)
            .cloned()
            .or_else(|| self.store.as_ref().and_then(|s| s.load_thumb(id)));
        let tex = thumb.map(|t| {
            let image = egui::ColorImage::from_rgba_unmultiplied([t.width, t.height], &t.rgba);
            ctx.load_texture(
                format!("remote-thumb-{:09}", id.0),
                image,
                egui::TextureOptions::LINEAR,
            )
        });
        self.textures.insert(id, tex.clone());
        tex
    }
}

impl App {
    /// The Remote devices page.
    pub fn remote_ui(&mut self, ui: &mut egui::Ui) {
        let tx = self.tx;
        let mut commands = Vec::new();
        let relay = self.settings.relay.clone();
        shell::canvas(ui, |ui| {
            shell::page_title(ui, tx, tx.t(S::MenuRemote), Some(tx.t(S::RemoteLede)));
            theme::glass(ui, |ui| {
                theme::column(ui, tx.rtl(), |ui| {
                    // Header: relay status at the start, "Add" at the end.
                    theme::row(ui, tx.rtl(), |ui| {
                        if relay.is_configured() {
                            let dark = ui.visuals().dark_mode;
                            icons::icon(ui, tx, Icon::Globe, 18.0, theme::success(dark));
                            ui.label(theme::caption(
                                ui,
                                &tx.f(S::RemoteVia, &[("relay", &relay.address)]),
                            ));
                            widgets::chip(ui, tx.t(S::RemoteCap), theme::Tone::Neutral);
                        } else {
                            widgets::toast_row(
                                ui,
                                tx,
                                theme::Tone::Warning,
                                Icon::Warning,
                                tx.t(S::RemoteNoRelayTitle),
                            );
                            if ui
                                .add(icons::icon_text_button(
                                    tx,
                                    Icon::Settings,
                                    tx.t(S::RemoteSetupRelay),
                                ))
                                .clicked()
                            {
                                commands.push(RemoteCommand::OpenRelaySettings);
                            }
                        }
                        theme::row_end(ui, tx.rtl(), |ui| {
                            let add = widgets::primary_icon_button(
                                ui,
                                tx,
                                Icon::Plus,
                                tx.t(S::RemoteAdd),
                            );
                            if ui.add(add).clicked() {
                                self.remote.dialog =
                                    Some(RemoteDialog::Edit(RemoteForm::default()));
                            }
                        });
                    });
                    if !relay.is_configured() {
                        ui.label(theme::caption(ui, tx.t(S::RemoteNoRelayHint)));
                    }
                    ui.add_space(theme::space::L);
                    if self.remote.book.is_empty() {
                        widgets::empty_state(
                            ui,
                            tx,
                            Icon::Globe,
                            tx.t(S::RemoteEmptyTitle),
                            tx.t(S::RemoteEmptyHint),
                        );
                    } else {
                        let max_h = (ui.available_height() - 8.0).max(160.0);
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, true])
                            .max_height(max_h)
                            .show(ui, |ui| self.remote_cards(ui, &mut commands));
                    }
                });
            });
        });
        self.remote_dialogs(ui.ctx());
        self.remote_toast(ui.ctx());
        for c in commands {
            match c {
                RemoteCommand::Connect(id) => self.connect(ConnectTarget::Id(id)),
                RemoteCommand::OpenRelaySettings => {
                    self.settings_ui.section = super::settings::SettingsSection::Security;
                    self.settings_ui.scroll_offset = Some(1_840.0);
                    self.screen = Screen::Settings;
                }
            }
        }
    }

    /// The cards, as many per row as fit.
    fn remote_cards(&mut self, ui: &mut Ui, commands: &mut Vec<RemoteCommand>) {
        let tx = self.tx;
        let avail = ui.available_width();
        let cols = (((avail + GAP) / (CARD_W + GAP)).floor() as usize).max(1);
        let card_w = ((avail - GAP * (cols as f32 - 1.0)) / cols as f32).max(200.0);
        let entries = self.remote.book.entries().to_vec();
        let now = self.remote.now();
        let ctx = ui.ctx().clone();
        for row in entries.chunks(cols) {
            ui.spacing_mut().item_spacing.x = GAP;
            let layout = if tx.rtl() {
                Layout::right_to_left(Align::Min)
            } else {
                Layout::left_to_right(Align::Min)
            };
            ui.with_layout(layout, |ui| {
                for e in row {
                    let tex = self.remote.texture(&ctx, e.device_id);
                    ui.allocate_ui_with_layout(
                        egui::vec2(card_w, 0.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_width(card_w);
                            remote_card(
                                ui,
                                tx,
                                e,
                                tex.as_ref(),
                                now,
                                card_w,
                                commands,
                                &mut self.remote.dialog,
                            );
                        },
                    );
                }
            });
            ui.add_space(GAP);
        }
    }

    fn remote_dialogs(&mut self, ctx: &egui::Context) {
        let tx = self.tx;
        let Some(dialog) = self.remote.dialog.take() else {
            return;
        };
        self.remote.dialog = match dialog {
            RemoteDialog::Edit(form) => self.remote_edit_dialog(ctx, tx, form),
            RemoteDialog::Delete(id) => self.remote_delete_dialog(ctx, tx, id),
        };
    }

    fn remote_edit_dialog(
        &mut self,
        ctx: &egui::Context,
        tx: Tx,
        mut form: RemoteForm,
    ) -> Option<RemoteDialog> {
        let mut decision = None;
        egui::Modal::new(Id::new("remote-edit")).show(ctx, |ui| {
            ui.set_width(440.0);
            theme::column(ui, tx.rtl(), |ui| {
                let title = if form.original.is_some() {
                    S::RemoteEditTitle
                } else {
                    S::RemoteAdd
                };
                widgets::icon_heading(ui, tx, Icon::Globe, tx.t(title));
                ui.add_space(6.0);
                let w = ui.available_width();
                ui.label(theme::caption(ui, tx.t(S::DeviceId)));
                if form.original.is_some() {
                    ui.label(RichText::new(tx.n(&form.id)).monospace().strong());
                } else {
                    widgets::text_field(ui, tx, "remote-edit-id", &mut form.id, "123 456 789", w);
                }
                ui.label(theme::caption(ui, tx.t(S::AbAlias)));
                widgets::text_field(
                    ui,
                    tx,
                    "remote-edit-alias",
                    &mut form.alias,
                    tx.t(S::AbAliasHint),
                    w,
                );
                if let Some(err) = form.error {
                    let dark = ui.visuals().dark_mode;
                    ui.label(RichText::new(tx.t(err)).color(theme::danger(dark)));
                }
                ui.add_space(8.0);
                theme::row(ui, tx.rtl(), |ui| {
                    let save = widgets::primary_icon_button(ui, tx, Icon::Check, tx.t(S::Save));
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
            Some(true) => match form.build(&self.remote.book, self.remote.now()) {
                Ok(e) => {
                    self.remote.save_entry(e, tx);
                    None
                }
                Err(err) => {
                    form.error = Some(err);
                    Some(RemoteDialog::Edit(form))
                }
            },
            Some(false) => None,
            None => Some(RemoteDialog::Edit(form)),
        }
    }

    fn remote_delete_dialog(
        &mut self,
        ctx: &egui::Context,
        tx: Tx,
        id: DeviceId,
    ) -> Option<RemoteDialog> {
        let name = self.remote.book.get(id).map(RemoteEntry::display_name)?;
        let mut decision = None;
        egui::Modal::new(Id::new("remote-delete")).show(ctx, |ui| {
            ui.set_width(400.0);
            theme::column(ui, tx.rtl(), |ui| {
                widgets::icon_heading(ui, tx, Icon::Trash, tx.t(S::RemoteDeleteTitle));
                ui.add_space(4.0);
                ui.label(tx.f(S::RemoteDeleteBody, &[("name", &name)]));
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
                self.remote.delete(id, tx);
                None
            }
            Some(false) => None,
            None => Some(RemoteDialog::Delete(id)),
        }
    }

    fn remote_toast(&mut self, ctx: &egui::Context) {
        if let Some((text, error)) = self.remote.toast.take() {
            self.address_book.show_toast(text, error);
        }
        self.address_book.toast_ui(ctx, self.tx);
    }
}

/// One device card: the screen picture (or a placeholder), name, ID, last connection and the
/// actions.
#[allow(clippy::too_many_arguments)]
fn remote_card(
    ui: &mut Ui,
    tx: Tx,
    e: &RemoteEntry,
    tex: Option<&TextureHandle>,
    now: UnixTime,
    card_w: f32,
    commands: &mut Vec<RemoteCommand>,
    dialog: &mut Option<RemoteDialog>,
) {
    let dark = ui.visuals().dark_mode;
    let p = theme::tokens(ui);
    theme::card_frame(ui)
        .inner_margin(egui::Margin::same(0))
        .show(ui, |ui| {
            ui.set_width(card_w);
            theme::column(ui, tx.rtl(), |ui| {
                // Picture: 16:9, rounded top corners.
                let pic_h = (card_w * 9.0 / 16.0).round();
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(card_w, pic_h), egui::Sense::hover());
                let radius = egui::CornerRadius {
                    nw: theme::radius::XL,
                    ne: theme::radius::XL,
                    sw: 0,
                    se: 0,
                };
                match tex {
                    Some(tex) => {
                        egui::Image::from_texture(tex)
                            .corner_radius(radius)
                            .paint_at(ui, rect);
                    }
                    None => {
                        let fill = if dark {
                            Color32::from_rgb(0x2A, 0x20, 0x26)
                        } else {
                            Color32::from_rgb(0xEC, 0xE3, 0xE8)
                        };
                        ui.painter().rect_filled(rect, radius, fill);
                        let c = rect.center();
                        let icon_rect = egui::Rect::from_center_size(
                            egui::pos2(c.x, c.y - 10.0),
                            egui::vec2(36.0, 36.0),
                        );
                        Icon::Monitor
                            .image(36.0, p.text_muted)
                            .paint_at(ui, icon_rect);
                        ui.painter().text(
                            egui::pos2(c.x, c.y + 20.0),
                            egui::Align2::CENTER_CENTER,
                            tx.t(S::RemoteNoPicture),
                            egui::FontId::proportional(theme::font::CAPTION),
                            p.text_muted,
                        );
                    }
                }
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(16, 12))
                    .show(ui, |ui| {
                        ui.set_width(card_w - 32.0);
                        theme::column(ui, tx.rtl(), |ui| {
                            let name = widgets::fit(ui, &e.display_name(), card_w - 40.0, |s| {
                                RichText::new(s).strong().size(theme::font::BODY + 1.0)
                            });
                            ui.label(RichText::new(name).strong().size(theme::font::BODY + 1.0));
                            theme::row(ui, tx.rtl(), |ui| {
                                ui.label(
                                    RichText::new(tx.n(&e.device_id.to_string()))
                                        .monospace()
                                        .color(p.text_muted),
                                );
                                widgets::chip(ui, tx.t(S::RemoteViaRelay), theme::Tone::Accent);
                            });
                            let when = match e.last_connected {
                                Some(t) => super::address_book::ago_raw(tx, now, t),
                                None => tx.t(S::AbNever).to_owned(),
                            };
                            ui.label(theme::caption(
                                ui,
                                &format!("{}: {}", tx.t(S::AbLastConnected), when),
                            ));
                            ui.add_space(theme::space::S);
                            theme::row(ui, tx.rtl(), |ui| {
                                let b = widgets::primary_icon_button(
                                    ui,
                                    tx,
                                    Icon::Connect,
                                    tx.t(S::Connect),
                                );
                                if ui.add(b).clicked() {
                                    commands.push(RemoteCommand::Connect(e.device_id));
                                }
                                theme::row_end(ui, tx.rtl(), |ui| {
                                    if icons::icon_button(ui, tx, Icon::Trash, tx.t(S::AbDelete))
                                        .clicked()
                                    {
                                        *dialog = Some(RemoteDialog::Delete(e.device_id));
                                    }
                                    if icons::icon_button(ui, tx, Icon::Rename, tx.t(S::AbEdit))
                                        .clicked()
                                    {
                                        *dialog = Some(RemoteDialog::Edit(RemoteForm::edit(e)));
                                    }
                                });
                            });
                        });
                    });
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx() -> Tx {
        Tx::new(crate::i18n::Lang::Ar, crate::i18n::DigitStyle::Western)
    }

    #[test]
    fn form_validates_and_builds_entries() {
        let mut ui = RemoteUi::load(None);
        let form = RemoteForm {
            id: "١٢٣ ٤٥٦ ٧٨٩".into(),
            alias: " بيت ليان ".into(),
            ..Default::default()
        };
        let e = form.build(&ui.book, 10).unwrap();
        assert_eq!(e.device_id, DeviceId(123_456_789));
        assert_eq!(e.alias, "بيت ليان");
        ui.save_entry(e, tx());
        assert!(ui.toast.take().is_some());
        assert_eq!(form.build(&ui.book, 11), Err(S::RemoteErrDuplicate));
        let bad = RemoteForm {
            id: "12".into(),
            ..Default::default()
        };
        assert_eq!(bad.build(&ui.book, 11), Err(S::AbErrBadId));
        let mut edit = RemoteForm::edit(ui.book.get(DeviceId(123_456_789)).unwrap());
        edit.alias = "Office".into();
        let e = edit.build(&ui.book, 12).unwrap();
        assert_eq!(e.added, 10, "editing keeps the entry");
        ui.save_entry(e, tx());
        assert!(ui.toast.is_none(), "no toast for an edit");
        ui.delete(DeviceId(123_456_789), tx());
        assert!(ui.book.is_empty());
    }

    #[test]
    fn relay_sessions_land_in_the_list_with_their_picture() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().join(crate::remote::FILE_NAME));
        let mut ui = RemoteUi::load(Some(store.clone()));
        ui.fixed_now = Some(500);
        let peer = PeerInfo {
            device_id: DeviceId(5),
            name: "ليان".into(),
            address: "203.0.113.9".parse().unwrap(),
        };
        let thumb = Thumb::from_rgba(1, 1, vec![1, 2, 3, 255]).unwrap();
        ui.store_thumb(DeviceId(5), thumb.clone());
        assert!(
            store.load_thumb(DeviceId(5)).is_none(),
            "unknown device: no picture kept"
        );
        ui.on_relay_session_started(&peer);
        ui.store_thumb(DeviceId(5), thumb.clone());
        let again = RemoteUi::load(Some(store.clone()));
        let e = again.book.get(DeviceId(5)).unwrap();
        assert_eq!(
            (e.name.as_str(), e.last_connected, e.thumb_taken),
            ("ليان", Some(500), Some(500))
        );
        assert_eq!(store.load_thumb(DeviceId(5)), Some(thumb));
    }
}
