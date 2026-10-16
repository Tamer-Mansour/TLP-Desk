//! Connection log screen: every session and refused attempt on this device, with filter,
//! search and CSV export. Reads the local `connections.log` only; nothing leaves the machine.

use std::path::Path;

use eframe::egui::{self, RichText};
use time::UtcOffset;

use super::icons::{self, Icon};
use super::settings::PERMISSION_LABELS;
use super::{theme, widgets};
use crate::app::{App, Screen};
use crate::connection_log::export::{
    self, auth_key, direction_key, end_key, format_time, LogFilter,
};
use crate::connection_log::{Direction, LogEntry, LogStore};
use crate::i18n::{format, Tx, S};

/// At most this many rows are laid out (newest first); the CSV export always has everything.
const MAX_ROWS: usize = 500;

/// State of the connection-log screen (not persisted).
#[derive(Debug)]
pub struct ConnLogUi {
    /// Entries as read from the log, oldest first.
    pub entries: Vec<LogEntry>,
    /// Entries were injected (tests, README media): do not re-read the file.
    pub fixed: bool,
    /// Which kind of entries to show.
    pub filter: LogFilter,
    /// Search text (name, ID, address, fingerprint).
    pub query: String,
    /// UTC offset used to show times.
    pub offset: UtcOffset,
    /// Result of the last export (display-ready).
    pub status: Option<String>,
}

impl Default for ConnLogUi {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            fixed: false,
            filter: LogFilter::All,
            query: String::new(),
            offset: export::local_offset(),
            status: None,
        }
    }
}

impl ConnLogUi {
    /// Show these entries and never touch the log file (README media, tests).
    pub fn with_fixed(entries: Vec<LogEntry>, offset: UtcOffset) -> Self {
        Self {
            entries,
            fixed: true,
            offset,
            ..Self::default()
        }
    }

    /// Entries passing the filter and search, newest first.
    pub fn visible(&self) -> Vec<&LogEntry> {
        let mut v: Vec<&LogEntry> = self
            .entries
            .iter()
            .filter(|e| export::matches(e, self.filter, &self.query))
            .collect();
        v.sort_by_key(|e| std::cmp::Reverse(e.started_ms));
        v
    }

    fn reload(&mut self) {
        if !self.fixed {
            self.entries = LogStore::default_location()
                .map(|s| s.read_all())
                .unwrap_or_default();
            // TLP-service: sessions the installed service hosted (machine-wide log; users may
            // read it). Missing or unreadable means there are none.
            let machine =
                LogStore::new(crate::service_host::log_dir(&tlp_desk_crypto::default_dir()));
            if LogStore::default_location().map(|s| s.path()) != Some(machine.path()) {
                self.entries.extend(machine.read_all());
            }
        }
    }
}

fn filter_key(f: LogFilter) -> S {
    match f {
        LogFilter::All => S::LogFilterAll,
        LogFilter::Incoming => S::LogIncoming,
        LogFilter::Outgoing => S::LogOutgoing,
        LogFilter::Refused => S::LogFilterRefused,
    }
}

impl App {
    /// Open the connection log screen (re-reads the file).
    pub fn open_connection_log(&mut self) {
        self.conn_log.reload();
        self.conn_log.status = None;
        self.screen = Screen::ConnectionLog;
    }

    /// The whole connection-log screen.
    pub fn connection_log_ui(&mut self, ui: &mut egui::Ui) {
        self.settings_top_bar(ui);
        let tx = self.tx;
        let mut back = false;
        let mut refresh = false;
        let mut export_now = false;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(20),
            )
            .show(ui, |ui| {
                theme::row(ui, tx.rtl(), |ui| {
                    if ui
                        .add(icons::icon_text_button(tx, Icon::Back, tx.t(S::Back)))
                        .clicked()
                    {
                        back = true;
                    }
                    ui.label(RichText::new(tx.t(S::ConnectionLog)).heading().strong());
                });
                ui.add_space(10.0);
                let log = &mut self.conn_log;
                theme::row(ui, tx.rtl(), |ui| {
                    let w = (ui.available_width() * 0.45).clamp(160.0, 360.0);
                    let hint = tx.t(S::LogSearchHint);
                    widgets::text_field(ui, tx, "log-search", &mut log.query, hint, w);
                    for f in LogFilter::ALL {
                        if ui
                            .selectable_label(log.filter == f, tx.t(filter_key(f)))
                            .clicked()
                        {
                            log.filter = f;
                        }
                    }
                });
                ui.add_space(6.0);
                theme::row(ui, tx.rtl(), |ui| {
                    let refresh_button =
                        icons::icon_text_button(tx, Icon::Refresh, tx.t(S::LogRefresh));
                    if ui.add(refresh_button).clicked() {
                        refresh = true;
                    }
                    let export_button = icons::icon_text_button(tx, Icon::Save, tx.t(S::LogExport));
                    if ui
                        .add_enabled(!log.entries.is_empty(), export_button)
                        .clicked()
                    {
                        export_now = true;
                    }
                    let n = tx.digits(&log.visible().len().to_string());
                    ui.label(theme::caption(ui, &tx.f(S::LogCount, &[("n", &n)])));
                });
                if let Some(status) = &log.status {
                    ui.label(theme::caption(ui, status));
                }
                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        theme::column(ui, tx.rtl(), |ui| {
                            let rows = log.visible();
                            if rows.is_empty() {
                                let key = if log.entries.is_empty() {
                                    S::LogEmpty
                                } else {
                                    S::LogNoMatch
                                };
                                ui.label(theme::caption(ui, tx.t(key)));
                            }
                            for e in rows.into_iter().take(MAX_ROWS) {
                                entry_card(ui, tx, e, log.offset);
                                ui.add_space(6.0);
                            }
                        });
                    });
            });
        if refresh {
            self.conn_log.reload();
        }
        if export_now {
            self.export_connection_log();
        }
        if back {
            self.screen = Screen::Home;
        }
    }

    fn export_connection_log(&mut self) {
        let tx = self.tx;
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("TLP-Desk-connections.csv")
            .add_filter("CSV", &["csv"])
            .save_file()
        else {
            return;
        };
        let csv = export::to_csv(&self.conn_log.entries, tx.lang, self.conn_log.offset);
        self.conn_log.status = Some(match write_csv(&path, &csv) {
            Ok(()) => tx.f(S::LogExported, &[("path", &path.display().to_string())]),
            Err(e) => tx.f(S::LogExportFailed, &[("detail", &e.to_string())]),
        });
    }
}

fn write_csv(path: &Path, csv: &str) -> std::io::Result<()> {
    std::fs::write(path, csv)
}

fn entry_card(ui: &mut egui::Ui, tx: Tx, e: &LogEntry, offset: UtcOffset) {
    theme::card(ui, |ui| {
        theme::column(ui, tx.rtl(), |ui| {
            theme::row(ui, tx.rtl(), |ui| {
                let icon = match e.direction {
                    Direction::Incoming => Icon::Users,
                    Direction::Outgoing => Icon::Connect,
                };
                let weak = ui.visuals().weak_text_color();
                icons::icon(ui, tx, icon, 18.0, weak);
                let name = if e.name.trim().is_empty() {
                    tx.t(S::LogUnknown).to_owned()
                } else {
                    tx.d(&e.name)
                };
                ui.label(RichText::new(name).strong());
                ui.label(theme::caption(ui, &tx.n(&group_id(e.device_id))));
                theme::row_end(ui, tx.rtl(), |ui| {
                    let color = if e.end.is_refusal() {
                        ui.visuals().warn_fg_color
                    } else {
                        ui.visuals().weak_text_color()
                    };
                    ui.label(RichText::new(tx.t(end_key(e.end))).small().color(color));
                });
            });
            let when = tx.digits(&format_time(e.started_ms, offset));
            let mut line = vec![tx.raw(direction_key(e.direction)).to_owned(), when];
            if !e.address.is_empty() {
                line.push(tx.digits(&e.address));
            }
            line.push(tx.raw(auth_key(e.auth)).to_owned());
            if e.duration_ms > 0 {
                line.push(format::duration(
                    std::time::Duration::from_millis(e.duration_ms),
                    tx.digits,
                ));
            }
            ui.label(theme::caption(ui, &tx.p(&line.join("  \u{b7}  "))));
            let perms: Vec<&str> = PERMISSION_LABELS
                .iter()
                .filter(|(bit, _)| e.permissions.0.contains(*bit))
                .map(|(_, key)| tx.raw(*key))
                .collect();
            if !perms.is_empty() {
                ui.label(theme::caption(
                    ui,
                    &tx.p(&format!(
                        "{}: {}",
                        tx.raw(S::LogColPermissions),
                        perms.join(", ")
                    )),
                ));
            }
        });
    });
}

/// `482 913 057` for a 9-digit device ID.
fn group_id(id: u32) -> String {
    let s = format!("{id:09}");
    format!("{} {} {}", &s[0..3], &s[3..6], &s[6..9])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection_log::LogEnd;

    fn entry(ms: u64, name: &str, dir: Direction, end: LogEnd) -> LogEntry {
        LogEntry {
            started_ms: ms,
            name: name.into(),
            direction: dir,
            end,
            ..LogEntry::default()
        }
    }

    #[test]
    fn visible_is_newest_first_and_filtered() {
        let mut ui = ConnLogUi::with_fixed(
            vec![
                entry(1, "حاسوب سارة", Direction::Incoming, LogEnd::ClosedRemotely),
                entry(3, "Laptop", Direction::Outgoing, LogEnd::ClosedHere),
                entry(2, "x", Direction::Incoming, LogEnd::WrongCredentials),
            ],
            UtcOffset::UTC,
        );
        let names: Vec<_> = ui.visible().iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, ["Laptop", "x", "حاسوب سارة"]);
        ui.filter = LogFilter::Refused;
        assert_eq!(ui.visible().len(), 1);
        ui.filter = LogFilter::All;
        ui.query = "سارة".into();
        assert_eq!(ui.visible().len(), 1);
    }

    #[test]
    fn device_ids_are_grouped() {
        assert_eq!(group_id(482_913_057), "482 913 057");
        assert_eq!(group_id(7), "000 000 007");
    }
}
