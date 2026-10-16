//! Short notices when a transfer ends ("Received report.pdf", with an "Open folder" button for
//! files that arrived). Used by the viewer session and by the host, where seeing that files
//! arrived is part of knowing what a connected viewer does.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};

use super::icons::{self, Icon};
use super::theme;
use super::transfer_queue::{finished, QueueAction};
use crate::backend::files::{TransferDir, TransferStatus, TransferView};
use crate::i18n::{Tx, S};

/// How long a notice stays.
pub const SHOW_FOR: Duration = Duration::from_secs(8);
/// Most notices on screen at once (older ones are dropped).
const MAX: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Sent,
    Received,
    Failed,
}

#[derive(Debug, Clone)]
struct Toast {
    kind: Kind,
    label: String,
    who: String,
    folder: Option<PathBuf>,
    until: Instant,
}

/// The notices currently shown.
#[derive(Default)]
pub struct Toasts {
    list: Vec<Toast>,
}

impl Toasts {
    /// Adds a notice if `view` just finished (`was_finished`: it already was before this
    /// update). Returns `true` if a notice was added.
    pub fn on_update(
        &mut self,
        view: &TransferView,
        who: &str,
        was_finished: bool,
        now: Instant,
    ) -> bool {
        if was_finished || !finished(view) {
            return false;
        }
        let kind = match (&view.status, view.dir) {
            (TransferStatus::Completed, TransferDir::Send) => Kind::Sent,
            (TransferStatus::Completed, TransferDir::Receive) => Kind::Received,
            (TransferStatus::Failed(_) | TransferStatus::Disconnected, _) => Kind::Failed,
            // Cancelling is the user's own act: no notice.
            _ => return false,
        };
        self.list.push(Toast {
            kind,
            label: view.label.clone(),
            who: who.to_owned(),
            folder: (kind == Kind::Received)
                .then(|| view.local_dest.clone())
                .flatten(),
            until: now + SHOW_FOR,
        });
        let extra = self.list.len().saturating_sub(MAX);
        self.list.drain(..extra);
        true
    }

    /// Number of notices waiting to expire.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// Whether no notice is shown.
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Draws the notices in a corner of the window and returns clicks on their buttons.
    pub fn show(&mut self, ctx: &egui::Context, tx: Tx, salt: &str) -> Vec<QueueAction> {
        let now = Instant::now();
        self.list.retain(|t| t.until > now);
        let mut actions = Vec::new();
        let mut dismiss = None;
        let dark = ctx.global_style().visuals.dark_mode;
        for (i, t) in self.list.iter().enumerate() {
            let anchor = if tx.rtl() {
                egui::Align2::LEFT_BOTTOM
            } else {
                egui::Align2::RIGHT_BOTTOM
            };
            let offset = egui::vec2(if tx.rtl() { 14.0 } else { -14.0 }, -14.0 - 64.0 * i as f32);
            egui::Area::new(egui::Id::new((salt, "toast", i)))
                .anchor(anchor, offset)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(340.0);
                        theme::row(ui, tx.rtl(), |ui| {
                            let (icon, color, key) = match t.kind {
                                Kind::Sent => (Icon::Upload, theme::success(dark), S::ToastSent),
                                Kind::Received => {
                                    (Icon::Download, theme::success(dark), S::ToastReceived)
                                }
                                Kind::Failed => {
                                    (Icon::Warning, theme::danger(dark), S::ToastFailed)
                                }
                            };
                            icons::icon(ui, tx, icon, icons::BUTTON_ICON, color);
                            ui.label(
                                RichText::new(tx.f(key, &[("label", &tx.d(&t.label))])).strong(),
                            );
                        });
                        if !t.who.is_empty() {
                            ui.label(theme::caption(ui, &tx.d(&t.who)));
                        }
                        theme::row(ui, tx.rtl(), |ui| {
                            if let Some(dir) = &t.folder {
                                if ui.button(tx.t(S::OpenFolder)).clicked() {
                                    actions.push(QueueAction::OpenFolder(dir.clone()));
                                    dismiss = Some(i);
                                }
                            }
                            if icons::icon_button(ui, tx, Icon::Close, tx.t(S::CloseTransfers))
                                .clicked()
                            {
                                dismiss = Some(i);
                            }
                        });
                    });
                });
        }
        if let Some(i) = dismiss {
            self.list.remove(i);
        }
        if !self.list.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::files::{FsError, TransferId};

    fn view(dir: TransferDir, status: TransferStatus) -> TransferView {
        TransferView {
            id: TransferId(1),
            dir,
            label: "تقرير.pdf".into(),
            status,
            bytes_done: 1,
            total_bytes: 1,
            files_done: 1,
            total_files: 1,
            failed_files: 0,
            bytes_per_sec: 0,
            local_dest: Some(PathBuf::from("C:\\وارد")),
            local: false,
            started_unix_ms: 0,
        }
    }

    #[test]
    fn notices_only_for_the_moment_a_transfer_ends() {
        let mut t = Toasts::default();
        let now = Instant::now();
        let running = view(TransferDir::Receive, TransferStatus::Running);
        assert!(!t.on_update(&running, "x", false, now));
        let done = view(TransferDir::Receive, TransferStatus::Completed);
        assert!(t.on_update(&done, "حاسوب سارة", false, now));
        assert!(
            !t.on_update(&done, "x", true, now),
            "already finished before"
        );
        assert_eq!(t.len(), 1);
        assert_eq!(
            t.list[0].folder.as_deref(),
            Some(std::path::Path::new("C:\\وارد"))
        );
    }

    #[test]
    fn sent_files_have_no_folder_and_cancels_and_pauses_are_silent() {
        let mut t = Toasts::default();
        let now = Instant::now();
        assert!(t.on_update(
            &view(TransferDir::Send, TransferStatus::Completed),
            "",
            false,
            now
        ));
        assert_eq!(t.list[0].folder, None);
        assert!(!t.on_update(
            &view(TransferDir::Send, TransferStatus::Cancelled),
            "",
            false,
            now
        ));
        assert!(!t.on_update(
            &view(TransferDir::Send, TransferStatus::Paused),
            "",
            false,
            now
        ));
        assert!(t.on_update(
            &view(TransferDir::Send, TransferStatus::Failed(FsError::NoSpace)),
            "",
            false,
            now
        ));
    }

    #[test]
    fn at_most_three_notices_stay() {
        let mut t = Toasts::default();
        let now = Instant::now();
        for _ in 0..6 {
            t.on_update(
                &view(TransferDir::Send, TransferStatus::Completed),
                "",
                false,
                now,
            );
        }
        assert_eq!(t.len(), MAX);
        assert!(!t.is_empty());
    }
}
