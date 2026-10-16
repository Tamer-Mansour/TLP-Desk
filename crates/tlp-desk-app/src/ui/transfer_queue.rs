//! The transfer queue: every transfer with its state, speed and time left, controls to pause,
//! resume, cancel, retry and open the result, a per-file list, sorting, the total throughput,
//! and the list of recent transfers with "send again".
//!
//! [`Queue`] holds no engine state: callers feed it [`TransferView`]s and apply the
//! [`QueueAction`]s [`Queue::ui`] returns. The same widget serves the viewer's session panel
//! and the host's "File transfers" window (where each row also says who the other side is).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{self, CornerRadius, RichText, Sense};

use super::icons::{self, Icon};
use super::theme;
use super::transfers::status_line;
use crate::backend::files::{
    FileRow, FileState, TransferDir, TransferId, TransferStatus, TransferView,
};
use crate::backend::transfer_history::{now_unix_ms, Outcome, TransferRecord};
use crate::backend::Backend;
use crate::i18n::{format, ltr_isolate, Tx, S};

/// Most transfers kept in the list (oldest finished ones are dropped first).
pub const MAX_ITEMS: usize = 200;
/// Files listed under an opened transfer.
pub const FILE_PAGE: usize = 100;
/// How often the file list of a running transfer is refreshed.
const REFRESH_RUNNING: Duration = Duration::from_millis(400);
/// How often the file list of a stopped transfer is refreshed.
const REFRESH_IDLE: Duration = Duration::from_secs(3);

/// How the list is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    /// Running first, then the most recently started.
    #[default]
    Status,
    /// Most recently started first.
    Newest,
    /// By name.
    Name,
    /// Largest first.
    Size,
    /// Least complete first.
    Progress,
}

impl Sort {
    const ALL: [Sort; 5] = [
        Sort::Status,
        Sort::Newest,
        Sort::Name,
        Sort::Size,
        Sort::Progress,
    ];

    fn label(self) -> S {
        match self {
            Sort::Status => S::TqSortStatus,
            Sort::Newest => S::TqSortNewest,
            Sort::Name => S::TqSortName,
            Sort::Size => S::TqSortSize,
            Sort::Progress => S::TqSortProgress,
        }
    }
}

/// One transfer in the list, with who is on the other side (empty on a viewer, where it is
/// always the host of the session).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The newest state.
    pub view: TransferView,
    /// The other device's display name.
    pub who: String,
}

/// What the user asked for in the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueAction {
    /// Cancel a running transfer.
    Cancel(TransferId),
    /// Pause a running transfer.
    Pause(TransferId),
    /// Continue a paused, cancelled or failed transfer from its partial data.
    Resume(TransferId),
    /// Show this folder in the file manager.
    OpenFolder(PathBuf),
    /// Open this received file with its program.
    OpenFile(PathBuf),
    /// Show this file selected in its folder.
    ShowInFolder(PathBuf),
    /// Run an earlier transfer again.
    Repeat(TransferRecord),
    /// Forget the recent transfers.
    ClearHistory,
}

/// What the queue needs to know about its surroundings.
pub struct QueueEnv<'a> {
    /// Source of the per-file rows and the history.
    pub backend: &'a dyn Backend,
    /// The device ID of the peer of this viewer session (`None` on the host): "send again" is
    /// only offered for transfers with that peer.
    pub session_peer: Option<u32>,
    /// Its name, for the hint on records that cannot be repeated now.
    pub session_peer_name: &'a str,
    /// Whether this device may start transfers right now (the host granted `FILES`).
    pub can_start: bool,
}

/// State of the queue widget.
#[derive(Default)]
pub struct Queue {
    entries: Vec<Entry>,
    /// Current ordering.
    pub sort: Sort,
    /// Showing the recent list instead of the live transfers.
    pub show_recent: bool,
    expanded: HashSet<TransferId>,
    rows: HashMap<TransferId, (Vec<FileRow>, Instant)>,
    history: Option<(Vec<TransferRecord>, Instant)>,
}

impl Queue {
    /// Records the newest state of a transfer. `true` if it is new to the list.
    pub fn update(&mut self, view: TransferView, who: &str) -> bool {
        match self.entries.iter_mut().find(|e| e.view.id == view.id) {
            Some(slot) => {
                slot.view = view;
                if slot.who.is_empty() {
                    slot.who = who.to_owned();
                }
                false
            }
            None => {
                self.entries.push(Entry {
                    view,
                    who: who.to_owned(),
                });
                if self.entries.len() > MAX_ITEMS {
                    if let Some(i) = self.entries.iter().position(|e| finished(&e.view)) {
                        let gone = self.entries.remove(i);
                        self.expanded.remove(&gone.view.id);
                        self.rows.remove(&gone.view.id);
                    }
                }
                true
            }
        }
    }

    /// The transfers, in arrival order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Removes finished transfers from the list (they stay in the history).
    pub fn clear_finished(&mut self) {
        self.entries.retain(|e| !finished(&e.view));
        let live: HashSet<_> = self.entries.iter().map(|e| e.view.id).collect();
        self.expanded.retain(|i| live.contains(i));
        self.rows.retain(|i, _| live.contains(i));
        self.history = None;
    }

    /// Transfers that are not finished.
    pub fn active_count(&self) -> usize {
        self.entries.iter().filter(|e| !finished(&e.view)).count()
    }

    /// Combined speed of the running transfers, bytes per second.
    pub fn total_rate(&self) -> u64 {
        self.entries
            .iter()
            .filter(|e| e.view.status == TransferStatus::Running)
            .map(|e| e.view.bytes_per_sec)
            .sum()
    }

    /// Progress of everything not finished, 0.0 to 1.0.
    pub fn active_fraction(&self) -> f32 {
        let (done, total) =
            self.entries
                .iter()
                .filter(|e| !finished(&e.view))
                .fold((0u64, 0u64), |(d, t), e| {
                    (
                        d.saturating_add(e.view.bytes_done),
                        t.saturating_add(e.view.total_bytes),
                    )
                });
        if total == 0 {
            0.0
        } else {
            (done as f64 / total as f64).clamp(0.0, 1.0) as f32
        }
    }

    /// Indexes into [`Queue::entries`] in display order.
    pub fn order(&self) -> Vec<usize> {
        sort_order(&self.entries, self.sort)
    }

    /// Draws the queue; returns what the user asked for.
    pub fn ui(&mut self, ui: &mut egui::Ui, tx: Tx, env: &QueueEnv<'_>) -> Vec<QueueAction> {
        let mut actions = Vec::new();
        self.header(ui, tx, &mut actions);
        if self.show_recent {
            self.history_ui(ui, tx, env, &mut actions);
        } else {
            self.live_ui(ui, tx, env, &mut actions);
        }
        actions
    }

    fn header(&mut self, ui: &mut egui::Ui, tx: Tx, actions: &mut Vec<QueueAction>) {
        self.header_row(ui, tx, actions);
        if !self.show_recent && self.active_count() > 0 {
            let rate = ltr_isolate(&format::file_size(self.total_rate(), tx.lang, tx.digits));
            let speed = tx.raw(S::TransferRate).replace("{size}", &rate);
            let text = tx.raw(S::TqTotalSpeed).replace("{speed}", &speed);
            ui.label(theme::caption(ui, &tx.p(&text)));
        }
    }

    fn header_row(&mut self, ui: &mut egui::Ui, tx: Tx, actions: &mut Vec<QueueAction>) {
        theme::row(ui, tx.rtl(), |ui| {
            for (recent, key) in [(false, S::TqActive), (true, S::TqRecent)] {
                if ui
                    .selectable_label(self.show_recent == recent, tx.t(key))
                    .clicked()
                {
                    self.show_recent = recent;
                }
            }
            theme::row_end(ui, tx.rtl(), |ui| {
                if self.show_recent {
                    if ui.button(tx.t(S::TqClearHistory)).clicked() {
                        actions.push(QueueAction::ClearHistory);
                        self.history = None;
                    }
                } else {
                    if self.entries.iter().any(|e| finished(&e.view))
                        && ui.button(tx.t(S::ClearFinished)).clicked()
                    {
                        self.clear_finished();
                    }
                    egui::ComboBox::from_id_salt("tq-sort")
                        .selected_text(tx.t(self.sort.label()))
                        .width(110.0)
                        .show_ui(ui, |ui| {
                            for s in Sort::ALL {
                                ui.selectable_value(&mut self.sort, s, tx.t(s.label()));
                            }
                        })
                        .response
                        .on_hover_text(tx.t(S::TqSortLabel));
                }
            });
        });
    }

    fn live_ui(
        &mut self,
        ui: &mut egui::Ui,
        tx: Tx,
        env: &QueueEnv<'_>,
        actions: &mut Vec<QueueAction>,
    ) {
        if self.entries.is_empty() {
            ui.label(theme::caption(ui, tx.t(S::NoTransfers)));
            return;
        }
        let order = self.order();
        let now = Instant::now();
        for i in order {
            let Some(entry) = self.entries.get(i).cloned() else {
                continue;
            };
            let id = entry.view.id;
            let open = self.expanded.contains(&id);
            if open {
                self.refresh_rows(ui.ctx(), env.backend, &entry.view, now);
            }
            let mut toggle = false;
            transfer_row(ui, tx, &entry, open, &mut toggle, actions);
            if open {
                if let Some((rows, _)) = self.rows.get(&id) {
                    file_list(ui, tx, rows, entry.view.total_files);
                }
            }
            if toggle {
                if open {
                    self.expanded.remove(&id);
                } else {
                    self.expanded.insert(id);
                    self.rows.remove(&id);
                }
            }
        }
    }

    fn refresh_rows(
        &mut self,
        ctx: &egui::Context,
        backend: &dyn Backend,
        view: &TransferView,
        now: Instant,
    ) {
        let every = if view.status.is_final() {
            REFRESH_IDLE
        } else {
            REFRESH_RUNNING
        };
        let stale = self
            .rows
            .get(&view.id)
            .is_none_or(|(_, at)| now.duration_since(*at) >= every);
        if stale {
            let rows = backend.transfer_files(view.id, 0, FILE_PAGE);
            self.rows.insert(view.id, (rows, now));
        }
        ctx.request_repaint_after(every);
    }

    fn history_ui(
        &mut self,
        ui: &mut egui::Ui,
        tx: Tx,
        env: &QueueEnv<'_>,
        actions: &mut Vec<QueueAction>,
    ) {
        let stale = self
            .history
            .as_ref()
            .is_none_or(|(_, at)| at.elapsed() >= Duration::from_secs(2));
        if stale {
            self.history = Some((env.backend.transfer_history(), Instant::now()));
        }
        let records: Vec<TransferRecord> = self
            .history
            .as_ref()
            .map(|(r, _)| r.iter().rev().cloned().collect())
            .unwrap_or_default();
        if records.is_empty() {
            ui.label(theme::caption(ui, tx.t(S::TqHistoryEmpty)));
            return;
        }
        let now = now_unix_ms();
        for r in &records {
            history_row(ui, tx, env, r, now, actions);
        }
    }
}

/// Stopped for good: unlike a paused transfer, which stays in the queue to be resumed.
pub fn finished(v: &TransferView) -> bool {
    v.status.is_final() && v.status != TransferStatus::Paused
}

/// Indexes of `entries` in the order `sort` asks for.
pub fn sort_order(entries: &[Entry], sort: Sort) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..entries.len()).collect();
    let e = |i: usize| &entries[i].view;
    match sort {
        Sort::Status => {
            idx.sort_by_key(|&i| (finished(e(i)), std::cmp::Reverse(e(i).started_unix_ms), i))
        }
        Sort::Newest => idx.sort_by_key(|&i| (std::cmp::Reverse(e(i).started_unix_ms), i)),
        Sort::Name => idx.sort_by_key(|&i| (e(i).label.to_lowercase(), i)),
        Sort::Size => idx.sort_by_key(|&i| (std::cmp::Reverse(e(i).total_bytes), i)),
        Sort::Progress => {
            idx.sort_by(|&a, &b| e(a).fraction().total_cmp(&e(b).fraction()).then(a.cmp(&b)))
        }
    }
    idx
}

fn transfer_row(
    ui: &mut egui::Ui,
    tx: Tx,
    entry: &Entry,
    open: bool,
    toggle: &mut bool,
    actions: &mut Vec<QueueAction>,
) {
    let v = &entry.view;
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
            if !entry.who.is_empty() {
                let key = match v.dir {
                    TransferDir::Send => S::TqToPeer,
                    TransferDir::Receive => S::TqFromPeer,
                };
                ui.label(theme::caption(ui, &tx.f(key, &[("name", &entry.who)])));
            }
            ui.label(theme::caption(ui, &status_line(v, tx)));
        });
        theme::row_end(ui, tx.rtl(), |ui| {
            controls(ui, tx, v, open, toggle, actions)
        });
    });
    progress_bar(ui, tx.rtl(), v);
}

fn controls(
    ui: &mut egui::Ui,
    tx: Tx,
    v: &TransferView,
    open: bool,
    toggle: &mut bool,
    actions: &mut Vec<QueueAction>,
) {
    let more = if open {
        Icon::ChevronUp
    } else {
        Icon::ChevronDown
    };
    let tip = if open { S::TqHideFiles } else { S::TqShowFiles };
    if v.total_files > 0 && icons::icon_button(ui, tx, more, tx.t(tip)).clicked() {
        *toggle = true;
    }
    if !v.status.is_final() {
        if icons::icon_button(ui, tx, Icon::Stop, tx.t(S::TransferCancel)).clicked() {
            actions.push(QueueAction::Cancel(v.id));
        }
        if v.can_pause() && icons::icon_button(ui, tx, Icon::Pause, tx.t(S::TqPause)).clicked() {
            actions.push(QueueAction::Pause(v.id));
        }
    } else if v.local && v.status.can_retry() {
        let (icon, tip) = if v.status == TransferStatus::Paused {
            (Icon::Play, S::TqResume)
        } else {
            (Icon::Refresh, S::TransferRetry)
        };
        if icons::icon_button(ui, tx, icon, tx.t(tip)).clicked() {
            actions.push(QueueAction::Resume(v.id));
        }
    }
    if v.dir == TransferDir::Receive && v.status != TransferStatus::Preparing {
        if let Some(dest) = &v.local_dest {
            if icons::icon_button(ui, tx, Icon::FolderOpen, tx.t(S::OpenFolder)).clicked() {
                actions.push(QueueAction::OpenFolder(dest.clone()));
            }
            if v.status == TransferStatus::Completed && v.total_files == 1 {
                let file = dest.join(&v.label);
                if is_risky_extension(&file) {
                    if icons::icon_button(ui, tx, Icon::File, tx.t(S::TqShowInFolder)).clicked() {
                        actions.push(QueueAction::ShowInFolder(file));
                    }
                } else if icons::icon_button(ui, tx, Icon::File, tx.t(S::TqOpenFile)).clicked() {
                    actions.push(QueueAction::OpenFile(file));
                }
            }
        }
    }
}

/// A thin progress bar that fills from the reading-start side (right in Arabic).
pub fn progress_bar(ui: &mut egui::Ui, rtl: bool, v: &TransferView) {
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
            TransferStatus::Paused => ui.visuals().weak_text_color(),
            _ => theme::accent(dark),
        };
        ui.painter().rect_filled(filled, round, color);
    }
}

fn file_state_text(tx: Tx, s: FileState) -> &'static str {
    tx.t(match s {
        FileState::Pending => S::TqFileWaiting,
        FileState::Active => S::TqFileActive,
        FileState::Done => S::TqFileDone,
        FileState::Failed => S::TqFileFailed,
        FileState::Skipped => S::TqFileSkipped,
    })
}

fn file_list(ui: &mut egui::Ui, tx: Tx, rows: &[FileRow], total_files: u32) {
    let dark = ui.visuals().dark_mode;
    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical()
                .max_height(150.0)
                .id_salt(("tq-files", rows.len(), total_files))
                .show(ui, |ui| {
                    for r in rows {
                        theme::row(ui, tx.rtl(), |ui| {
                            ui.label(tx.d(&r.rel_path));
                            theme::row_end(ui, tx.rtl(), |ui| {
                                let color = match r.state {
                                    FileState::Failed => theme::danger(dark),
                                    FileState::Done => theme::success(dark),
                                    _ => ui.visuals().weak_text_color(),
                                };
                                ui.label(
                                    RichText::new(file_state_text(tx, r.state))
                                        .small()
                                        .color(color),
                                );
                                ui.label(theme::caption(
                                    ui,
                                    &ltr_isolate(&format::file_size(r.size, tx.lang, tx.digits)),
                                ));
                            });
                        });
                    }
                    let shown = u32::try_from(rows.len()).unwrap_or(u32::MAX);
                    if total_files > shown {
                        let n = tx.digits(&(total_files - shown).to_string());
                        ui.label(theme::caption(ui, &tx.f(S::TqMoreFiles, &[("n", &n)])));
                    }
                });
        });
}

fn history_row(
    ui: &mut egui::Ui,
    tx: Tx,
    env: &QueueEnv<'_>,
    r: &TransferRecord,
    now_ms: u64,
    actions: &mut Vec<QueueAction>,
) {
    ui.add_space(4.0);
    theme::row(ui, tx.rtl(), |ui| {
        let icon = if r.sent { Icon::Upload } else { Icon::Download };
        icons::icon(ui, tx, icon, icons::BUTTON_ICON, ui.visuals().text_color());
        theme::column(ui, tx.rtl(), |ui| {
            ui.label(RichText::new(tx.d(&r.label)).strong());
            ui.label(theme::caption(ui, &history_line(r, tx, now_ms)));
        });
        theme::row_end(ui, tx.rtl(), |ui| {
            if let (Some(dest), false) = (&r.local_dest, r.sent) {
                if icons::icon_button(ui, tx, Icon::FolderOpen, tx.t(S::OpenFolder)).clicked() {
                    actions.push(QueueAction::OpenFolder(PathBuf::from(dest)));
                }
            }
            if r.can_repeat() {
                let same_peer = env.session_peer == Some(r.peer_id);
                let tip = if same_peer {
                    tx.t(if r.sent {
                        S::TqSendAgain
                    } else {
                        S::TqGetAgain
                    })
                    .to_owned()
                } else {
                    tx.f(S::TqConnectToRepeat, &[("name", &r.peer_name)])
                };
                let enabled = same_peer && env.can_start;
                let resp = ui.add_enabled(
                    enabled,
                    icons::icon_text_button(
                        tx,
                        Icon::Refresh,
                        if r.sent {
                            tx.t(S::TqSendAgain)
                        } else {
                            tx.t(S::TqGetAgain)
                        },
                    ),
                );
                if resp.on_hover_text(tip).clicked() {
                    actions.push(QueueAction::Repeat(r.clone()));
                }
            }
        });
    });
}

/// Text under a recent transfer: outcome, size, who and when.
pub fn history_line(r: &TransferRecord, tx: Tx, now_ms: u64) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(match r.outcome {
        Outcome::Completed if r.failed_files == 0 => tx.raw(S::TransferDone).to_owned(),
        Outcome::Completed => tx.f(
            S::TransferDoneFailures,
            &[("n", &tx.digits(&r.failed_files.to_string()))],
        ),
        Outcome::Cancelled => tx.raw(S::TransferCancelled).to_owned(),
        Outcome::Failed => tx.raw(S::TqFileFailed).to_owned(),
        Outcome::Lost => tx.raw(S::TransferLost).to_owned(),
    });
    parts.push(ltr_isolate(&format::file_size(
        r.total_bytes,
        tx.lang,
        tx.digits,
    )));
    if !r.peer_name.is_empty() {
        let key = if r.sent { S::TqToPeer } else { S::TqFromPeer };
        parts.push(tx.raw(key).replace("{name}", &r.peer_name));
    }
    parts.push(ago(tx, now_ms, r.finished_unix_ms));
    tx.p(&parts.join("  ·  "))
}

/// "5 min ago" style text, in logical order (join it with other parts, then `Tx::p`).
pub fn ago(tx: Tx, now_ms: u64, then_ms: u64) -> String {
    let mins = now_ms.saturating_sub(then_ms) / 60_000;
    let n = |v: u64| tx.digits(&v.to_string());
    match mins {
        0 => tx.raw(S::TqJustNow).to_owned(),
        1..=59 => tx.raw(S::TqAgoMinutes).replace("{n}", &n(mins)),
        60..=1439 => tx.raw(S::TqAgoHours).replace("{n}", &n(mins / 60)),
        _ => tx.raw(S::TqAgoDays).replace("{n}", &n(mins / 1440)),
    }
}

/// Whether opening `path` could run code (programs, scripts, shortcuts, installers). Files
/// received from another PC are never launched directly when this is true; the file manager
/// shows them selected instead.
pub fn is_risky_extension(path: &Path) -> bool {
    const RISKY: &[&str] = &[
        "exe",
        "com",
        "scr",
        "bat",
        "cmd",
        "msi",
        "msp",
        "ps1",
        "psm1",
        "vbs",
        "vbe",
        "js",
        "jse",
        "wsf",
        "wsh",
        "hta",
        "lnk",
        "url",
        "pif",
        "cpl",
        "dll",
        "jar",
        "reg",
        "inf",
        "sys",
        "appx",
        "msix",
        "application",
        "gadget",
        "msc",
        "scf",
        "chm",
        "iso",
        "vhd",
        "vhdx",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .is_none_or(|e| RISKY.contains(&e.to_ascii_lowercase().as_str()))
}

/// Opens `path` with its default program (never called for risky extensions).
pub fn open_file(path: &Path) {
    if is_risky_extension(path) {
        show_in_folder(path);
        return;
    }
    spawn_file_manager(&[path.as_os_str()]);
}

/// Shows `path` selected in the file manager.
pub fn show_in_folder(path: &Path) {
    #[cfg(windows)]
    {
        let mut arg = std::ffi::OsString::from("/select,");
        arg.push(path.as_os_str());
        spawn_file_manager(&[arg.as_os_str()]);
    }
    #[cfg(not(windows))]
    if let Some(dir) = path.parent() {
        super::transfers::open_folder(dir);
    }
}

#[cfg(windows)]
fn spawn_file_manager(args: &[&std::ffi::OsStr]) {
    if let Err(e) = std::process::Command::new("explorer").args(args).spawn() {
        tracing::warn!(%e, "could not open the file");
    }
}

#[cfg(not(windows))]
fn spawn_file_manager(args: &[&std::ffi::OsStr]) {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Err(e) = std::process::Command::new(cmd).args(args).spawn() {
        tracing::warn!(%e, "could not open the file");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{DigitStyle, Lang};

    fn view(
        id: u64,
        label: &str,
        status: TransferStatus,
        total: u64,
        done: u64,
        at: u64,
    ) -> TransferView {
        TransferView {
            id: TransferId(id),
            dir: TransferDir::Send,
            label: label.into(),
            status,
            bytes_done: done,
            total_bytes: total,
            files_done: 0,
            total_files: 3,
            failed_files: 0,
            bytes_per_sec: 1000,
            local_dest: None,
            local: true,
            started_unix_ms: at,
        }
    }

    fn queue() -> Queue {
        let mut q = Queue::default();
        q.update(view(1, "ب ملف", TransferStatus::Completed, 50, 50, 10), "");
        q.update(view(2, "alpha", TransferStatus::Running, 100, 10, 20), "");
        q.update(view(3, "تقرير", TransferStatus::Running, 300, 150, 30), "");
        q.update(view(4, "Zed", TransferStatus::Paused, 10, 9, 40), "");
        q
    }

    fn ids(q: &Queue) -> Vec<u64> {
        q.order()
            .iter()
            .map(|&i| q.entries()[i].view.id.0)
            .collect()
    }

    #[test]
    fn sorting_orders_by_every_key() {
        let mut q = queue();
        // Status: not finished first (running and paused), newest first, finished last.
        assert_eq!(ids(&q), [4, 3, 2, 1]);
        q.sort = Sort::Newest;
        assert_eq!(ids(&q), [4, 3, 2, 1]);
        q.sort = Sort::Name;
        assert_eq!(
            ids(&q),
            [2, 4, 1, 3].map(|x| x),
            "Latin before Arabic, case-insensitive"
        );
        q.sort = Sort::Size;
        assert_eq!(ids(&q), [3, 2, 1, 4]);
        q.sort = Sort::Progress;
        assert_eq!(ids(&q), [2, 3, 4, 1]);
    }

    #[test]
    fn totals_count_only_unfinished_transfers() {
        let q = queue();
        assert_eq!(q.active_count(), 3);
        assert_eq!(q.total_rate(), 2000, "paused transfers do not add speed");
        // (10 + 150 + 9) / (100 + 300 + 10)
        assert!((q.active_fraction() - 169.0 / 410.0).abs() < 1e-4);
        assert_eq!(Queue::default().active_fraction(), 0.0);
    }

    #[test]
    fn update_replaces_by_id_keeps_who_and_clear_keeps_running() {
        let mut q = Queue::default();
        assert!(q.update(
            view(1, "a", TransferStatus::Running, 10, 1, 1),
            "حاسوب سارة"
        ));
        assert!(!q.update(view(1, "a", TransferStatus::Completed, 10, 10, 1), ""));
        assert_eq!(q.entries().len(), 1);
        assert_eq!(q.entries()[0].who, "حاسوب سارة");
        q.update(view(2, "b", TransferStatus::Running, 10, 1, 2), "");
        q.clear_finished();
        assert_eq!(q.entries().len(), 1);
        assert_eq!(q.entries()[0].view.id, TransferId(2));
    }

    #[test]
    fn the_list_is_capped_dropping_finished_first() {
        let mut q = Queue::default();
        q.update(
            view(0, "keep running", TransferStatus::Running, 1, 0, 0),
            "",
        );
        for i in 1..=MAX_ITEMS as u64 {
            q.update(view(i, "x", TransferStatus::Completed, 1, 1, i), "");
        }
        assert_eq!(q.entries().len(), MAX_ITEMS);
        assert!(q.entries().iter().any(|e| e.view.id == TransferId(0)));
    }

    #[test]
    fn programs_and_scripts_are_never_opened_directly() {
        for risky in [
            "a.exe",
            "B.BAT",
            "x.Lnk",
            "ملف.ps1",
            "noext",
            "setup.msi",
            "a.js",
        ] {
            assert!(is_risky_extension(Path::new(risky)), "{risky}");
        }
        for fine in ["تقرير.pdf", "photo.JPG", "notes.txt", "a.docx", "عرض.pptx"] {
            assert!(!is_risky_extension(Path::new(fine)), "{fine}");
        }
    }

    #[test]
    fn history_lines_and_relative_times_read_in_both_languages() {
        let r = TransferRecord {
            finished_unix_ms: 1_000_000,
            label: "مشروع".into(),
            peer_name: "حاسوب المكتب".into(),
            total_bytes: 3 << 20,
            ..TransferRecord::default()
        };
        let en = Tx::new(Lang::En, DigitStyle::Western);
        let line = history_line(&r, en, 1_000_000 + 5 * 60_000);
        for needle in ["Done", "3.0 MB", "5 min ago"] {
            assert!(line.contains(needle), "{needle} in {line}");
        }
        let ar = Tx::new(Lang::Ar, DigitStyle::ArabicIndic);
        let line = history_line(&r, ar, 1_000_000 + 3 * 3_600_000);
        assert!(line.contains('٣'), "{line}");
        assert_eq!(ago(en, 5, 5), "just now");
        let days = ago(en, 3 * 86_400_000, 0);
        assert!(days.contains("d ago") && days.contains('3'), "{days:?}");
    }

    #[test]
    fn file_state_texts_exist_in_both_languages() {
        for lang in [Lang::En, Lang::Ar] {
            let tx = Tx::new(lang, DigitStyle::Western);
            for s in [
                FileState::Pending,
                FileState::Active,
                FileState::Done,
                FileState::Failed,
                FileState::Skipped,
            ] {
                assert!(!file_state_text(tx, s).is_empty());
            }
        }
    }
}
