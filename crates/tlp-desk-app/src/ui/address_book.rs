//! Address book on the home screen: a tab next to the devices on this network, with search,
//! groups, favorites, sorting, add / edit / delete, import / export, Connect and Wake-on-LAN,
//! plus the "save this device?" offer after a connection and short toasts.
//!
//! State changes go through [`AddressBookUi`] methods (unit-tested without a window); the
//! `*_ui` methods only draw and collect clicks.

mod dialogs;

use eframe::egui::{self, Align, Id, Layout, RichText, Ui};

use super::icons::{self, Icon};
use super::{theme, widgets};
use crate::address_book::{
    self as ab, AddressBook, Entry, GroupFilter, MacAddr, MacSource, Query, SortBy, Store, UnixTime,
};
use crate::backend::{Backend, ConnectTarget, DeviceId, DiscoveredDevice, PeerInfo};
use crate::i18n::{format::to_western_digits, Tx, S};

/// How long a toast stays up, in seconds.
const TOAST_SECS: f64 = 4.0;
/// Longer for notices the user should read (file recovered).
const NOTICE_SECS: f64 = 9.0;

/// Which list the home screen's device card shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HomeTab {
    /// Devices discovery finds on the LAN right now.
    #[default]
    Nearby,
    /// Saved devices.
    AddressBook,
}

/// What the address book asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookCommand {
    /// Start connecting (the normal connect flow, with the one-time code field).
    Connect(ConnectTarget),
}

/// A short message at the bottom of the window.
#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    /// Display-ready text.
    pub text: String,
    /// Shown in the danger color.
    pub error: bool,
    secs: f64,
    /// Set when first drawn.
    until: Option<f64>,
}

/// The add / edit dialog's fields, as typed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EditForm {
    /// `None` when adding a new device.
    pub original: Option<DeviceId>,
    /// Device ID text (only editable when adding).
    pub id: String,
    /// Alias.
    pub alias: String,
    /// Group.
    pub group: String,
    /// Tags, comma separated.
    pub tags: String,
    /// Notes.
    pub notes: String,
    /// MAC address text.
    pub mac: String,
    /// Validation error to show.
    pub error: Option<S>,
}

impl EditForm {
    /// Empty form for a new device.
    pub fn add() -> Self {
        Self::default()
    }

    /// Form pre-filled from `e`.
    pub fn edit(e: &Entry) -> Self {
        Self {
            original: Some(e.device_id),
            id: e.device_id.to_string(),
            alias: e.alias.clone(),
            group: e.group.clone(),
            tags: e.tags.join(", "),
            notes: e.notes.clone(),
            mac: e.mac.map(|m| m.to_string()).unwrap_or_default(),
            error: None,
        }
    }

    /// Validate the form against `book` and build the entry to store. The error is the
    /// message key to show.
    pub fn build(&self, book: &AddressBook) -> Result<Entry, S> {
        let id = match self.original {
            Some(id) => id,
            None => DeviceId::parse(&to_western_digits(&self.id)).ok_or(S::AbErrBadId)?,
        };
        if self.original.is_none() && book.contains(id) {
            return Err(S::AbErrDuplicate);
        }
        let mut e = book.get(id).cloned().unwrap_or_else(|| Entry::new(id));
        e.alias = self.alias.trim().to_owned();
        e.group = self.group.trim().to_owned();
        e.tags = ab::parse_tags(&self.tags);
        e.notes = self.notes.trim().to_owned();
        let mac_text = self.mac.trim();
        if mac_text.is_empty() {
            e.mac = None;
            e.mac_source = MacSource::Learned;
        } else {
            let mac = MacAddr::parse(mac_text).ok_or(S::AbErrBadMac)?;
            if e.mac != Some(mac) {
                e.mac = Some(mac);
                e.mac_source = MacSource::Manual;
            }
        }
        e.sanitize();
        Ok(e)
    }
}

/// An open dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dialog {
    /// Add or edit a device.
    Edit(EditForm),
    /// Confirm deleting a device.
    Delete(DeviceId),
    /// Import from the file path being typed.
    Import(String),
    /// Export to the file path being typed.
    Export(String),
}

/// Address book state owned by the app.
pub struct AddressBookUi {
    /// The saved devices.
    pub book: AddressBook,
    store: Option<Store>,
    /// Tab shown on the home screen.
    pub tab: HomeTab,
    /// Search box text.
    pub query: String,
    /// Group filter.
    pub filter: GroupFilter,
    /// List order.
    pub sort: SortBy,
    /// Open dialog.
    pub dialog: Option<Dialog>,
    /// Current toast.
    pub toast: Option<Toast>,
    /// A device we just connected to that is not saved: offer to save it.
    pub offer_save: Option<PeerInfo>,
    /// Last discovery snapshot.
    devices: Vec<DiscoveredDevice>,
    /// Fixed "now" for relative times and timestamps (deterministic screenshots); `None` =
    /// the system clock.
    pub fixed_now: Option<UnixTime>,
}

impl AddressBookUi {
    /// Load the book from `store` (`None`: memory only). Recovery notices become a toast.
    pub fn load(store: Option<Store>, tx: Tx) -> Self {
        let (book, report) = match &store {
            Some(s) => s.load(),
            None => (AddressBook::default(), ab::LoadReport::default()),
        };
        let mut me = Self {
            book,
            store,
            tab: HomeTab::default(),
            query: String::new(),
            filter: GroupFilter::default(),
            sort: SortBy::default(),
            dialog: None,
            toast: None,
            offer_save: None,
            devices: Vec::new(),
            fixed_now: None,
        };
        if let Some(path) = &report.corrupt_backup {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            me.notice(tx.f(S::AbRecovered, &[("path", &name)]), true, NOTICE_SECS);
        } else if report.restored_from_backup {
            me.notice(tx.t(S::AbRestoredBackup).into(), true, NOTICE_SECS);
        }
        me
    }

    /// `%APPDATA%\TLP-Desk\address_book.json`.
    pub fn load_default(tx: Tx) -> Self {
        Self::load(Store::default_location(), tx)
    }

    fn notice(&mut self, text: String, error: bool, secs: f64) {
        self.toast = Some(Toast {
            text,
            error,
            secs,
            until: None,
        });
    }

    /// Show a short display-ready message at the bottom of the window.
    pub fn show_toast(&mut self, text: String, error: bool) {
        self.notice(text, error, TOAST_SECS);
    }

    /// The current time ([`AddressBookUi::fixed_now`] if set).
    pub fn now(&self) -> UnixTime {
        self.fixed_now.unwrap_or_else(ab::now)
    }

    fn persist(&mut self, tx: Tx) {
        let Some(store) = &self.store else { return };
        if let Err(e) = store.save(&self.book) {
            tracing::warn!("cannot save the address book: {e:#}");
            let detail = format!("{e:#}");
            self.show_toast(tx.f(S::AbSaveError, &[("detail", &detail)]), true);
        }
    }

    /// Whether discovery currently sees `id`.
    pub fn is_online(&self, id: DeviceId) -> bool {
        self.devices.iter().any(|d| d.device_id == id && d.online)
    }

    /// A new discovery snapshot: refresh online status, last seen and addresses, and learn
    /// MACs of saved devices that have none yet.
    pub fn on_devices(&mut self, list: &[DiscoveredDevice], backend: &dyn Backend, tx: Tx) {
        self.devices = list.to_vec();
        let mut changed = self.book.observe(list, self.now());
        for d in list.iter().filter(|d| d.online) {
            let wants_mac = self.book.get(d.device_id).is_some_and(|e| e.mac.is_none());
            if wants_mac {
                if let Some(mac) = backend.neighbor_mac(d.address) {
                    changed |= self.book.learn_mac(d.device_id, MacAddr(mac));
                }
            }
        }
        if changed {
            self.persist(tx);
        }
    }

    /// A viewer session started: update a saved device, or offer to save a new one.
    pub fn on_session_started(&mut self, peer: &PeerInfo, backend: &dyn Backend, tx: Tx) {
        if self.book.contains(peer.device_id) {
            self.record(peer, backend, self.now());
            self.persist(tx);
        } else {
            self.offer_save = Some(peer.clone());
        }
    }

    fn record(&mut self, peer: &PeerInfo, backend: &dyn Backend, now: UnixTime) {
        let mac = backend.neighbor_mac(peer.address).map(MacAddr);
        let fp = backend.pinned_fingerprint(peer.device_id);
        self.book.record_connection(peer, mac, fp, now);
    }

    /// Save a device we connected to (the "save this device?" offer).
    pub fn save_peer(&mut self, peer: &PeerInfo, backend: &dyn Backend, tx: Tx) {
        let now = self.now();
        self.book
            .save_device(peer.device_id, &peer.name, Some(peer.address), now);
        self.record(peer, backend, now);
        self.offer_save = None;
        self.persist(tx);
        let name = self
            .book
            .get(peer.device_id)
            .map(Entry::display_name)
            .unwrap_or_default();
        self.show_toast(tx.f(S::AbSavedToast, &[("name", &name)]), false);
    }

    /// Save a device from the discovered list.
    pub fn save_discovered(&mut self, dev: &DiscoveredDevice, backend: &dyn Backend, tx: Tx) {
        self.book
            .save_device(dev.device_id, &dev.name, Some(dev.address), self.now());
        if let Some(mac) = backend.neighbor_mac(dev.address) {
            self.book.learn_mac(dev.device_id, MacAddr(mac));
        }
        if let Some(fp) = backend.pinned_fingerprint(dev.device_id) {
            if let Some(e) = self.book.get(dev.device_id).cloned() {
                self.book.upsert(Entry {
                    pinned_fingerprint: Some(fp),
                    ..e
                });
            }
        }
        self.persist(tx);
        let name = self
            .book
            .get(dev.device_id)
            .map(Entry::display_name)
            .unwrap_or_default();
        self.show_toast(tx.f(S::AbSavedToast, &[("name", &name)]), false);
    }

    /// Send a Wake-on-LAN packet to a saved device and report the outcome as a toast.
    pub fn wake(&mut self, id: DeviceId, backend: &dyn Backend, tx: Tx) {
        let Some(e) = self.book.get(id) else { return };
        let name = e.display_name();
        let Some(mac) = e.mac else {
            self.show_toast(tx.t(S::AbWakeNoMac).into(), true);
            return;
        };
        match backend.wake_on_lan(mac.0, &e.last_ips) {
            Ok(()) => self.show_toast(tx.f(S::AbWakeSent, &[("name", &name)]), false),
            Err(detail) => self.show_toast(tx.f(S::AbWakeFailed, &[("detail", &detail)]), true),
        }
    }

    /// Store the edit dialog's result. Returns `false` (and sets the form error) when the form
    /// is invalid.
    pub fn apply_edit(&mut self, form: &mut EditForm, tx: Tx) -> bool {
        match form.build(&self.book) {
            Ok(mut e) => {
                if form.original.is_none() {
                    if let Some(d) = self.devices.iter().find(|d| d.device_id == e.device_id) {
                        e.name = d.name.clone();
                        e.push_ip(d.address);
                        e.last_seen = Some(self.now());
                    }
                }
                self.book.upsert(e);
                self.persist(tx);
                true
            }
            Err(key) => {
                form.error = Some(key);
                false
            }
        }
    }

    /// Delete a saved device.
    pub fn delete(&mut self, id: DeviceId, tx: Tx) {
        if self.book.remove(id).is_some() {
            self.persist(tx);
        }
    }

    /// Flip a device's favorite flag.
    pub fn toggle_favorite(&mut self, id: DeviceId, tx: Tx) {
        if self.book.toggle_favorite(id) {
            self.persist(tx);
        }
    }

    /// Import (merge) an exported address book.
    pub fn import(&mut self, path: &str, tx: Tx) -> bool {
        match ab::import_file(std::path::Path::new(path.trim())) {
            Ok(other) => {
                let s = self.book.merge(other);
                self.persist(tx);
                let (added, updated) = (s.added.to_string(), s.updated.to_string());
                self.show_toast(
                    tx.f(
                        S::AbImportDone,
                        &[
                            ("added", &tx.digits(&added)),
                            ("updated", &tx.digits(&updated)),
                        ],
                    ),
                    false,
                );
                true
            }
            Err(e) => {
                self.show_toast(tx.f(S::AbFileError, &[("detail", &format!("{e:#}"))]), true);
                false
            }
        }
    }

    /// Export the book to `path`.
    pub fn export(&mut self, path: &str, tx: Tx) -> bool {
        let path = path.trim();
        match ab::export_file(&self.book, std::path::Path::new(path)) {
            Ok(()) => {
                self.show_toast(tx.f(S::AbExportDone, &[("path", path)]), false);
                true
            }
            Err(e) => {
                self.show_toast(tx.f(S::AbFileError, &[("detail", &format!("{e:#}"))]), true);
                false
            }
        }
    }
}

/// Suggested import / export file: `Documents\TLP-Desk-address-book.json`.
fn default_exchange_path() -> String {
    let file = "TLP-Desk-address-book.json";
    std::env::var_os("USERPROFILE").map_or_else(
        || file.to_owned(),
        |home| {
            std::path::PathBuf::from(home)
                .join("Documents")
                .join(file)
                .to_string_lossy()
                .into_owned()
        },
    )
}

/// Arabic plural category of `n` for the "… ago" strings: one, two, few (3-10), many
/// (11-99), other (100, 101, 102 …). English uses one / two / the rest.
fn plural(n: u64) -> usize {
    match (n, n % 100) {
        (1, _) => 0,
        (2, _) => 1,
        (_, 3..=10) => 2,
        (_, 11..=99) => 3,
        _ => 4,
    }
}

/// "5 minutes ago" in logical order (not shaped), digit style applied.
pub fn ago_raw(tx: Tx, now: UnixTime, then: UnixTime) -> String {
    use S::*;
    let secs = now.saturating_sub(then);
    let (n, keys) = if secs < 60 {
        return tx.raw(AgoJustNow).to_owned();
    } else if secs < 3600 {
        (
            secs / 60,
            [
                AgoMinute1,
                AgoMinute2,
                AgoMinutesFew,
                AgoMinutesMany,
                AgoMinutesMany,
            ],
        )
    } else if secs < 86_400 {
        (
            secs / 3600,
            [AgoHour1, AgoHour2, AgoHoursFew, AgoHoursMany, AgoHoursMany],
        )
    } else {
        (
            secs / 86_400,
            [AgoDay1, AgoDay2, AgoDaysFew, AgoDaysMany, AgoDaysOther],
        )
    };
    let key = keys.get(plural(n)).copied().unwrap_or(AgoJustNow);
    tx.raw(key).replace("{n}", &tx.digits(&n.to_string()))
}

/// Things the user clicked in the list, applied after drawing.
enum RowAction {
    Connect(DeviceId),
    Wake(DeviceId),
    Edit(DeviceId),
    Delete(DeviceId),
    Favorite(DeviceId),
}

impl AddressBookUi {
    /// The tab's content: toolbar, filters and the list.
    pub fn list_ui(&mut self, ui: &mut Ui, tx: Tx, backend: &dyn Backend) -> Option<BookCommand> {
        self.toolbar_ui(ui, tx);
        ui.add_space(4.0);
        let now = self.now();
        let query = Query::new(&self.query);
        let mut actions = Vec::new();
        let view = self
            .book
            .view(&query, &self.filter, self.sort, |id| self.is_online(id));
        if view.is_empty() {
            let key = if self.book.entries().is_empty() {
                S::AbEmpty
            } else {
                S::AbNoMatches
            };
            ui.label(theme::caption(ui, tx.t(key)));
        }
        egui::ScrollArea::vertical()
            .id_salt("ab-list")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for e in &view {
                    entry_row(ui, tx, e, self.is_online(e.device_id), now, &mut actions);
                }
            });
        let mut command = None;
        for a in actions {
            match a {
                RowAction::Connect(id) => {
                    command = Some(BookCommand::Connect(ConnectTarget::Id(id)))
                }
                RowAction::Wake(id) => self.wake(id, backend, tx),
                RowAction::Edit(id) => {
                    if let Some(e) = self.book.get(id) {
                        self.dialog = Some(Dialog::Edit(EditForm::edit(e)));
                    }
                }
                RowAction::Delete(id) => self.dialog = Some(Dialog::Delete(id)),
                RowAction::Favorite(id) => self.toggle_favorite(id, tx),
            }
        }
        command
    }

    fn toolbar_ui(&mut self, ui: &mut Ui, tx: Tx) {
        theme::row(ui, tx.rtl(), |ui| {
            let weak = ui.visuals().weak_text_color();
            icons::icon(ui, tx, Icon::Search, 18.0, weak);
            let w = (ui.available_width() - 8.0).max(120.0);
            widgets::text_field(
                ui,
                tx,
                "ab-search",
                &mut self.query,
                tx.t(S::AbSearchHint),
                w,
            )
        });
        theme::row(ui, tx.rtl(), |ui| {
            let groups = self.book.groups();
            let label = match &self.filter {
                GroupFilter::All => tx.t(S::AbAllGroups).to_owned(),
                GroupFilter::Favorites => tx.t(S::AbFavorites).to_owned(),
                GroupFilter::Group(g) => tx.d(g),
            };
            egui::ComboBox::from_id_salt("ab-group")
                .selected_text(label)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.filter, GroupFilter::All, tx.t(S::AbAllGroups));
                    ui.selectable_value(
                        &mut self.filter,
                        GroupFilter::Favorites,
                        tx.t(S::AbFavorites),
                    );
                    for g in groups {
                        let shown = tx.d(&g);
                        ui.selectable_value(&mut self.filter, GroupFilter::Group(g), shown);
                    }
                });
            let sort_label = |s: SortBy| match s {
                SortBy::Name => tx.t(S::AbSortName),
                SortBy::LastSeen => tx.t(S::AbSortLastSeen),
            };
            egui::ComboBox::from_id_salt("ab-sort")
                .selected_text(sort_label(self.sort))
                .show_ui(ui, |ui| {
                    for s in [SortBy::Name, SortBy::LastSeen] {
                        ui.selectable_value(&mut self.sort, s, sort_label(s));
                    }
                })
                .response
                .on_hover_text(tx.t(S::AbSortBy));
            theme::row_end(ui, tx.rtl(), |ui| {
                if icons::icon_button(ui, tx, Icon::Upload, tx.t(S::AbExport)).clicked() {
                    self.dialog = Some(Dialog::Export(default_exchange_path()));
                }
                if icons::icon_button(ui, tx, Icon::Download, tx.t(S::AbImport)).clicked() {
                    self.dialog = Some(Dialog::Import(default_exchange_path()));
                }
                if ui
                    .add(icons::icon_text_button(tx, Icon::Plus, tx.t(S::AbAdd)))
                    .clicked()
                {
                    self.dialog = Some(Dialog::Edit(EditForm::add()));
                }
            });
        });
    }

    /// The "save this device?" offer after connecting to an unsaved device.
    pub fn offer_ui(&mut self, ui: &mut Ui, tx: Tx, backend: &dyn Backend) {
        let Some(peer) = self.offer_save.clone() else {
            return;
        };
        let mut answer = None;
        theme::card(ui, |ui| {
            theme::row(ui, tx.rtl(), |ui| {
                let accent = theme::accent(ui.visuals().dark_mode);
                icons::icon(ui, tx, Icon::AddressBook, 20.0, accent);
                ui.label(tx.f(S::AbOfferSave, &[("name", &peer.name)]));
                theme::row_end(ui, tx.rtl(), |ui| {
                    if ui
                        .add(icons::icon_text_button(tx, Icon::Close, tx.t(S::AbNotNow)))
                        .clicked()
                    {
                        answer = Some(false);
                    }
                    let save =
                        widgets::primary_icon_button(ui, tx, Icon::Star, tx.t(S::AbSaveDevice));
                    if ui.add(save).clicked() {
                        answer = Some(true);
                    }
                });
            });
        });
        ui.add_space(12.0);
        match answer {
            Some(true) => self.save_peer(&peer, backend, tx),
            Some(false) => self.offer_save = None,
            None => {}
        }
    }

    /// Draw the toast, if any.
    pub fn toast_ui(&mut self, ctx: &egui::Context, tx: Tx) {
        let now = ctx.input(|i| i.time);
        let Some(toast) = self.toast.as_mut() else {
            return;
        };
        let until = *toast.until.get_or_insert(now + toast.secs);
        if now >= until {
            self.toast = None;
            return;
        }
        let (text, error) = (toast.text.clone(), toast.error);
        egui::Area::new(Id::new("ab-toast"))
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -24.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                let dark = ui.visuals().dark_mode;
                let color = if error {
                    theme::danger(dark)
                } else {
                    theme::success(dark)
                };
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let icon = if error { Icon::Warning } else { Icon::Check };
                        // Icon at the reading start.
                        if tx.rtl() {
                            ui.label(RichText::new(text).strong());
                            icons::icon(ui, tx, icon, 18.0, color);
                        } else {
                            icons::icon(ui, tx, icon, 18.0, color);
                            ui.label(RichText::new(text).strong());
                        }
                    });
                });
            });
        ctx.request_repaint_after(std::time::Duration::from_secs_f64((until - now).max(0.05)));
    }
}

/// One saved device: status dot, favorite star, name + meta line, actions at the reading end.
fn entry_row(
    ui: &mut Ui,
    tx: Tx,
    e: &Entry,
    online: bool,
    now: UnixTime,
    actions: &mut Vec<RowAction>,
) {
    let dark = ui.visuals().dark_mode;
    let id = e.device_id;
    let rtl = tx.rtl();
    ui.push_id(("ab-row", id.0), |ui| {
        widgets::list_row(ui, |ui| {
            let (row, _) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 36.0), egui::Sense::hover());
            let (start, end) = if rtl {
                (
                    Layout::right_to_left(Align::Center),
                    Layout::left_to_right(Align::Center),
                )
            } else {
                (
                    Layout::left_to_right(Align::Center),
                    Layout::right_to_left(Align::Center),
                )
            };
            // Actions first: name and meta line get only what is left, so they never run
            // under the buttons (long IPv6 addresses, long Arabic names).
            let mut trail = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(end));
            entry_actions(&mut trail, tx, e, online, actions);
            let trailing = trail.min_rect();
            let gap = theme::space::M;
            let lead_rect = if rtl {
                egui::Rect::from_min_max(egui::pos2(trailing.right() + gap, row.top()), row.max)
            } else {
                egui::Rect::from_min_max(row.min, egui::pos2(trailing.left() - gap, row.bottom()))
            };
            let mut lead = ui.new_child(egui::UiBuilder::new().max_rect(lead_rect).layout(start));
            lead.set_clip_rect(lead_rect.intersect(ui.clip_rect()));
            let color = if online {
                theme::success(dark)
            } else {
                lead.visuals().weak_text_color()
            };
            theme::status_dot(&mut lead, color);
            let fav_tip = if e.favorite {
                S::AbFavoriteRemove
            } else {
                S::AbFavoriteAdd
            };
            let fav = widgets::ghost_scope(&mut lead, |ui| {
                icons::icon_toggle(ui, tx, e.favorite, Icon::Star, tx.t(fav_tip))
            });
            if fav.clicked() {
                actions.push(RowAction::Favorite(id));
            }
            theme::column(&mut lead, rtl, |ui| {
                let max_w = ui.available_width();
                let full_name = e.display_name();
                let name = widgets::fit(ui, &full_name, max_w, |s| RichText::new(tx.d(s)).strong());
                let r = ui.label(RichText::new(tx.d(&name)).strong());
                if name != full_name {
                    r.on_hover_text(tx.d(&full_name));
                }
                let full_meta = meta_line(tx, e, online, now);
                let meta = widgets::fit(ui, &full_meta, max_w, |s| theme::caption(ui, &tx.p(s)));
                let r = ui.label(theme::caption(ui, &tx.p(&meta)));
                if meta != full_meta {
                    r.on_hover_text(tx.p(&full_meta));
                }
            });
        });
    });
}

/// Connect (or Wake), edit and delete, from the reading end of the row.
fn entry_actions(ui: &mut Ui, tx: Tx, e: &Entry, online: bool, actions: &mut Vec<RowAction>) {
    let id = e.device_id;
    widgets::ghost_scope(ui, |ui| {
        if icons::icon_button(ui, tx, Icon::Trash, tx.t(S::AbDelete)).clicked() {
            actions.push(RowAction::Delete(id));
        }
        if icons::icon_button(ui, tx, Icon::Rename, tx.t(S::AbEdit)).clicked() {
            actions.push(RowAction::Edit(id));
        }
    });
    if online {
        let b = icons::icon_text_button(tx, Icon::Connect, tx.t(S::Connect));
        if ui.add(b).clicked() {
            actions.push(RowAction::Connect(id));
        }
    } else {
        let b = icons::icon_text_button(tx, Icon::WakeOnLan, tx.t(S::AbWake));
        let tip = if e.mac.is_some() {
            S::AbWakeTooltip
        } else {
            S::AbWakeNoMac
        };
        let resp = ui.add_enabled(e.mac.is_some(), b);
        let resp = resp
            .on_hover_text(tx.t(tip))
            .on_disabled_hover_text(tx.t(tip));
        if resp.clicked() {
            actions.push(RowAction::Wake(id));
        }
    }
}
/// "ID · IP · group · Available / Last seen 5 minutes ago", logical order (shape once).
fn meta_line(tx: Tx, e: &Entry, online: bool, now: UnixTime) -> String {
    let mut parts = vec![tx.digits(&e.device_id.to_string())];
    if let Some(ip) = e.last_ips.first() {
        parts.push(tx.digits(&ip.to_string()));
    }
    if !e.group.is_empty() {
        parts.push(e.group.clone());
    }
    let status = if online {
        tx.raw(S::Online).to_owned()
    } else {
        match e.last_seen {
            Some(t) => format!("{}: {}", tx.raw(S::AbSortLastSeen), ago_raw(tx, now, t)),
            None => tx.raw(S::Offline).to_owned(),
        }
    };
    parts.push(status);
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::sync::Mutex;

    use tlp_desk_proto::InputEvent;

    use super::*;
    use crate::backend::{AttemptId, LocalInfo, RequestId, SessionId};
    use crate::i18n::{DigitStyle, Lang};

    type Wakes = Mutex<Vec<([u8; 6], Vec<IpAddr>)>>;

    /// Backend that records Wake-on-LAN requests and answers the address book hooks.
    #[derive(Default)]
    struct Rec {
        wakes: Wakes,
        mac: Option<[u8; 6]>,
    }

    impl Backend for Rec {
        fn local_info(&self) -> LocalInfo {
            LocalInfo {
                device_id: DeviceId(1),
                device_name: String::new(),
                addresses: vec![],
                one_time_code: String::new(),
                identity_conflict: None,
            }
        }
        fn set_device_name(&self, _: String) {}
        fn refresh_one_time_code(&self) {}
        fn connect(&self, _: ConnectTarget, _: Option<String>) -> AttemptId {
            AttemptId(0)
        }
        fn cancel_connect(&self, _: AttemptId) {}
        fn forget_host_key(&self, _: AttemptId) -> bool {
            false
        }
        fn respond_to_request(&self, _: RequestId, _: bool) {}
        fn send_input(&self, _: SessionId, _: InputEvent) {}
        fn set_view_only(&self, _: SessionId, _: bool) {}
        fn disconnect(&self, _: SessionId) {}
        fn select_monitor(&self, _: SessionId, _: u32) {}
        fn neighbor_mac(&self, _: IpAddr) -> Option<[u8; 6]> {
            self.mac
        }
        fn pinned_fingerprint(&self, _: DeviceId) -> Option<String> {
            Some("ab12 cd34 ef56 7890".into())
        }
        fn wake_on_lan(&self, mac: [u8; 6], last_ips: &[IpAddr]) -> Result<(), String> {
            self.wakes.lock().unwrap().push((mac, last_ips.to_vec()));
            Ok(())
        }
    }

    fn en() -> Tx {
        Tx::new(Lang::En, DigitStyle::Western)
    }

    fn ar() -> Tx {
        Tx::new(Lang::Ar, DigitStyle::Western)
    }

    fn peer(id: u32, name: &str, ip: &str) -> PeerInfo {
        PeerInfo {
            device_id: DeviceId(id),
            name: name.into(),
            address: ip.parse().unwrap(),
        }
    }

    fn on_disk(dir: &tempfile::TempDir) -> AddressBookUi {
        AddressBookUi::load(Some(Store::at(dir.path().join(ab::FILE_NAME))), en())
    }

    #[test]
    fn relative_times_follow_english_and_arabic_number_agreement() {
        let now = 10_000_000;
        let cases_en = [
            (30, "just now"),
            (60, "1 minute ago"),
            (120, "2 minutes ago"),
            (5 * 60, "5 minutes ago"),
            (3 * 3600, "3 hours ago"),
            (86_400, "1 day ago"),
        ];
        let plain = |s: String| s.replace(['\u{2066}', '\u{2069}'], "");
        for (secs, want) in cases_en {
            assert_eq!(plain(ago_raw(en(), now, now - secs)), want);
        }
        let ar_has = |secs: u64, word: &str| {
            let s = ago_raw(ar(), now, now - secs);
            assert!(s.contains(word), "{secs}s: {s:?} lacks {word:?}");
            s
        };
        assert_eq!(ar_has(60, "دقيقة"), "منذ دقيقة");
        assert_eq!(ar_has(120, "دقيقتين"), "منذ دقيقتين");
        ar_has(5 * 60, "دقائق");
        assert!(!ar_has(15 * 60, "دقيقة").contains("دقائق"));
        ar_has(2 * 3600, "ساعتين");
        ar_has(4 * 3600, "ساعات");
        ar_has(3 * 86_400, "أيام");
        ar_has(11 * 86_400, "يومًا");
        ar_has(100 * 86_400, "يوم");
        // The number is an LTR isolate in the user's digit style.
        let indic = Tx::new(Lang::Ar, DigitStyle::ArabicIndic);
        assert!(ago_raw(indic, now, now - 5 * 60).contains("\u{2066}٥\u{2069}"));
        assert_eq!(
            ago_raw(en(), 5, 10),
            "just now",
            "clock skew is not negative"
        );
    }

    #[test]
    fn edit_form_validates_ids_duplicates_and_macs() {
        let mut book = AddressBook::default();
        book.upsert(Entry::new(DeviceId(111_111_111)));
        let mut form = EditForm::add();
        form.id = "12".into();
        assert_eq!(form.build(&book), Err(S::AbErrBadId));
        form.id = "111 111 111".into();
        assert_eq!(form.build(&book), Err(S::AbErrDuplicate));
        form.id = "٢٢٢-٢٢٢-٢٢٢".into();
        form.mac = "zz".into();
        assert_eq!(form.build(&book), Err(S::AbErrBadMac));
        form.mac = "00-1A-2B-3C-4D-5E".into();
        form.alias = "  جهاز المحاسبة  ".into();
        form.tags = "مالية، طابعة".into();
        let e = form.build(&book).unwrap();
        assert_eq!(e.device_id, DeviceId(222_222_222));
        assert_eq!(e.alias, "جهاز المحاسبة");
        assert_eq!(e.tags, vec!["مالية".to_owned(), "طابعة".into()]);
        assert_eq!(e.mac_source, MacSource::Manual);
    }

    #[test]
    fn editing_keeps_a_learned_mac_learned_until_it_is_changed() {
        let mut book = AddressBook::default();
        let mut e = Entry::new(DeviceId(5));
        e.learn_mac(MacAddr([0, 1, 2, 3, 4, 5]));
        book.upsert(e.clone());
        let mut form = EditForm::edit(&e);
        form.alias = "x".into();
        assert_eq!(form.build(&book).unwrap().mac_source, MacSource::Learned);
        form.mac = "00:01:02:03:04:06".into();
        assert_eq!(form.build(&book).unwrap().mac_source, MacSource::Manual);
        form.mac.clear();
        assert_eq!(form.build(&book).unwrap().mac, None);
    }

    #[test]
    fn unsaved_connection_is_offered_then_saved_with_mac_and_key() {
        let dir = tempfile::tempdir().unwrap();
        let mut ui = on_disk(&dir);
        let backend = Rec {
            mac: Some([0x00, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e]),
            ..Rec::default()
        };
        let p = peer(306_117_942, "حاسوب-المكتبة", "192.168.1.57");
        ui.on_session_started(&p, &backend, en());
        assert_eq!(ui.offer_save, Some(p.clone()));
        assert!(!ui.book.contains(p.device_id));
        ui.save_peer(&p, &backend, en());
        assert!(ui.offer_save.is_none());
        assert!(!ui.toast.as_ref().unwrap().error);
        // It is on disk, Arabic name intact, with the learned MAC and the pinned key.
        let (book, _) = Store::at(dir.path().join(ab::FILE_NAME)).load();
        let e = book.get(p.device_id).unwrap();
        assert_eq!(e.name, "حاسوب-المكتبة");
        assert_eq!(e.mac, Some(MacAddr([0x00, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e])));
        assert_eq!(e.pinned_fingerprint.as_deref(), Some("ab12 cd34 ef56 7890"));
        assert!(e.last_connected.is_some());
    }

    #[test]
    fn saved_connection_is_recorded_without_an_offer() {
        let mut ui = AddressBookUi::load(None, en());
        let p = peer(7, "PC", "10.0.0.7");
        ui.book.save_device(p.device_id, "old", None, 1);
        ui.on_session_started(&p, &Rec::default(), en());
        assert!(ui.offer_save.is_none());
        let e = ui.book.get(p.device_id).unwrap();
        assert_eq!(e.name, "PC");
        assert_eq!(
            e.last_ips.first().map(ToString::to_string).as_deref(),
            Some("10.0.0.7")
        );
    }

    #[test]
    fn discovery_sets_online_status_and_learns_missing_macs() {
        let mut ui = AddressBookUi::load(None, en());
        ui.book.save_device(DeviceId(9), "", None, 1);
        let backend = Rec {
            mac: Some([0x02, 0, 0, 0, 0, 9]),
            ..Rec::default()
        };
        let list = vec![DiscoveredDevice {
            device_id: DeviceId(9),
            name: "مختبر".into(),
            address: "192.168.1.9".parse().unwrap(),
            online: true,
            app_version: None,
        }];
        ui.on_devices(&list, &backend, en());
        assert!(ui.is_online(DeviceId(9)));
        let e = ui.book.get(DeviceId(9)).unwrap();
        assert_eq!(e.mac, Some(MacAddr([0x02, 0, 0, 0, 0, 9])));
        assert_eq!(e.name, "مختبر");
        ui.on_devices(&[], &backend, en());
        assert!(!ui.is_online(DeviceId(9)));
    }

    #[test]
    fn wake_needs_a_mac_and_aims_at_the_last_addresses() {
        let mut ui = AddressBookUi::load(None, ar());
        let backend = Rec::default();
        ui.book
            .save_device(DeviceId(3), "", "192.168.1.30".parse().ok(), 1);
        ui.wake(DeviceId(3), &backend, ar());
        assert!(backend.wakes.lock().unwrap().is_empty());
        assert!(ui.toast.as_ref().unwrap().error);
        ui.book.learn_mac(DeviceId(3), MacAddr([0, 1, 2, 3, 4, 5]));
        ui.wake(DeviceId(3), &backend, ar());
        let wakes = backend.wakes.lock().unwrap().clone();
        assert_eq!(
            wakes,
            vec![([0, 1, 2, 3, 4, 5], vec!["192.168.1.30".parse().unwrap()])]
        );
        assert!(!ui.toast.as_ref().unwrap().error);
    }

    #[test]
    fn corrupt_file_on_startup_becomes_a_notice() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(ab::FILE_NAME), "{ not json").unwrap();
        let ui = on_disk(&dir);
        let toast = ui.toast.unwrap();
        assert!(toast.error);
        assert!(
            toast.text.contains("address_book.corrupt-"),
            "{}",
            toast.text
        );
    }

    #[test]
    fn import_merges_and_export_writes_the_chosen_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut ui = on_disk(&dir);
        ui.book.save_device(DeviceId(1), "واحد", None, 1);
        let path = dir.path().join("تصدير.json");
        let path = path.to_str().unwrap();
        assert!(ui.export(path, en()));
        let mut other = AddressBookUi::load(None, en());
        assert!(other.import(path, en()));
        assert!(other.book.contains(DeviceId(1)));
        assert!(!other.import("Z:\\no\\such\\file.json", en()));
        assert!(other.toast.unwrap().error);
    }

    /// Draws the tab, every dialog and a toast headlessly in both languages: no panic, and the
    /// modal reports itself as open.
    #[test]
    fn address_book_draws_headless_in_english_and_arabic() {
        for tx in [en(), ar()] {
            let ctx = egui::Context::default();
            theme::install(&ctx);
            theme::apply_lang(&ctx, tx.lang);
            let backend = Rec::default();
            let mut ui_state = AddressBookUi::load(None, tx);
            ui_state
                .book
                .save_device(DeviceId(1), "مكتب أحمد", "192.168.1.5".parse().ok(), 1);
            ui_state.book.save_device(DeviceId(2), "Lab", None, 1);
            ui_state
                .book
                .learn_mac(DeviceId(2), MacAddr([0, 1, 2, 3, 4, 5]));
            ui_state.book.toggle_favorite(DeviceId(2));
            ui_state.on_devices(
                &[DiscoveredDevice {
                    device_id: DeviceId(1),
                    name: "مكتب أحمد".into(),
                    address: "192.168.1.5".parse().unwrap(),
                    online: true,
                    app_version: None,
                }],
                &backend,
                tx,
            );
            ui_state.offer_save = Some(peer(3, "ضيف", "192.168.1.3"));
            ui_state.show_toast(tx.f(S::AbWakeSent, &[("name", "Lab")]), false);
            let dialogs = [
                Dialog::Edit(EditForm::edit(ui_state.book.get(DeviceId(1)).unwrap())),
                Dialog::Edit(EditForm::add()),
                Dialog::Delete(DeviceId(2)),
                Dialog::Import("C:\\بيانات\\book.json".into()),
                Dialog::Export(String::new()),
            ];
            for dialog in dialogs {
                ui_state.dialog = Some(dialog);
                for _ in 0..2 {
                    let input = egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1000.0, 700.0),
                        )),
                        ..Default::default()
                    };
                    let mut open = false;
                    let mut out = ctx.run_ui(input, |ui| {
                        egui::CentralPanel::default().show(ui, |ui| {
                            ui_state.offer_ui(ui, tx, &backend);
                            let cmd = ui_state.list_ui(ui, tx, &backend);
                            assert!(cmd.is_none());
                        });
                        open = ui_state.dialogs_ui(ui.ctx(), tx);
                        ui_state.toast_ui(ui.ctx(), tx);
                    });
                    out.textures_delta.clear();
                    assert!(open);
                }
            }
        }
    }
}
