//! Two-pane file manager shown under the session toolbar: this PC | the remote PC (mirrored in
//! Arabic: the remote PC on the left, this PC on the right). Browse, sort, multi-select,
//! create, rename and delete on either side; copy between the panes with the copy buttons or by
//! dragging; paste files copied in Explorer on the other PC.
//!
//! All file-system work goes through [`Backend::fs_request`] (answers arrive as
//! `BackendEvent::FsResult` and are fed to [`FileBrowser::on_result`]); copies go through
//! [`Backend::fs_transfer`]. Files dragged out to Explorer or dropped in from it: see
//! [`outside`].

mod outside;
mod pane;

use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui::{self, Id, RichText};
use tlp_desk_proto::{FsError, Permissions};

use crate::backend::files::{
    FilesNotice, FsFailure, FsOp, FsReply, FsRequestId, FsSide, RemoteClipboardFiles,
    TransferRequest,
};
use crate::backend::{Backend, SessionId};
use crate::i18n::{Tx, S};
use crate::ui::icons::{self, Icon};
use crate::ui::{theme, widgets};
pub use outside::release_pointer_if_requested;
use pane::{DragFiles, Pane, PaneAction, SortKey};

const LOCAL: usize = 0;
const REMOTE: usize = 1;

/// What a pending request was for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pending {
    /// Listing (or roots) of the pane's current folder.
    List(usize),
    /// A change in the pane's folder; refresh it afterwards.
    Change(usize),
}

/// A dialog of the browser (drawn as a modal).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Dialog {
    NewFolder {
        pane: usize,
        name: String,
    },
    Rename {
        pane: usize,
        old: String,
        name: String,
    },
    ConfirmDelete {
        pane: usize,
        names: Vec<String>,
    },
}

/// State of the file browser of one viewer session.
pub struct FileBrowser {
    session: SessionId,
    /// Shown under the toolbar.
    pub open: bool,
    panes: [Pane; 2],
    pending: HashMap<FsRequestId, Pending>,
    dialog: Option<Dialog>,
    /// Display-ready message under the panes.
    notice: Option<String>,
    /// Files copied in Explorer on the remote PC, ready to paste here.
    pub remote_clipboard: Option<RemoteClipboardFiles>,
    /// Local UTC offset for the "modified" column, seconds.
    offset_s: i32,
    /// Language of the last frame (for messages produced between frames).
    tx: Tx,
    /// The last click landed inside the panel: keys belong to it, not to the remote view.
    keyboard: bool,
    /// Where each pane was drawn in the last frame (window points), to hit-test drops.
    pane_rects: [Option<egui::Rect>; 2],
    /// Remote rows are being dragged and the backend has prepared a drag-out.
    dragging_out: bool,
    /// Where files dragged over the window from another program would land.
    drop_hover: Option<outside::DropTarget>,
    /// Finds the pointer during a drag from another program (the OS; tests use egui's).
    pointer: fn(&egui::Context) -> Option<egui::Pos2>,
}

impl FileBrowser {
    pub fn new(session: SessionId) -> Self {
        Self {
            session,
            open: false,
            panes: [Pane::new(FsSide::Local), Pane::new(FsSide::Remote(session))],
            pending: HashMap::new(),
            dialog: None,
            notice: None,
            remote_clipboard: None,
            offset_s: local_offset_s(),
            tx: Tx::new(crate::i18n::Lang::En, crate::i18n::DigitStyle::Western),
            keyboard: false,
            pane_rects: [None, None],
            dragging_out: false,
            drop_hover: None,
            pointer: outside::os_pointer,
        }
    }

    /// Opens the browser with both panes in a fixed state, without any request: `(folder,
    /// entries, selected name)` for this PC, then for the remote PC. Used by the README media
    /// scenes, which must not touch the disk or the network.
    pub fn show_fixture(
        &mut self,
        local: (&str, Vec<tlp_desk_proto::FsEntry>, &str),
        remote: (&str, Vec<tlp_desk_proto::FsEntry>, &str),
    ) {
        for (pane, (path, entries, selected)) in self.panes.iter_mut().zip([local, remote]) {
            pane.path = Some(path.to_owned());
            pane.entries = entries;
            pane.requested = true;
            pane.loading = None;
            pane.error = None;
            pane.click(selected, false, false);
        }
        self.open = true;
    }

    /// Whether keys belong to the browser (it was clicked last). The remote view must then
    /// neither capture nor forward them until the user clicks it again.
    pub fn owns_keyboard(&self) -> bool {
        self.open && self.keyboard
    }

    /// Whether a browser dialog is open (the remote view must not take input then).
    pub fn modal_open(&self) -> bool {
        self.open && self.dialog.is_some()
    }

    /// Feeds a `BackendEvent::FsResult`. Returns `false` if it was not ours.
    pub fn on_result(
        &mut self,
        side: FsSide,
        request: FsRequestId,
        result: Result<FsReply, FsFailure>,
    ) -> bool {
        let Some(what) = self.pending.remove(&request) else {
            return false;
        };
        match what {
            Pending::List(i) => {
                let Some(p) = self.panes.get_mut(i).filter(|p| p.side == side) else {
                    return true;
                };
                if p.loading != Some(request) {
                    return true; // superseded by a newer listing
                }
                p.loading = None;
                match result {
                    Ok(FsReply::Entries(e)) => {
                        p.selected.retain(|n| e.iter().any(|x| x.name == *n));
                        p.entries = e;
                        p.error = None;
                    }
                    Ok(FsReply::Roots(r)) => {
                        p.roots = r;
                        p.error = None;
                    }
                    Ok(_) => p.error = Some(FsFailure::Other("unexpected answer".into())),
                    Err(e) => p.error = Some(e),
                }
            }
            Pending::Change(i) => {
                if let Err(e) = result {
                    self.notice = Some(failure_text(self.tx, &e));
                }
                if let Some(p) = self.panes.get_mut(i) {
                    p.requested = false; // refresh on the next frame
                }
            }
        }
        true
    }

    /// Feeds a `BackendEvent::FilesNotice`.
    pub fn on_notice(&mut self, tx: Tx, notice: &FilesNotice) {
        self.notice = Some(match notice {
            FilesNotice::TransferStarted { count, .. } => tx.f(
                S::FsTransferStarted,
                &[("n", &tx.digits(&count.to_string()))],
            ),
            FilesNotice::NothingToPaste => tx.t(S::FsNothingToPaste).to_owned(),
            FilesNotice::Failed(f) => failure_text(tx, f),
        });
    }

    /// The panel (call before the remote view's central panel).
    pub fn ui(&mut self, ui: &mut egui::Ui, tx: Tx, backend: &dyn Backend, perms: Permissions) {
        self.tx = tx;
        if !self.open {
            self.pane_rects = [None, None];
            self.drop_hover = None;
            return;
        }
        for i in [LOCAL, REMOTE] {
            self.ensure_listed(i, backend);
        }
        let mut actions: Vec<(usize, PaneAction)> = Vec::new();
        let mut drop_into: Option<(usize, DragFiles)> = None;
        let panel = egui::Panel::bottom("file-browser")
            .resizable(true)
            .default_size(320.0)
            .min_size(180.0)
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().window_fill)
                    .inner_margin(egui::Margin::symmetric(10, 8)),
            )
            .show(ui, |ui| {
                self.title_row(ui, tx, backend);
                if !perms.contains(Permissions::FILES) {
                    ui.label(
                        RichText::new(tx.t(S::FsErrNoPermission))
                            .color(theme::danger(ui.visuals().dark_mode)),
                    );
                }
                if let Some(n) = self.notice.clone() {
                    theme::row(ui, tx.rtl(), |ui| {
                        icons::icon(
                            ui,
                            tx,
                            Icon::Info,
                            icons::BUTTON_ICON,
                            ui.visuals().text_color(),
                        );
                        ui.label(n);
                        if icons::icon_button(ui, tx, Icon::Close, tx.t(S::Ok)).clicked() {
                            self.notice = None;
                        }
                    });
                }
                // Mirrored order in Arabic: the remote PC sits on the reading-end side.
                let order = if tx.rtl() {
                    [REMOTE, LOCAL]
                } else {
                    [LOCAL, REMOTE]
                };
                ui.columns(2, |cols| {
                    for (col, &i) in cols.iter_mut().zip(order.iter()) {
                        let (acts, dropped) = self.pane_ui(col, tx, i);
                        if let Some(r) = self.pane_rects.get_mut(i) {
                            *r = Some(col.min_rect());
                        }
                        actions.extend(acts.into_iter().map(|a| (i, a)));
                        if let Some(d) = dropped {
                            drop_into = Some((i, d));
                        }
                    }
                });
            });
        let rect = panel.response.rect;
        let pressed = ui.input(|i| i.pointer.any_pressed().then(|| i.pointer.interact_pos()));
        if let Some(pos) = pressed.flatten() {
            self.keyboard = rect.contains(pos);
        }
        for (i, a) in actions {
            self.apply(i, a, tx, backend);
        }
        if let Some((to, d)) = drop_into {
            if d.from != to {
                self.copy_between(d.from, d.names, backend);
            }
        }
        self.files_from_outside(ui.ctx(), tx, backend);
        self.drag_out_tick(ui.ctx(), tx, backend);
        self.dialogs(ui.ctx(), tx, backend);
    }

    fn title_row(&mut self, ui: &mut egui::Ui, tx: Tx, backend: &dyn Backend) {
        theme::row(ui, tx.rtl(), |ui| {
            icons::icon(
                ui,
                tx,
                Icon::FolderOpen,
                18.0,
                theme::accent(ui.visuals().dark_mode),
            );
            ui.label(RichText::new(tx.t(S::FsTitle)).strong());
            theme::row_end(ui, tx.rtl(), |ui| {
                if icons::icon_button(ui, tx, Icon::Close, tx.t(S::FsClose)).clicked() {
                    self.open = false;
                }
                let remote_dir = self.panes[REMOTE].path.clone();
                let paste_local = ui.add_enabled(
                    remote_dir.is_some(),
                    icons::icon_text_button(tx, Icon::Upload, tx.t(S::FsPasteLocal)),
                );
                if paste_local.clicked() {
                    if let Some(remote_dir) = remote_dir {
                        backend.fs_transfer(
                            self.session,
                            TransferRequest::PasteLocalClipboard { remote_dir },
                        );
                    }
                }
                if let Some(clip) = &self.remote_clipboard {
                    let hint = tx.f(
                        S::FsPasteRemoteHint,
                        &[("n", &tx.digits(&clip.top_level_count().to_string()))],
                    );
                    let b = widgets::primary_icon_button(
                        ui,
                        tx,
                        Icon::Clipboard,
                        tx.t(S::FsPasteRemote),
                    );
                    if ui.add(b).on_hover_text(hint).clicked() {
                        let local_dir = self.panes[LOCAL].path.clone().map(PathBuf::from);
                        backend.fs_transfer(
                            self.session,
                            TransferRequest::PasteRemoteClipboard { local_dir },
                        );
                    }
                }
            });
        });
    }

    /// One pane; returns its actions and a drop from the other pane, if any.
    fn pane_ui(
        &mut self,
        ui: &mut egui::Ui,
        tx: Tx,
        i: usize,
    ) -> (Vec<PaneAction>, Option<DragFiles>) {
        let mut acts = Vec::new();
        let offset_s = self.offset_s;
        let Some(p) = self.panes.get_mut(i) else {
            return (acts, None);
        };
        let rtl = tx.rtl();
        theme::row(ui, rtl, |ui| {
            let title = if i == LOCAL {
                tx.t(S::FsThisPc).to_owned()
            } else {
                tx.t(S::FsRemotePc).to_owned()
            };
            icons::icon(
                ui,
                tx,
                if i == LOCAL {
                    Icon::Laptop
                } else {
                    Icon::Monitor
                },
                icons::BUTTON_ICON,
                ui.visuals().text_color(),
            );
            let title = ui.label(RichText::new(title).strong());
            if i == REMOTE {
                title.on_hover_text(tx.t(S::FsDragOutHint));
            }
        });
        // Toolbar: Back / Forward / Up / Refresh | New folder / Rename / Delete | Copy across.
        theme::row(ui, rtl, |ui| {
            let sel = p.selection();
            let in_folder = p.path.is_some();
            let tool = |ui: &mut egui::Ui, enabled: bool, icon: Icon, key: S| {
                ui.add_enabled_ui(enabled, |ui| icons::icon_button(ui, tx, icon, tx.t(key)))
                    .inner
                    .clicked()
            };
            if tool(ui, p.can_back(), Icon::Back, S::Back) {
                acts.push(PaneAction::Back);
            }
            if tool(ui, p.can_forward(), Icon::Forward, S::FsForward) {
                acts.push(PaneAction::Forward);
            }
            if tool(ui, in_folder, Icon::ChevronUp, S::FsUp) {
                acts.push(PaneAction::Navigate(
                    p.path.as_deref().and_then(pane::parent),
                ));
            }
            if tool(ui, true, Icon::Refresh, S::FsRefresh) {
                acts.push(PaneAction::Refresh);
            }
            ui.separator();
            if tool(ui, in_folder, Icon::NewFolder, S::FsNewFolder) {
                acts.push(PaneAction::NewFolder);
            }
            if tool(ui, sel.len() == 1, Icon::Rename, S::FsRename) {
                if let Some(n) = sel.first() {
                    acts.push(PaneAction::Rename(n.clone()));
                }
            }
            if tool(ui, !sel.is_empty(), Icon::Trash, S::FsDelete) {
                acts.push(PaneAction::Delete(sel.clone()));
            }
            ui.separator();
            let (icon, key) = if i == LOCAL {
                (Icon::Upload, S::FsUpload)
            } else {
                (Icon::Download, S::FsDownload)
            };
            // Icon only: the pane is narrow, and the tooltip names the direction.
            if tool(ui, !sel.is_empty(), icon, key) {
                acts.push(PaneAction::CopyToOther(sel));
            }
        });
        // Breadcrumbs.
        theme::row(ui, rtl, |ui| {
            if ui
                .add(egui::Button::new(RichText::new(tx.t(S::FsDrives)).small()).frame(false))
                .clicked()
            {
                acts.push(PaneAction::Navigate(None));
            }
            if let Some(path) = &p.path {
                for (label, target) in pane::crumbs(path) {
                    ui.label(theme::caption(ui, if rtl { "‹" } else { "›" }));
                    if ui
                        .add(egui::Button::new(RichText::new(tx.d(&label)).small()).frame(false))
                        .clicked()
                    {
                        acts.push(PaneAction::Navigate(Some(target)));
                    }
                }
            }
        });
        if let Some(e) = &p.error {
            ui.label(
                RichText::new(failure_text(tx, e)).color(theme::danger(ui.visuals().dark_mode)),
            );
        }
        if p.loading.is_some() {
            theme::row(ui, rtl, |ui| {
                ui.spinner();
                ui.label(theme::caption(ui, tx.t(S::FsLoading)));
            });
        }
        let frame = egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .corner_radius(6)
            .inner_margin(4);
        let (_, dropped) = ui.dnd_drop_zone::<DragFiles, _>(frame, |ui| {
            ui.set_min_size(egui::vec2(
                ui.available_width(),
                ui.available_height().max(60.0),
            ));
            if p.path.is_none() {
                egui::ScrollArea::vertical()
                    .id_salt(("fs-roots", i))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for r in &p.roots {
                            if pane::root_row(ui, tx, r) {
                                acts.push(PaneAction::Navigate(Some(r.path.clone())));
                            }
                        }
                    });
                return;
            }
            if let Some(key) = pane::header(ui, tx, p) {
                if p.sort == key {
                    p.ascending = !p.ascending;
                } else {
                    p.sort = key;
                    p.ascending = key == SortKey::Name;
                }
            }
            let rows: Vec<_> = p.sorted().into_iter().cloned().collect();
            if rows.is_empty() && p.loading.is_none() && p.error.is_none() {
                ui.label(theme::caption(ui, tx.t(S::FsEmpty)));
            }
            let modifiers = ui.input(|inp| inp.modifiers);
            egui::ScrollArea::vertical()
                .id_salt(("fs-list", i))
                .auto_shrink([false, false])
                .show_rows(ui, pane::ROW_H, rows.len(), |ui, range| {
                    for e in rows.get(range).unwrap_or_default() {
                        let selected = p.selected.contains(&e.name);
                        let r = pane::entry_row(ui, tx, e, selected, offset_s).response;
                        if r.drag_started() {
                            if !selected {
                                p.click(&e.name, false, false);
                            }
                            r.dnd_set_drag_payload(DragFiles {
                                from: i,
                                names: p.selection(),
                            });
                            if i == REMOTE {
                                acts.push(PaneAction::DragOut(p.selection()));
                            }
                        }
                        if r.double_clicked() && e.is_dir {
                            if let Some(dir) = &p.path {
                                acts.push(PaneAction::Navigate(Some(pane::join(dir, &e.name))));
                            }
                        } else if r.clicked() {
                            p.click(&e.name, modifiers.command, modifiers.shift);
                        }
                    }
                });
        });
        let dropped = dropped.map(|d| (*d).clone());
        (acts, dropped)
    }

    /// Lists the pane's folder (or roots) if that has not been asked for yet.
    fn ensure_listed(&mut self, i: usize, backend: &dyn Backend) {
        let Some(p) = self.panes.get_mut(i) else {
            return;
        };
        if p.requested {
            return;
        }
        p.requested = true;
        let op = match &p.path {
            None => FsOp::Roots,
            Some(path) => FsOp::ListDir { path: path.clone() },
        };
        match backend.fs_request(p.side, op) {
            Some(id) => {
                p.loading = Some(id);
                self.pending.insert(id, Pending::List(i));
            }
            None => p.error = Some(FsFailure::Other(String::new())),
        }
    }

    fn apply(&mut self, i: usize, action: PaneAction, tx: Tx, backend: &dyn Backend) {
        self.tx = tx;
        let Some(p) = self.panes.get_mut(i) else {
            return;
        };
        match action {
            PaneAction::Navigate(path) => p.navigate(path),
            PaneAction::Back => p.go_back(),
            PaneAction::Forward => p.go_forward(),
            PaneAction::Refresh => p.requested = false,
            PaneAction::NewFolder => {
                self.dialog = Some(Dialog::NewFolder {
                    pane: i,
                    name: tx.raw(S::FsNewFolder).to_owned(),
                });
            }
            PaneAction::Rename(old) => {
                self.dialog = Some(Dialog::Rename {
                    pane: i,
                    name: old.clone(),
                    old,
                });
            }
            PaneAction::Delete(names) => {
                self.dialog = Some(Dialog::ConfirmDelete { pane: i, names });
            }
            PaneAction::CopyToOther(names) => self.copy_between(i, names, backend),
            PaneAction::DragOut(names) => self.prepare_drag_out(names, backend),
        }
    }

    /// Copies `names` of pane `from` into the other pane's folder.
    fn copy_between(&mut self, from: usize, names: Vec<String>, backend: &dyn Backend) {
        let to = if from == LOCAL { REMOTE } else { LOCAL };
        let (Some(src), Some(dst)) = (
            self.panes.get(from).and_then(|p| p.path.clone()),
            self.panes.get(to).and_then(|p| p.path.clone()),
        ) else {
            self.notice = Some(self.tx.t(S::FsPickFolder).to_owned());
            return;
        };
        let full: Vec<String> = names.iter().map(|n| pane::join(&src, n)).collect();
        let request = if from == LOCAL {
            TransferRequest::Upload {
                local: full.into_iter().map(PathBuf::from).collect(),
                remote_dir: dst,
            }
        } else {
            TransferRequest::Download {
                remote: full,
                local_dir: PathBuf::from(dst),
            }
        };
        backend.fs_transfer(self.session, request);
    }

    /// Sends a change for pane `i` and refreshes it when answered.
    fn change(&mut self, i: usize, op: FsOp, backend: &dyn Backend) {
        let Some(side) = self.panes.get(i).map(|p| p.side) else {
            return;
        };
        if let Some(id) = backend.fs_request(side, op) {
            self.pending.insert(id, Pending::Change(i));
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context, tx: Tx, backend: &dyn Backend) {
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        let mut keep = true;
        let mut submit = false;
        egui::Modal::new(Id::new(("fs-dialog", self.session.0))).show(ctx, |ui| {
            ui.set_width(400.0);
            theme::column(ui, tx.rtl(), |ui| {
                let ok_enabled = match &mut dialog {
                    Dialog::NewFolder { name, .. } => {
                        widgets::icon_heading(ui, tx, Icon::NewFolder, tx.t(S::FsNewFolder));
                        name_field(ui, tx, name, &mut submit)
                    }
                    Dialog::Rename { old, name, .. } => {
                        let title = tx.f(S::FsRenameTitle, &[("name", old.as_str())]);
                        widgets::icon_heading(ui, tx, Icon::Rename, &title);
                        name_field(ui, tx, name, &mut submit)
                    }
                    Dialog::ConfirmDelete { pane, names } => {
                        let dark = ui.visuals().dark_mode;
                        theme::row(ui, tx.rtl(), |ui| {
                            icons::icon(ui, tx, Icon::Warning, 22.0, theme::danger(dark));
                            ui.label(
                                RichText::new(tx.t(S::FsDeleteTitle))
                                    .heading()
                                    .strong()
                                    .color(theme::danger(dark)),
                            );
                        });
                        let place = if *pane == LOCAL {
                            tx.raw(S::FsThisPc)
                        } else {
                            tx.raw(S::FsRemotePc)
                        };
                        ui.label(tx.f(
                            S::FsDeleteBody,
                            &[
                                ("n", &tx.digits(&names.len().to_string())),
                                ("where", place),
                            ],
                        ));
                        for n in names.iter().take(6) {
                            ui.label(RichText::new(tx.d(n)).strong());
                        }
                        if names.len() > 6 {
                            ui.label("…");
                        }
                        true
                    }
                };
                ui.add_space(8.0);
                theme::row(ui, tx.rtl(), |ui| {
                    let confirm = match dialog {
                        Dialog::NewFolder { .. } => {
                            widgets::primary_icon_button(ui, tx, Icon::Check, tx.t(S::FsCreate))
                        }
                        Dialog::Rename { .. } => {
                            widgets::primary_icon_button(ui, tx, Icon::Check, tx.t(S::FsRename))
                        }
                        Dialog::ConfirmDelete { .. } => widgets::danger_icon_button(
                            ui,
                            tx,
                            Icon::Trash,
                            tx.t(S::FsDeleteConfirm),
                        ),
                    };
                    if ui.add_enabled(ok_enabled, confirm).clicked() {
                        submit = true;
                    }
                    if ui
                        .add(icons::icon_text_button(tx, Icon::Close, tx.t(S::Cancel)))
                        .clicked()
                    {
                        keep = false;
                    }
                });
                if !ok_enabled {
                    submit = false;
                }
            });
        });
        if submit {
            self.submit(dialog, backend);
        } else if keep {
            self.dialog = Some(dialog);
        }
    }

    fn submit(&mut self, dialog: Dialog, backend: &dyn Backend) {
        let pane = match &dialog {
            Dialog::NewFolder { pane, .. }
            | Dialog::Rename { pane, .. }
            | Dialog::ConfirmDelete { pane, .. } => *pane,
        };
        let Some(dir) = self.panes.get(pane).and_then(|p| p.path.clone()) else {
            return;
        };
        match dialog {
            Dialog::NewFolder { name, .. } => {
                let path = pane::join(&dir, name.trim());
                self.change(pane, FsOp::MakeDir { path }, backend);
            }
            Dialog::Rename { old, name, .. } => {
                let name = name.trim();
                if name != old {
                    let op = FsOp::Rename {
                        from: pane::join(&dir, &old),
                        to: pane::join(&dir, name),
                    };
                    self.change(pane, op, backend);
                }
            }
            Dialog::ConfirmDelete { names, .. } => {
                for n in names {
                    let op = FsOp::Delete {
                        path: pane::join(&dir, &n),
                        recursive: true,
                    };
                    self.change(pane, op, backend);
                }
            }
        }
    }
}

/// Name input of the new-folder / rename dialogs. Returns whether the name is valid; sets
/// `submit` on Enter.
fn name_field(ui: &mut egui::Ui, tx: Tx, name: &mut String, submit: &mut bool) -> bool {
    let resp = widgets::text_field(ui, tx, "fs-dialog-name", name, "", 360.0);
    if !resp.has_focus() && !resp.lost_focus() {
        resp.request_focus();
    }
    let trimmed = name.trim();
    let valid = tlp_desk_files::fs::path::check_name(trimmed).is_ok();
    if !trimmed.is_empty() && !valid {
        ui.label(
            RichText::new(tx.t(S::FsInvalidName)).color(theme::danger(ui.visuals().dark_mode)),
        );
    }
    if valid && resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        *submit = true;
    }
    valid
}

/// Display-ready text for a failure.
pub fn failure_text(tx: Tx, f: &FsFailure) -> String {
    let key = match f {
        FsFailure::Fs(FsError::NotFound) => S::FsErrNotFound,
        FsFailure::Fs(FsError::PermissionDenied) => S::FsErrDenied,
        FsFailure::Fs(FsError::AlreadyExists) => S::FsErrExists,
        FsFailure::Fs(FsError::NotEmpty) => S::FsErrNotEmpty,
        FsFailure::Fs(FsError::InvalidPath) => S::FsErrInvalidPath,
        FsFailure::Fs(FsError::NoSpace) => S::FsErrNoSpace,
        FsFailure::Fs(FsError::Busy) => S::FsErrBusy,
        FsFailure::NoPermission => S::FsErrNoPermission,
        FsFailure::Timeout => S::FsErrTimeout,
        FsFailure::Disconnected => S::SessionEnded,
        FsFailure::TransfersUnavailable => S::FsErrTransfers,
        FsFailure::Fs(FsError::Other(d)) | FsFailure::Other(d) => {
            return tx.f(S::FsErrOther, &[("detail", d)]);
        }
        FsFailure::Fs(other) => {
            return tx.f(S::FsErrOther, &[("detail", &format!("{other:?}"))]);
        }
    };
    tx.t(key).to_owned()
}

/// The local UTC offset in seconds (0 if the OS does not say).
fn local_offset_s() -> i32 {
    time::UtcOffset::current_local_offset().map_or(0, time::UtcOffset::whole_seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::files::{FsEntry, FsRoot, FsRootKind};
    use crate::backend::{AttemptId, ConnectTarget, DeviceId, LocalInfo, RequestId};
    use crate::i18n::{DigitStyle, Lang};
    use eframe::egui::{Pos2, Rect};
    use std::sync::Mutex;
    use tlp_desk_proto::InputEvent;

    /// Records file requests and transfers; answers nothing by itself.
    #[derive(Default)]
    struct Rec {
        ops: Mutex<Vec<(FsSide, FsOp, FsRequestId)>>,
        transfers: Mutex<Vec<TransferRequest>>,
    }

    impl Backend for Rec {
        fn local_info(&self) -> LocalInfo {
            LocalInfo {
                device_id: DeviceId(1),
                device_name: String::new(),
                addresses: vec![],
                one_time_code: String::new(),
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
        fn fs_request(&self, side: FsSide, op: FsOp) -> Option<FsRequestId> {
            let mut ops = self.ops.lock().unwrap();
            let id = FsRequestId(ops.len() as u64 + 1);
            ops.push((side, op, id));
            Some(id)
        }
        fn fs_transfer(&self, _: SessionId, request: TransferRequest) {
            self.transfers.lock().unwrap().push(request);
        }
    }

    fn frame(ctx: &egui::Context, fb: &mut FileBrowser, tx: Tx, backend: &Rec) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1100.0, 700.0))),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            fb.ui(ui, tx, backend, Permissions::ALL);
        });
        out.textures_delta.clear();
    }

    fn entry(name: &str, is_dir: bool) -> FsEntry {
        FsEntry {
            name: name.into(),
            is_dir,
            size: 1234,
            modified_unix_ms: 1_700_000_000_000,
            readonly: false,
            hidden: false,
        }
    }

    fn last_id(b: &Rec) -> FsRequestId {
        b.ops.lock().unwrap().last().unwrap().2
    }

    #[test]
    fn lists_both_sides_navigates_and_renders_in_both_languages() {
        for lang in [Lang::En, Lang::Ar] {
            let ctx = egui::Context::default();
            crate::ui::icons::install(&ctx);
            let tx = Tx::new(lang, DigitStyle::Western);
            let b = Rec::default();
            let mut fb = FileBrowser::new(SessionId(5));
            fb.open = true;
            frame(&ctx, &mut fb, tx, &b);
            {
                let ops = b.ops.lock().unwrap();
                assert_eq!(ops.len(), 2);
                assert!(ops.iter().all(|(_, op, _)| *op == FsOp::Roots));
                assert_eq!(ops[1].0, FsSide::Remote(SessionId(5)));
            }
            let roots = vec![FsRoot {
                path: "C:\\".into(),
                label: "النظام".into(),
                kind: FsRootKind::Drive,
                total_bytes: Some(500 << 30),
                free_bytes: Some(100 << 30),
            }];
            assert!(fb.on_result(
                FsSide::Remote(SessionId(5)),
                FsRequestId(2),
                Ok(FsReply::Roots(roots))
            ));
            frame(&ctx, &mut fb, tx, &b);
            fb.apply(REMOTE, PaneAction::Navigate(Some("C:\\".into())), tx, &b);
            frame(&ctx, &mut fb, tx, &b);
            let (side, op, id) = b.ops.lock().unwrap().last().cloned().unwrap();
            assert_eq!(side, FsSide::Remote(SessionId(5)));
            assert_eq!(
                op,
                FsOp::ListDir {
                    path: "C:\\".into()
                }
            );
            fb.on_result(
                side,
                id,
                Ok(FsReply::Entries(vec![
                    entry("ملف تجريبي.txt", false),
                    entry("مجلد", true),
                ])),
            );
            frame(&ctx, &mut fb, tx, &b);
            assert_eq!(fb.panes[REMOTE].entries.len(), 2);
            // A stale answer for an unknown request is ignored.
            assert!(!fb.on_result(side, FsRequestId(999), Ok(FsReply::Done)));
        }
    }

    #[test]
    fn delete_rename_and_new_folder_go_through_dialogs() {
        let tx = Tx::new(Lang::Ar, DigitStyle::Western);
        let b = Rec::default();
        let mut fb = FileBrowser::new(SessionId(1));
        fb.open = true;
        fb.panes[LOCAL].path = Some("C:\\بيانات".into());
        fb.panes[LOCAL].requested = true;
        fb.panes[REMOTE].requested = true;

        fb.apply(
            LOCAL,
            PaneAction::Delete(vec!["أ.txt".into(), "ب".into()]),
            tx,
            &b,
        );
        assert!(matches!(fb.dialog, Some(Dialog::ConfirmDelete { .. })));
        assert!(
            b.ops.lock().unwrap().is_empty(),
            "nothing deleted before confirming"
        );
        let d = fb.dialog.take().unwrap();
        fb.submit(d, &b);
        let ops: Vec<FsOp> = b.ops.lock().unwrap().iter().map(|o| o.1.clone()).collect();
        assert_eq!(
            ops,
            [
                FsOp::Delete {
                    path: "C:\\بيانات\\أ.txt".into(),
                    recursive: true
                },
                FsOp::Delete {
                    path: "C:\\بيانات\\ب".into(),
                    recursive: true
                },
            ]
        );
        // The answer refreshes the pane.
        fb.on_result(FsSide::Local, last_id(&b), Ok(FsReply::Done));
        assert!(!fb.panes[LOCAL].requested);

        fb.submit(
            Dialog::Rename {
                pane: LOCAL,
                old: "أ.txt".into(),
                name: " الجديد.txt ".into(),
            },
            &b,
        );
        fb.submit(
            Dialog::NewFolder {
                pane: LOCAL,
                name: "مجلد جديد".into(),
            },
            &b,
        );
        let ops: Vec<FsOp> = b.ops.lock().unwrap().iter().map(|o| o.1.clone()).collect();
        assert_eq!(
            ops[2],
            FsOp::Rename {
                from: "C:\\بيانات\\أ.txt".into(),
                to: "C:\\بيانات\\الجديد.txt".into()
            }
        );
        assert_eq!(
            ops[3],
            FsOp::MakeDir {
                path: "C:\\بيانات\\مجلد جديد".into()
            }
        );
        // A failed change is reported.
        fb.on_result(
            FsSide::Local,
            last_id(&b),
            Err(FsFailure::Fs(FsError::AlreadyExists)),
        );
        assert_eq!(fb.notice.as_deref(), Some(tx.t(S::FsErrExists)));
    }

    #[test]
    fn copying_between_panes_asks_for_a_transfer() {
        let tx = Tx::new(Lang::En, DigitStyle::Western);
        let b = Rec::default();
        let mut fb = FileBrowser::new(SessionId(1));
        fb.apply(LOCAL, PaneAction::CopyToOther(vec!["a.txt".into()]), tx, &b);
        assert!(b.transfers.lock().unwrap().is_empty());
        assert_eq!(fb.notice.as_deref(), Some(tx.t(S::FsPickFolder)));

        fb.panes[LOCAL].path = Some("C:\\src".into());
        fb.panes[REMOTE].path = Some("D:\\وجهة".into());
        fb.apply(LOCAL, PaneAction::CopyToOther(vec!["a.txt".into()]), tx, &b);
        fb.apply(
            REMOTE,
            PaneAction::CopyToOther(vec!["ملف.bin".into()]),
            tx,
            &b,
        );
        let t = b.transfers.lock().unwrap().clone();
        assert_eq!(
            t,
            [
                TransferRequest::Upload {
                    local: vec![PathBuf::from("C:\\src\\a.txt")],
                    remote_dir: "D:\\وجهة".into()
                },
                TransferRequest::Download {
                    remote: vec!["D:\\وجهة\\ملف.bin".into()],
                    local_dir: PathBuf::from("C:\\src")
                },
            ]
        );
    }

    #[test]
    fn every_failure_has_text() {
        let tx = Tx::new(Lang::Ar, DigitStyle::Western);
        for f in [
            FsFailure::Fs(FsError::NotFound),
            FsFailure::Fs(FsError::Other("x".into())),
            FsFailure::Fs(FsError::Cancelled),
            FsFailure::NoPermission,
            FsFailure::Timeout,
            FsFailure::Disconnected,
            FsFailure::TransfersUnavailable,
            FsFailure::Other("تفاصيل".into()),
        ] {
            assert!(!failure_text(tx, &f).trim().is_empty(), "{f:?}");
        }
        let mut fb = FileBrowser::new(SessionId(1));
        fb.on_notice(
            tx,
            &FilesNotice::TransferStarted {
                count: 3,
                upload: true,
            },
        );
        assert!(fb.notice.as_deref().unwrap_or_default().contains('3'));
    }
}
