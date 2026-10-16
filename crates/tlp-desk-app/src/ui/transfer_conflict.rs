//! "These files already exist" dialog: for every clashing file the existing and the new
//! version side by side (size, age, newer or older), a choice per file (keep both, replace,
//! skip, or resume an interrupted copy) and an "apply to all" row. Used for downloads on the
//! viewer and, when the host turned the question on, for files a viewer sends to the host.

use std::collections::VecDeque;

use eframe::egui::{self, Id, RichText};

use super::icons::Icon;
use super::theme;
use super::transfer_queue::ago;
use super::widgets;
use crate::backend::files::{
    ConflictChoice, ConflictPrompt, ConflictResolution, FileConflict, TransferId,
};
use crate::backend::transfer_history::now_unix_ms;
use crate::i18n::{format, ltr_isolate, Tx, S};

/// Questions waiting for the user, oldest first.
#[derive(Default)]
pub struct ConflictDialogs {
    queue: VecDeque<Pending>,
    /// Pretend it is this time (Unix ms) when writing ages; for deterministic screenshots.
    pub fixed_now_ms: Option<u64>,
}

struct Pending {
    prompt: ConflictPrompt,
    /// Who sends the files (host side only).
    peer: String,
    default: ConflictChoice,
    choices: Vec<ConflictChoice>,
}

/// The user's decision on one prompt: `None` cancels the transfer.
pub type Decision = (TransferId, Option<ConflictResolution>);

impl ConflictDialogs {
    /// Queues a question. `peer` names the sender when the files come to this host.
    pub fn push(&mut self, prompt: ConflictPrompt, peer: &str) {
        if self.queue.iter().any(|p| p.prompt.id == prompt.id) {
            return;
        }
        let choices = vec![ConflictChoice::KeepBoth; prompt.files.len()];
        self.queue.push_back(Pending {
            prompt,
            peer: peer.to_owned(),
            default: ConflictChoice::KeepBoth,
            choices,
        });
    }

    /// Drops the question of a transfer that no longer waits (answered elsewhere, timed out).
    pub fn close(&mut self, id: TransferId) {
        self.queue.retain(|p| p.prompt.id != id);
    }

    /// Whether a question is on screen.
    pub fn is_open(&self) -> bool {
        !self.queue.is_empty()
    }

    /// Draws the oldest question; returns the decision once the user made one.
    pub fn show(&mut self, ctx: &egui::Context, tx: Tx) -> Option<Decision> {
        let p = self.queue.front_mut()?;
        let now = self.fixed_now_ms.unwrap_or_else(now_unix_ms);
        let mut result = None;
        egui::Modal::new(Id::new(("transfer-conflict", p.prompt.id.0))).show(ctx, |ui| {
            ui.set_width(520.0);
            theme::column(ui, tx.rtl(), |ui| {
                let n = tx.digits(&p.prompt.total.to_string());
                widgets::icon_heading(ui, tx, Icon::Warning, &tx.f(S::CfTitle, &[("n", &n)]));
                ui.add_space(4.0);
                let folder = tx.d(&p.prompt.dest_dir);
                let body = if p.prompt.local || p.peer.is_empty() {
                    tx.f(S::CfBody, &[("folder", &folder)])
                } else {
                    tx.f(
                        S::CfFromPeer,
                        &[("name", &tx.d(&p.peer)), ("folder", &folder)],
                    )
                };
                ui.label(body);
                ui.add_space(6.0);
                theme::row(ui, tx.rtl(), |ui| {
                    ui.label(RichText::new(tx.t(S::CfApplyAll)).strong());
                    for (choice, key) in [
                        (ConflictChoice::KeepBoth, S::CfKeepBoth),
                        (ConflictChoice::Overwrite, S::CfReplace),
                        (ConflictChoice::Skip, S::CfSkip),
                    ] {
                        if ui
                            .selectable_label(p.default == choice, tx.t(key))
                            .clicked()
                        {
                            p.default = choice;
                            p.choices.iter_mut().for_each(|c| *c = choice);
                        }
                    }
                });
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .max_height(240.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (i, c) in p.prompt.files.iter().enumerate() {
                            if let Some(choice) = p.choices.get_mut(i) {
                                conflict_row(ui, tx, c, choice, now);
                            }
                        }
                        let listed = u32::try_from(p.prompt.files.len()).unwrap_or(u32::MAX);
                        if p.prompt.total > listed {
                            let n = tx.digits(&(p.prompt.total - listed).to_string());
                            ui.label(theme::caption(ui, &tx.f(S::CfMore, &[("n", &n)])));
                        }
                    });
                ui.add_space(8.0);
                theme::row(ui, tx.rtl(), |ui| {
                    let go =
                        widgets::primary_icon_button(ui, tx, Icon::Accept, tx.t(S::CfContinue));
                    if ui.add(go).clicked() {
                        result = Some(Some(build_resolution(
                            p.default,
                            &p.prompt.files,
                            &p.choices,
                        )));
                    }
                    let cancel =
                        super::icons::icon_text_button(tx, Icon::Reject, tx.t(S::CfCancel));
                    if ui.add(cancel).clicked() {
                        result = Some(None);
                    }
                });
            });
        });
        let answer = result?;
        let id = p.prompt.id;
        self.queue.pop_front();
        Some((id, answer))
    }
}

fn conflict_row(
    ui: &mut egui::Ui,
    tx: Tx,
    c: &FileConflict,
    choice: &mut ConflictChoice,
    now: u64,
) {
    ui.separator();
    ui.label(RichText::new(tx.d(&c.rel_path)).strong());
    let size = |n: u64| ltr_isolate(&format::file_size(n, tx.lang, tx.digits));
    let cmp = match compare_age(c) {
        Age::Newer => format!(" ({})", tx.raw(S::CfNewer)),
        Age::Older => format!(" ({})", tx.raw(S::CfOlder)),
        Age::Same => format!(" ({})", tx.raw(S::CfSameTime)),
    };
    let line = |label: S, bytes: u64, when: i64, extra: &str| {
        let when = u64::try_from(when).unwrap_or(0);
        tx.p(&format!(
            "{}: {}  ·  {}{}",
            tx.raw(label),
            size(bytes),
            ago(tx, now, when),
            extra
        ))
    };
    ui.label(theme::caption(
        ui,
        &line(
            S::CfExisting,
            c.existing_size,
            c.existing_modified_unix_ms,
            "",
        ),
    ));
    ui.label(theme::caption(
        ui,
        &line(
            S::CfIncoming,
            c.incoming_size,
            c.incoming_modified_unix_ms,
            &cmp,
        ),
    ));
    let mut options = vec![
        (ConflictChoice::KeepBoth, tx.t(S::CfKeepBoth).to_owned()),
        (ConflictChoice::Overwrite, tx.t(S::CfReplace).to_owned()),
        (ConflictChoice::Skip, tx.t(S::CfSkip).to_owned()),
    ];
    if let Some(done) = c.partial_bytes {
        options.push((
            ConflictChoice::Resume,
            tx.f(S::CfResume, &[("size", &size(done))]),
        ));
    }
    let shown = options
        .iter()
        .find(|(o, _)| o == choice)
        .map(|(_, t)| t.clone())
        .unwrap_or_default();
    egui::ComboBox::from_id_salt(("cf-choice", c.index))
        .selected_text(shown)
        .width(260.0)
        .show_ui(ui, |ui| {
            for (o, text) in options {
                ui.selectable_value(choice, o, text);
            }
        });
}

/// Whether the incoming file is newer than the existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Age {
    /// Modified later than the existing file.
    Newer,
    /// Modified earlier.
    Older,
    /// Within a second of it.
    Same,
}

/// Compares the two versions' modification times.
pub fn compare_age(c: &FileConflict) -> Age {
    let d = c.incoming_modified_unix_ms - c.existing_modified_unix_ms;
    match d {
        1000.. => Age::Newer,
        ..=-1000 => Age::Older,
        _ => Age::Same,
    }
}

/// The engine's answer for the choices made: the common choice is the default, the files that
/// differ from it are listed.
pub fn build_resolution(
    default: ConflictChoice,
    files: &[FileConflict],
    choices: &[ConflictChoice],
) -> ConflictResolution {
    ConflictResolution {
        default,
        per_file: files
            .iter()
            .zip(choices)
            .filter(|(_, c)| **c != default)
            .map(|(f, c)| (f.index, *c))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conflict(index: u32, name: &str, existing: i64, incoming: i64) -> FileConflict {
        FileConflict {
            index,
            rel_path: name.into(),
            existing_size: 10,
            existing_modified_unix_ms: existing,
            incoming_size: 20,
            incoming_modified_unix_ms: incoming,
            partial_bytes: None,
        }
    }

    fn prompt(id: u64, n: usize) -> ConflictPrompt {
        ConflictPrompt {
            id: TransferId(id),
            local: true,
            label: "مجلد".into(),
            dest_dir: "C:\\وارد".into(),
            total: n as u32,
            files: (0..n)
                .map(|i| conflict(i as u32, &format!("ملف {i}.txt"), 0, 0))
                .collect(),
        }
    }

    #[test]
    fn only_deviations_from_the_common_choice_are_listed() {
        let files = [
            conflict(0, "أ.txt", 0, 0),
            conflict(3, "ب.txt", 0, 0),
            conflict(7, "ج.txt", 0, 0),
        ];
        let choices = [
            ConflictChoice::Skip,
            ConflictChoice::Overwrite,
            ConflictChoice::Skip,
        ];
        let r = build_resolution(ConflictChoice::Skip, &files, &choices);
        assert_eq!(r.default, ConflictChoice::Skip);
        assert_eq!(r.per_file, vec![(3, ConflictChoice::Overwrite)]);
        let all = build_resolution(
            ConflictChoice::KeepBoth,
            &files,
            &[ConflictChoice::KeepBoth; 3],
        );
        assert!(all.per_file.is_empty());
    }

    #[test]
    fn newer_and_older_need_a_second_of_difference() {
        assert_eq!(compare_age(&conflict(0, "a", 0, 5_000)), Age::Newer);
        assert_eq!(compare_age(&conflict(0, "a", 5_000, 0)), Age::Older);
        assert_eq!(compare_age(&conflict(0, "a", 1_000, 1_400)), Age::Same);
    }

    #[test]
    fn queue_keeps_order_ignores_duplicates_and_closes_by_id() {
        let mut d = ConflictDialogs::default();
        assert!(!d.is_open());
        d.push(prompt(1, 2), "");
        d.push(prompt(1, 2), "");
        d.push(prompt(2, 1), "حاسوب سارة");
        assert!(d.is_open());
        assert_eq!(d.queue.len(), 2);
        d.close(TransferId(1));
        assert_eq!(d.queue.front().unwrap().prompt.id, TransferId(2));
        assert_eq!(d.queue.front().unwrap().choices.len(), 1);
        d.close(TransferId(2));
        assert!(!d.is_open());
    }
}
