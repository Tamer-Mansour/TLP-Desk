//! File transfer queue of a viewer session: send files or a folder (picker or drag and drop),
//! progress with speed and time left, cancel, retry, open the destination folder.
//!
//! The panel holds no engine state: [`TransferPanel::update`] receives
//! [`BackendEvent::Transfer`](crate::backend::BackendEvent::Transfer) views and
//! [`TransferPanel::ui`] returns the [`Action`]s the session applies to the backend.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};

use eframe::egui::{self, Color32, CornerRadius, RichText, Sense};

use super::icons::{self, Icon};
use super::theme;
use crate::backend::files::{
    FsError, TransferDir, TransferId, TransferStartError, TransferStatus, TransferView,
};
use crate::i18n::{format, ltr_isolate, Tx, S};

/// What the user asked for in the panel; the session forwards these to the backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Send these local files and folders to the host.
    Send(Vec<PathBuf>),
    /// Cancel a running transfer.
    Cancel(TransferId),
    /// Continue a stopped transfer from its partial data.
    Retry(TransferId),
    /// Show this local folder in the file manager.
    OpenFolder(PathBuf),
}

/// Most transfers kept in the list (oldest finished ones are dropped first).
const MAX_ITEMS: usize = 200;

/// State of the transfer queue panel.
#[derive(Default)]
pub struct TransferPanel {
    /// Whether the panel is shown.
    pub open: bool,
    items: Vec<TransferView>,
    error: Option<TransferStartError>,
    /// Result of the native picker running on its own thread.
    picker: Option<Receiver<Vec<PathBuf>>>,
}

impl TransferPanel {
    /// An empty, closed panel.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the newest state of a transfer and opens the panel for new ones.
    pub fn update(&mut self, view: TransferView) {
        self.error = None;
        match self.items.iter_mut().find(|v| v.id == view.id) {
            Some(slot) => *slot = view,
            None => {
                self.items.push(view);
                self.open = true;
                if self.items.len() > MAX_ITEMS {
                    if let Some(i) = self.items.iter().position(|v| v.status.is_final()) {
                        self.items.remove(i);
                    }
                }
            }
        }
    }

    /// A transfer could not start (shown as a line in the panel).
    pub fn not_started(&mut self, error: TransferStartError) {
        self.error = Some(error);
        self.open = true;
    }

    /// The transfers currently listed, oldest first.
    pub fn items(&self) -> &[TransferView] {
        &self.items
    }

    fn clear_finished(&mut self) {
        self.items.retain(|v| !v.status.is_final());
        self.error = None;
    }

    /// Local paths dropped onto the window this frame.
    pub fn take_dropped(ctx: &egui::Context) -> Vec<PathBuf> {
        ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        })
    }

    /// Dims the window and says where dropped files go while a drag is over it.
    pub fn drop_overlay(ctx: &egui::Context, tx: Tx, peer_name: &str, allowed: bool) {
        if ctx.input(|i| i.raw.hovered_files.is_empty()) {
            return;
        }
        let rect = ctx.content_rect();
        let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop-overlay"));
        let painter = ctx.layer_painter(layer);
        painter.rect_filled(rect, 0.0, Color32::from_black_alpha(170));
        let text = if allowed {
            tx.f(S::DropToSend, &[("name", peer_name)])
        } else {
            tx.t(S::FilesNotAllowed).to_owned()
        };
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            tx.d(&text),
            egui::FontId::proportional(22.0),
            Color32::WHITE,
        );
    }

    /// Opens the native picker on a helper thread (it must not block the UI).
    fn pick(&mut self, ctx: &egui::Context, folder: bool) {
        if self.picker.is_some() {
            return;
        }
        let (send, recv) = channel();
        self.picker = Some(recv);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let picked = if folder {
                rfd::FileDialog::new().pick_folder().map(|p| vec![p])
            } else {
                rfd::FileDialog::new().pick_files()
            };
            // The receiver is gone when the session ended; nothing to do then.
            let _ = send.send(picked.unwrap_or_default());
            ctx.request_repaint();
        });
    }

    /// Draws the panel (if open) and returns what the user asked for. `allowed` is whether the
    /// session has the files permission.
    pub fn ui(&mut self, ui: &mut egui::Ui, tx: Tx, allowed: bool) -> Vec<Action> {
        let mut actions = Vec::new();
        if let Some(rx) = &self.picker {
            match rx.try_recv() {
                Ok(paths) => {
                    self.picker = None;
                    if !paths.is_empty() {
                        actions.push(Action::Send(paths));
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.picker = None,
            }
        }
        if !self.open {
            return actions;
        }
        let ctx = ui.ctx().clone();
        egui::Panel::bottom("session-transfers")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().window_fill)
                    .inner_margin(egui::Margin::symmetric(14, 8)),
            )
            .show(ui, |ui| {
                theme::row(ui, tx.rtl(), |ui| {
                    ui.label(RichText::new(tx.t(S::FilesPanelTitle)).strong());
                    ui.add_enabled_ui(allowed && self.picker.is_none(), |ui| {
                        if ui
                            .add(icons::icon_text_button(
                                tx,
                                Icon::Upload,
                                tx.t(S::SendFiles),
                            ))
                            .clicked()
                        {
                            self.pick(&ctx, false);
                        }
                        if ui
                            .add(icons::icon_text_button(
                                tx,
                                Icon::NewFolder,
                                tx.t(S::SendFolder),
                            ))
                            .clicked()
                        {
                            self.pick(&ctx, true);
                        }
                    });
                    theme::row_end(ui, tx.rtl(), |ui| {
                        if icons::icon_button(ui, tx, Icon::Close, tx.t(S::CloseTransfers))
                            .clicked()
                        {
                            self.open = false;
                        }
                        if self.items.iter().any(|v| v.status.is_final())
                            && ui.button(tx.t(S::ClearFinished)).clicked()
                        {
                            self.clear_finished();
                        }
                    });
                });
                if !allowed {
                    ui.label(
                        RichText::new(tx.t(S::FilesNotAllowed))
                            .color(theme::danger(ui.visuals().dark_mode)),
                    );
                }
                if let Some(e) = &self.error {
                    let text = match e {
                        TransferStartError::NoPermission => tx.t(S::FilesNotAllowed).to_owned(),
                        TransferStartError::Other(d) => tx.d(d),
                    };
                    ui.label(RichText::new(text).color(theme::danger(ui.visuals().dark_mode)));
                }
                if self.items.is_empty() {
                    ui.label(theme::caption(ui, tx.t(S::NoTransfers)));
                    return;
                }
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for v in self.items.iter().rev() {
                            transfer_row(ui, tx, v, &mut actions);
                        }
                    });
            });
        actions
    }
}

fn transfer_row(ui: &mut egui::Ui, tx: Tx, v: &TransferView, actions: &mut Vec<Action>) {
    ui.add_space(4.0);
    theme::row(ui, tx.rtl(), |ui| {
        let dir_icon = match v.dir {
            TransferDir::Send => Icon::Upload,
            TransferDir::Receive => Icon::Download,
        };
        icons::icon(
            ui,
            tx,
            dir_icon,
            icons::BUTTON_ICON,
            ui.visuals().text_color(),
        );
        theme::column(ui, tx.rtl(), |ui| {
            ui.label(RichText::new(tx.d(&v.label)).strong());
            ui.label(theme::caption(ui, &status_line(v, tx)));
        });
        theme::row_end(ui, tx.rtl(), |ui| {
            if !v.status.is_final() {
                if icons::icon_button(ui, tx, Icon::Stop, tx.t(S::TransferCancel)).clicked() {
                    actions.push(Action::Cancel(v.id));
                }
            } else if v.status.can_retry()
                && icons::icon_button(ui, tx, Icon::Refresh, tx.t(S::TransferRetry)).clicked()
            {
                actions.push(Action::Retry(v.id));
            }
            if let (TransferDir::Receive, Some(dest)) = (v.dir, &v.local_dest) {
                if v.status != TransferStatus::Preparing
                    && icons::icon_button(ui, tx, Icon::FolderOpen, tx.t(S::OpenFolder)).clicked()
                {
                    actions.push(Action::OpenFolder(dest.clone()));
                }
            }
        });
    });
    progress_bar(ui, tx.rtl(), v);
}

/// A thin progress bar that fills from the reading-start side (right in Arabic).
fn progress_bar(ui: &mut egui::Ui, rtl: bool, v: &TransferView) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 6.0), Sense::hover());
    let dark = ui.visuals().dark_mode;
    let round = CornerRadius::same(3);
    ui.painter()
        .rect_filled(rect, round, ui.visuals().extreme_bg_color);
    let w = rect.width() * v.fraction();
    if w > 0.5 {
        let filled = if rtl {
            egui::Rect::from_min_max(egui::pos2(rect.max.x - w, rect.min.y), rect.max)
        } else {
            egui::Rect::from_min_max(rect.min, egui::pos2(rect.min.x + w, rect.max.y))
        };
        let color = match v.status {
            TransferStatus::Completed if v.failed_files == 0 => theme::success(dark),
            TransferStatus::Failed(_) | TransferStatus::Disconnected => theme::danger(dark),
            _ => theme::accent(dark),
        };
        ui.painter().rect_filled(filled, round, color);
    }
}

/// Text under the transfer name: state, sizes, speed, time left, file count.
pub fn status_line(v: &TransferView, tx: Tx) -> String {
    let d = tx.digits;
    let size = |n: u64| ltr_isolate(&format::file_size(n, tx.lang, d));
    let mut parts: Vec<String> = Vec::new();
    match &v.status {
        TransferStatus::Preparing => parts.push(tx.raw(S::TransferPreparing).to_owned()),
        TransferStatus::Running => {
            parts.push(format!("{} / {}", size(v.bytes_done), size(v.total_bytes)));
            if v.bytes_per_sec > 0 {
                parts.push(tx.f(S::TransferRate, &[("size", &size(v.bytes_per_sec))]));
            }
            if let Some(eta) = v.eta_secs() {
                let time = ltr_isolate(&format::duration(std::time::Duration::from_secs(eta), d));
                parts.push(tx.f(S::TransferEta, &[("time", &time)]));
            }
        }
        TransferStatus::Paused => parts.push(tx.raw(S::TransferPaused).to_owned()),
        TransferStatus::Completed => {
            parts.push(if v.failed_files == 0 {
                tx.raw(S::TransferDone).to_owned()
            } else {
                tx.f(
                    S::TransferDoneFailures,
                    &[("n", &tx.digits(&v.failed_files.to_string()))],
                )
            });
            parts.push(size(v.total_bytes));
        }
        TransferStatus::Cancelled => parts.push(tx.raw(S::TransferCancelled).to_owned()),
        TransferStatus::Disconnected => parts.push(tx.raw(S::TransferLost).to_owned()),
        TransferStatus::Failed(e) => {
            let reason = fail_reason(e, tx);
            parts.push(tx.f(S::TransferFailed, &[("reason", &reason)]));
        }
    }
    if v.total_files > 1 {
        parts.push(tx.f(
            S::TransferFiles,
            &[
                ("done", &tx.digits(&v.files_done.to_string())),
                ("total", &tx.digits(&v.total_files.to_string())),
            ],
        ));
    }
    tx.p(&parts.join("  ·  "))
}

fn fail_reason(e: &FsError, tx: Tx) -> String {
    match e {
        FsError::NoSpace => tx.raw(S::FailNoSpace).to_owned(),
        FsError::Declined => tx.raw(S::FailDeclined).to_owned(),
        FsError::HashMismatch => tx.raw(S::FailHash).to_owned(),
        FsError::InvalidPath => tx.raw(S::FailPath).to_owned(),
        FsError::PermissionDenied => tx.raw(S::FailPermission).to_owned(),
        FsError::NotFound => tx.raw(S::FailNotFound).to_owned(),
        FsError::Busy => tx.raw(S::FailBusy).to_owned(),
        FsError::Other(d) => d.clone(),
        other => format!("{other:?}"),
    }
}

/// Shows `path` in the file manager. Best effort: failures are only logged.
pub fn open_folder(path: &std::path::Path) {
    #[cfg(windows)]
    let cmd = "explorer";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let cmd = "xdg-open";
    if let Err(e) = std::process::Command::new(cmd).arg(path).spawn() {
        tracing::warn!(%e, "could not open the folder");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{DigitStyle, Lang};

    fn view(status: TransferStatus) -> TransferView {
        TransferView {
            id: TransferId(1),
            dir: TransferDir::Send,
            label: "مجلد المشروع".into(),
            status,
            bytes_done: 1 << 30,
            total_bytes: 2 << 30,
            files_done: 3,
            total_files: 10,
            failed_files: 0,
            bytes_per_sec: 100 << 20,
            local_dest: None,
        }
    }

    #[test]
    fn running_line_has_sizes_speed_eta_and_count() {
        let tx = Tx::new(Lang::En, DigitStyle::Western);
        let line = status_line(&view(TransferStatus::Running), tx);
        for needle in ["1.0 GB", "2.0 GB", "100.0 MB", "0:10", "3 of 10 files"] {
            assert!(line.contains(needle), "{needle} not in {line}");
        }
    }

    #[test]
    fn arabic_line_renders_and_uses_arabic_indic_digits() {
        let tx = Tx::new(Lang::Ar, DigitStyle::ArabicIndic);
        let line = status_line(&view(TransferStatus::Running), tx);
        assert!(line.contains('٣') || line.contains('٠'), "{line}");
        let failed = status_line(&view(TransferStatus::Failed(FsError::NoSpace)), tx);
        assert!(!failed.is_empty());
    }

    #[test]
    fn update_replaces_by_id_and_opens_for_new_transfers() {
        let mut p = TransferPanel::new();
        assert!(!p.open);
        p.update(view(TransferStatus::Preparing));
        assert!(p.open);
        p.update(view(TransferStatus::Running));
        assert_eq!(p.items().len(), 1);
        assert_eq!(p.items()[0].status, TransferStatus::Running);
        p.clear_finished();
        assert_eq!(p.items().len(), 1, "running transfers stay");
        p.update(view(TransferStatus::Completed));
        p.clear_finished();
        assert!(p.items().is_empty());
    }

    #[test]
    fn eta_and_fraction() {
        let v = view(TransferStatus::Running);
        assert_eq!(v.eta_secs(), Some(10));
        assert!((v.fraction() - 0.5).abs() < 1e-6);
        let mut idle = v.clone();
        idle.bytes_per_sec = 0;
        assert_eq!(idle.eta_secs(), None);
    }
}
