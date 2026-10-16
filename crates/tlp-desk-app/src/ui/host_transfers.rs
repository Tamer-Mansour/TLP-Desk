//! File transfers as the host user sees them. A viewer that sends files to this PC or takes
//! files from it is never invisible: the host banner shows a "Files: n active" button, a
//! "File transfers" window lists every transfer with who is on the other side, progress and a
//! Cancel button, and a notice (with "Open folder") appears when files arrive.

use std::time::Instant;

use eframe::egui;

use super::icons::{self, Icon};
use super::transfer_queue::{Queue, QueueAction, QueueEnv};
use super::transfer_toast::Toasts;
use crate::backend::files::TransferView;
use crate::backend::Backend;
use crate::i18n::{format, ltr_isolate, Tx, S};

/// State of the host-side transfer list.
#[derive(Default)]
pub struct HostTransfers {
    /// All transfers of all connected viewers.
    pub queue: Queue,
    /// Notices for finished transfers.
    pub toasts: Toasts,
    /// Whether the window is shown.
    pub open: bool,
}

impl HostTransfers {
    /// Records the newest state of a transfer. `who` names the viewer behind it. Returns `true`
    /// when the host user should be alerted (a transfer started or ended).
    pub fn update(&mut self, view: TransferView, who: &str) -> bool {
        let was_finished = self
            .queue
            .entries()
            .iter()
            .find(|e| e.view.id == view.id)
            .is_some_and(|e| super::transfer_queue::finished(&e.view));
        let ended = self
            .toasts
            .on_update(&view, who, was_finished, Instant::now());
        let new = self.queue.update(view, who);
        if new {
            // Never hidden from the person at the keyboard.
            self.open = true;
        }
        new || ended
    }

    /// Whether the banner button is shown.
    pub fn has_activity(&self) -> bool {
        self.queue.active_count() > 0
    }

    /// The banner button: "Files: n active  ·  speed". `true` when clicked. Drawn in white on
    /// the banner's accent color.
    pub fn chip(&self, ui: &mut egui::Ui, tx: Tx) -> bool {
        if !self.has_activity() {
            return false;
        }
        let n = tx.digits(&self.queue.active_count().to_string());
        let speed = ltr_isolate(&format::file_size(
            self.queue.total_rate(),
            tx.lang,
            tx.digits,
        ));
        let text = format!(
            "{}  ·  {}",
            tx.raw(S::HostTqChip).replace("{n}", &n),
            tx.raw(S::TransferRate).replace("{size}", &speed)
        );
        ui.add(icons::icon_text_button(tx, Icon::Download, tx.p(&text)))
            .clicked()
    }

    /// Draws the window and the notices; returns what the host user asked for.
    pub fn show(&mut self, ctx: &egui::Context, tx: Tx, backend: &dyn Backend) -> Vec<QueueAction> {
        let mut actions = self.toasts.show(ctx, tx, "host-transfers");
        if !self.open {
            return actions;
        }
        let env = QueueEnv {
            backend,
            session_peer: None,
            session_peer_name: "",
            can_start: false,
        };
        let mut open = self.open;
        egui::Window::new(tx.t(S::HostTqTitle))
            .id(egui::Id::new("host-transfers-window"))
            .open(&mut open)
            .default_width(520.0)
            .collapsible(false)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(380.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| actions.extend(self.queue.ui(ui, tx, &env)));
            });
        self.open = open;
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::files::{TransferDir, TransferId, TransferStatus};

    fn view(id: u64, status: TransferStatus) -> TransferView {
        TransferView {
            id: TransferId(id),
            dir: TransferDir::Receive,
            label: "تقرير.pdf".into(),
            status,
            bytes_done: 5,
            total_bytes: 10,
            files_done: 0,
            total_files: 1,
            failed_files: 0,
            bytes_per_sec: 1000,
            local_dest: None,
            local: false,
            started_unix_ms: 1,
        }
    }

    #[test]
    fn an_incoming_transfer_opens_the_window_and_alerts_the_host_user() {
        let mut h = HostTransfers::default();
        assert!(!h.open && !h.has_activity());
        assert!(h.update(view(1, TransferStatus::Running), "حاسوب سارة"));
        assert!(h.open, "never hidden from the person at the keyboard");
        assert!(h.has_activity());
        assert_eq!(h.queue.entries()[0].who, "حاسوب سارة");
        // Progress updates do not alert again.
        assert!(!h.update(view(1, TransferStatus::Running), "حاسوب سارة"));
        h.open = false;
        assert!(h.update(view(1, TransferStatus::Completed), "حاسوب سارة"));
        assert!(!h.has_activity());
        assert_eq!(h.toasts.len(), 1, "a notice that files arrived");
        assert!(
            !h.open,
            "finishing does not reopen a window the user closed"
        );
    }

    #[test]
    fn a_cancelled_transfer_is_silent_but_stays_listed() {
        let mut h = HostTransfers::default();
        h.update(view(2, TransferStatus::Running), "x");
        assert!(!h.update(view(2, TransferStatus::Cancelled), "x"));
        assert_eq!(h.queue.entries().len(), 1);
        assert!(h.toasts.is_empty());
    }
}
