//! Recording UI (TLP-recording): the record toggle of the session toolbar, the Recordings
//! window (list, open folder, play, delete, repair) and the playback windows it opens.
//!
//! Privacy: recording is local to this viewer and the host is not notified (the protocol has
//! no message for it). The viewer always shows a red indicator with the elapsed time while
//! recording, and the Recordings window says that the other side is not told.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Id, RichText};
use tlp_desk_viewer::recording::{delete_recording, list_recordings, recover_file, RecordingEntry};

use super::icons::{self, Icon};
use super::playback::PlaybackWindow;
use super::{theme, widgets};
use crate::backend::recording::{recordings_folder, RecordingProblem};
use crate::i18n::{format, DigitStyle, Tx, S};

/// How often an open Recordings window re-reads the folder (a new file appears, a recording
/// grows).
const RELOAD_EVERY: Duration = Duration::from_secs(3);

/// Recording state of a viewer session, as the toolbar shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordState {
    #[default]
    Off,
    /// Asked the backend to start; waiting for `RecordingStarted`.
    Starting,
    On {
        since: Instant,
    },
}

/// Red used for the recording indicator (readable on light and dark toolbars).
pub fn record_red(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(0xFF, 0x5A, 0x5F)
    } else {
        Color32::from_rgb(0xD0, 0x1F, 0x2B)
    }
}

/// The record toggle plus, while recording, a red dot and the elapsed time. Returns
/// `Some(true)` to start and `Some(false)` to stop.
pub fn record_toggle(ui: &mut egui::Ui, tx: Tx, state: RecordState) -> Option<bool> {
    let red = record_red(ui.visuals().dark_mode);
    match state {
        RecordState::Off => icons::icon_toggle(ui, tx, false, Icon::Record, tx.t(S::RecStart))
            .clicked()
            .then_some(true),
        RecordState::Starting => {
            icons::icon_toggle_enabled(ui, tx, false, true, Icon::Record, tx.t(S::RecStarting));
            None
        }
        RecordState::On { since } => {
            let button = egui::Button::selectable(true, Icon::Record.image(icons::TOOL_ICON, red));
            let stop = ui.add(button).on_hover_text(tx.t(S::RecStop)).clicked();
            theme::status_dot(ui, red);
            let elapsed = format::duration(since.elapsed(), DigitStyle::Western);
            let text = tx.f(S::RecActive, &[("time", &tx.digits(&elapsed))]);
            ui.label(RichText::new(text).color(red).strong())
                .on_hover_text(tx.t(S::RecStop));
            // Keep the clock ticking without new frames.
            ui.ctx().request_repaint_after(Duration::from_millis(500));
            stop.then_some(false)
        }
    }
}

/// Display text for a recording problem.
pub fn problem_text(tx: Tx, p: &RecordingProblem) -> String {
    match p {
        RecordingProblem::DiskFull => tx.t(S::RecErrDiskFull).to_owned(),
        RecordingProblem::NoFolder => tx.t(S::RecErrNoFolder).to_owned(),
        RecordingProblem::Failed(d) => tx.f(S::RecErrFailed, &[("detail", d)]),
    }
}

/// Opens `folder` in the system file manager (creating it first).
fn open_folder(folder: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(folder)?;
    #[cfg(windows)]
    let program = "explorer";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let program = "xdg-open";
    std::process::Command::new(program).arg(folder).spawn()?;
    Ok(())
}

/// The Recordings window and the playback windows opened from it.
pub struct RecordingsUi {
    /// Whether the Recordings window is shown.
    pub open: bool,
    folder: Option<PathBuf>,
    entries: Vec<RecordingEntry>,
    loaded: Option<Instant>,
    confirm_delete: Option<RecordingEntry>,
    message: Option<String>,
    players: Vec<PlaybackWindow>,
}

impl Default for RecordingsUi {
    fn default() -> Self {
        Self::new(recordings_folder())
    }
}

impl RecordingsUi {
    /// Recordings in `folder` (`None`: no profile folder, the window says so).
    pub fn new(folder: Option<PathBuf>) -> Self {
        Self {
            open: false,
            folder,
            entries: Vec::new(),
            loaded: None,
            confirm_delete: None,
            message: None,
            players: Vec::new(),
        }
    }

    /// Re-reads the folder.
    pub fn reload(&mut self) {
        self.loaded = Some(Instant::now());
        let Some(folder) = &self.folder else {
            self.entries.clear();
            return;
        };
        match list_recordings(folder) {
            Ok(e) => self.entries = e,
            Err(e) => {
                tracing::warn!(%e, "cannot list recordings");
                self.entries.clear();
            }
        }
    }

    /// The listed recordings (after the last reload).
    pub fn entries(&self) -> &[RecordingEntry] {
        &self.entries
    }

    /// Opens a playback window for `path` (or brings the open one back).
    pub fn play(&mut self, ctx: &egui::Context, tx: Tx, path: &Path) {
        if self.players.iter().any(|p| p.path() == path) {
            return;
        }
        match PlaybackWindow::open(ctx, path) {
            Ok(w) => self.players.push(w),
            Err(e) => {
                tracing::warn!(%e, path = %path.display(), "cannot open recording");
                self.message = Some(tx.f(S::RecErrOpen, &[("detail", &e.to_string())]));
            }
        }
    }

    /// Draws the Recordings window (when open) and every playback window. `recording_now`
    /// disables repairing (the file being written looks interrupted until it is closed).
    pub fn show(&mut self, ctx: &egui::Context, tx: Tx, recording_now: bool) {
        self.players.retain_mut(|p| p.show(ctx, tx));
        if !self.open {
            return;
        }
        if self.loaded.is_none_or(|t| t.elapsed() >= RELOAD_EVERY) {
            self.reload();
        }
        let mut open = self.open;
        let mut actions = Vec::new();
        egui::Window::new(tx.t(S::Recordings))
            .id(Id::new("tlp-recordings"))
            .open(&mut open)
            .default_size([560.0, 420.0])
            .min_width(360.0)
            .collapsible(false)
            .show(ctx, |ui| self.list_ui(ui, tx, recording_now, &mut actions));
        self.open = open;
        for a in actions {
            self.apply(ctx, tx, a);
        }
        self.dialogs(ctx, tx);
        if self.open {
            ctx.request_repaint_after(RELOAD_EVERY);
        }
    }

    fn list_ui(&self, ui: &mut egui::Ui, tx: Tx, recording_now: bool, actions: &mut Vec<Action>) {
        theme::column(ui, tx.rtl(), |ui| {
            ui.label(theme::caption(ui, tx.t(S::RecordingsHint)));
            ui.add_space(4.0);
            theme::row(ui, tx.rtl(), |ui| {
                if let Some(folder) = &self.folder {
                    if ui
                        .add(icons::icon_text_button(
                            tx,
                            Icon::FolderOpen,
                            tx.t(S::RecOpenFolder),
                        ))
                        .on_hover_text(tx.d(&folder.display().to_string()))
                        .clicked()
                    {
                        actions.push(Action::OpenFolder);
                    }
                }
                if icons::icon_button(ui, tx, Icon::Refresh, tx.t(S::RecRefresh)).clicked() {
                    actions.push(Action::Reload);
                }
            });
            if let Some(m) = &self.message {
                let dark = ui.visuals().dark_mode;
                ui.label(RichText::new(m).color(theme::danger(dark)));
            }
            ui.separator();
            if self.folder.is_none() {
                ui.label(tx.t(S::RecErrNoFolder));
                return;
            }
            if self.entries.is_empty() {
                ui.add_space(12.0);
                ui.label(theme::caption(ui, tx.t(S::RecordingsEmpty)));
                return;
            }
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for e in &self.entries {
                        entry_row(ui, tx, e, recording_now, actions);
                        ui.separator();
                    }
                });
        });
    }

    fn apply(&mut self, ctx: &egui::Context, tx: Tx, action: Action) {
        self.message = None;
        let fail = |e: &dyn std::fmt::Display| tx.f(S::RecErrAction, &[("detail", &e.to_string())]);
        match action {
            Action::Reload => self.reload(),
            Action::OpenFolder => {
                if let Some(folder) = &self.folder {
                    if let Err(e) = open_folder(folder) {
                        self.message = Some(fail(&e));
                    }
                }
            }
            Action::Play(path) => self.play(ctx, tx, &path),
            Action::AskDelete(entry) => self.confirm_delete = Some(entry),
            Action::Repair(path) => {
                if let Err(e) = recover_file(&path) {
                    tracing::warn!(%e, path = %path.display(), "repair failed");
                    self.message = Some(fail(&e));
                }
                self.reload();
            }
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context, tx: Tx) {
        let Some(entry) = self.confirm_delete.clone() else {
            return;
        };
        let mut answer = None;
        egui::Modal::new(Id::new("tlp-recording-delete")).show(ctx, |ui| {
            ui.set_width(400.0);
            theme::column(ui, tx.rtl(), |ui| {
                widgets::icon_heading(ui, tx, Icon::Trash, tx.t(S::RecDeleteTitle));
                ui.add_space(4.0);
                ui.label(tx.f(S::RecDeleteBody, &[("name", &entry.file_name)]));
                ui.add_space(10.0);
                theme::row(ui, tx.rtl(), |ui| {
                    let cancel = icons::icon_text_button(tx, Icon::Close, tx.t(S::Cancel));
                    if ui.add(cancel).clicked() {
                        answer = Some(false);
                    }
                    let b = widgets::danger_icon_button(ui, tx, Icon::Trash, tx.t(S::RecDelete));
                    if ui.add(b).clicked() {
                        answer = Some(true);
                    }
                });
            });
        });
        match answer {
            Some(true) => {
                self.confirm_delete = None;
                // A file being played is closed first.
                self.players.retain(|p| p.path() != entry.path);
                if let Some(folder) = &self.folder {
                    if let Err(e) = delete_recording(folder, &entry.path) {
                        tracing::warn!(%e, "delete failed");
                        self.message = Some(tx.f(S::RecErrAction, &[("detail", &e.to_string())]));
                    }
                }
                self.reload();
            }
            Some(false) => self.confirm_delete = None,
            None => {}
        }
    }
}

/// What the user clicked in the list.
enum Action {
    Reload,
    OpenFolder,
    Play(PathBuf),
    AskDelete(RecordingEntry),
    Repair(PathBuf),
}

fn entry_row(
    ui: &mut egui::Ui,
    tx: Tx,
    e: &RecordingEntry,
    recording_now: bool,
    actions: &mut Vec<Action>,
) {
    theme::row(ui, tx.rtl(), |ui| {
        if icons::icon_button(ui, tx, Icon::Play, tx.t(S::RecPlay)).clicked() {
            actions.push(Action::Play(e.path.clone()));
        }
        theme::column(ui, tx.rtl(), |ui| {
            ui.label(RichText::new(tx.d(&e.file_name)).strong());
            let mut facts = Vec::new();
            if let Some(d) = e.duration {
                facts.push(tx.n(&format::duration(d, DigitStyle::Western)));
            }
            facts.push(tx.p(&format::file_size(e.bytes, tx.lang, tx.digits)));
            if !e.complete {
                facts.push(tx.t(S::RecIncomplete).to_owned());
            }
            ui.label(theme::caption(ui, &facts.join("  ·  ")));
        });
        theme::row_end(ui, tx.rtl(), |ui| {
            if icons::icon_button(ui, tx, Icon::Trash, tx.t(S::RecDelete)).clicked() {
                actions.push(Action::AskDelete(e.clone()));
            }
            if !e.complete {
                let resp = icons::icon_toggle_enabled(
                    ui,
                    tx,
                    !recording_now,
                    false,
                    Icon::Refresh,
                    tx.t(S::RecRepair),
                )
                .on_hover_text(tx.t(S::RecRepairHint));
                if resp.clicked() {
                    actions.push(Action::Repair(e.path.clone()));
                }
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;
    use eframe::egui::{Pos2, Rect};

    fn run(ctx: &egui::Context, f: impl FnMut(&mut egui::Ui)) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(900.0, 700.0))),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, f);
        out.textures_delta.clear(); // headless: no renderer consumes the uploads
    }

    #[test]
    fn toggle_renders_in_every_state_and_language() {
        for lang in [Lang::En, Lang::Ar] {
            let ctx = egui::Context::default();
            icons::install(&ctx);
            let tx = Tx::new(lang, DigitStyle::ArabicIndic);
            for state in [
                RecordState::Off,
                RecordState::Starting,
                RecordState::On {
                    since: Instant::now(),
                },
            ] {
                run(&ctx, |ui| {
                    assert_eq!(record_toggle(ui, tx, state), None, "no click, no action");
                });
            }
        }
    }

    #[test]
    fn clicking_the_toggle_starts_and_stops() {
        let ctx = egui::Context::default();
        icons::install(&ctx);
        let tx = Tx::new(Lang::Ar, DigitStyle::Western);
        let click = |p: Pos2| {
            vec![
                egui::Event::PointerMoved(p),
                egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
                egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                },
            ]
        };
        for (state, want) in [
            (RecordState::Off, Some(true)),
            (
                RecordState::On {
                    since: Instant::now(),
                },
                Some(false),
            ),
            (RecordState::Starting, None),
        ] {
            let mut got = None;
            let mut button_at = None;
            for events in [vec![], click(Pos2::ZERO)] {
                let events = match (events.is_empty(), button_at) {
                    (false, Some(p)) => click(p),
                    _ => events,
                };
                let input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(600.0, 100.0))),
                    events,
                    ..Default::default()
                };
                let mut out = ctx.run_ui(input, |ui| {
                    // The toggle is the first widget: its centre is one icon button in.
                    button_at = Some(ui.cursor().min + egui::vec2(14.0, 12.0));
                    if let Some(a) = record_toggle(ui, tx, state) {
                        got = Some(a);
                    }
                });
                out.textures_delta.clear();
            }
            assert_eq!(got, want, "{state:?}");
        }
    }

    #[test]
    fn problems_have_text_in_both_languages() {
        for lang in [Lang::En, Lang::Ar] {
            let tx = Tx::new(lang, DigitStyle::Western);
            for p in [
                RecordingProblem::DiskFull,
                RecordingProblem::NoFolder,
                RecordingProblem::Failed("قرص".into()),
            ] {
                assert!(!problem_text(tx, &p).is_empty());
            }
        }
    }

    #[test]
    fn window_lists_arabic_names_and_handles_an_empty_folder() {
        let dir = tempfile::tempdir().unwrap();
        // Not a valid recording, but listed (and marked interrupted) like any .mp4.
        std::fs::write(
            dir.path()
                .join("TLP-Desk مكتب أحمد 2026-10-13 18-35-07.mp4"),
            b"x",
        )
        .unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        for lang in [Lang::En, Lang::Ar] {
            let ctx = egui::Context::default();
            icons::install(&ctx);
            let tx = Tx::new(lang, DigitStyle::Western);
            let mut ui_state = RecordingsUi::new(Some(dir.path().to_owned()));
            ui_state.open = true;
            run(&ctx, |ui| ui_state.show(ui.ctx(), tx, false));
            assert_eq!(ui_state.entries().len(), 1);
            assert_eq!(
                ui_state.entries()[0].file_name,
                "TLP-Desk مكتب أحمد 2026-10-13 18-35-07.mp4"
            );
            assert!(!ui_state.entries()[0].complete);
            // Playing a damaged file shows a message instead of a window.
            let path = ui_state.entries()[0].path.clone();
            ui_state.play(&ctx, tx, &path);
            assert!(ui_state.players.is_empty());
            assert!(ui_state.message.is_some());
        }
        let empty = tempfile::tempdir().unwrap();
        let ctx = egui::Context::default();
        icons::install(&ctx);
        let mut ui_state = RecordingsUi::new(Some(empty.path().join("missing")));
        ui_state.open = true;
        run(&ctx, |ui| {
            ui_state.show(ui.ctx(), Tx::new(Lang::Ar, DigitStyle::Western), true)
        });
        assert!(ui_state.entries().is_empty());
        let mut none = RecordingsUi::new(None);
        none.open = true;
        run(&ctx, |ui| {
            none.show(ui.ctx(), Tx::new(Lang::En, DigitStyle::Western), false)
        });
    }
}
