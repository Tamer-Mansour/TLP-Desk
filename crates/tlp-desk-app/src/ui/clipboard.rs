//! Clipboard sync UI (TLP-clipboard): the "Sync clipboard" toggle in the session toolbar, the
//! brief "Clipboard sent/received" toast, the indicator in the host banner and the settings
//! card. Only kinds of events are shown, never clipboard contents.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, RichText};
use tlp_desk_files::{ClipboardEvent, Direction, RefusedReason};
use tlp_desk_proto::Permissions;

use super::icons::{self, Icon};
use super::{theme, widgets};
use crate::app::App;
use crate::backend::{Backend, SessionId};
use crate::i18n::{Tx, S};
use crate::settings::Settings;

/// How long the toast (viewer) and the banner indicator (host) stay visible.
pub const SHOW_FOR: Duration = Duration::from_millis(2500);

/// The message for a clipboard event.
pub fn event_text(ev: &ClipboardEvent) -> S {
    match ev {
        ClipboardEvent::Synced {
            direction: Direction::Sent,
            ..
        } => S::ClipboardSent,
        ClipboardEvent::Synced {
            direction: Direction::Received,
            ..
        } => S::ClipboardReceived,
        ClipboardEvent::Refused {
            reason: RefusedReason::TooLarge,
            ..
        } => S::ClipboardTooLarge,
        ClipboardEvent::Refused { .. } => S::ClipboardFailed,
    }
}

/// Clipboard state of one viewer session.
#[derive(Debug, Clone)]
pub struct ClipboardUi {
    /// The "Sync clipboard" switch.
    pub sync: bool,
    toast: Option<(S, Instant)>,
}

impl Default for ClipboardUi {
    fn default() -> Self {
        Self::new(true)
    }
}

impl ClipboardUi {
    pub fn new(sync: bool) -> Self {
        Self { sync, toast: None }
    }

    /// Shows the toast for `ev`.
    pub fn on_event(&mut self, ev: &ClipboardEvent) {
        self.toast = Some((event_text(ev), Instant::now()));
    }

    /// The current toast text, if one is showing.
    pub fn toast(&self) -> Option<S> {
        self.toast
            .filter(|(_, at)| at.elapsed() < SHOW_FOR)
            .map(|(k, _)| k)
    }

    /// Toolbar toggle (disabled without `CLIPBOARD`).
    pub fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        tx: Tx,
        session: SessionId,
        permissions: Permissions,
        backend: &dyn Backend,
    ) {
        let allowed = permissions.contains(Permissions::CLIPBOARD);
        let resp = icons::icon_toggle_enabled(
            ui,
            tx,
            allowed,
            self.sync && allowed,
            Icon::Clipboard,
            tx.t(S::ClipboardSync),
        );
        let resp = if allowed {
            resp
        } else {
            resp.on_disabled_hover_text(tx.t(S::ClipboardNotAllowed))
        };
        if resp.clicked() {
            self.sync = !self.sync;
            backend.set_clipboard_sync(session, self.sync);
        }
    }

    /// The toast overlay; call every frame, also while the toggle sits in the "More" menu.
    pub fn show_toast(&mut self, ctx: &egui::Context, tx: Tx) {
        let Some((key, at)) = self.toast else {
            return;
        };
        let age = at.elapsed();
        if age >= SHOW_FOR {
            self.toast = None;
            return;
        }
        egui::Area::new(egui::Id::new("clipboard-toast"))
            .anchor(Align2::CENTER_BOTTOM, egui::vec2(0.0, -28.0))
            .interactable(false)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    theme::row(ui, tx.rtl(), |ui| {
                        let color = ui.visuals().text_color();
                        icons::icon(ui, tx, Icon::Clipboard, icons::BUTTON_ICON, color);
                        ui.label(tx.t(key));
                    });
                });
            });
        ctx.request_repaint_after(SHOW_FOR.saturating_sub(age));
    }
}

/// Recent clipboard activity of host sessions, for the banner indicator.
#[derive(Debug, Default)]
pub struct HostClipboard {
    last: HashMap<SessionId, (S, Instant)>,
}

impl HostClipboard {
    pub fn on_event(&mut self, session: SessionId, ev: &ClipboardEvent) {
        self.last.insert(session, (event_text(ev), Instant::now()));
    }

    /// The indicator text for `session`, while recent.
    pub fn recent(&self, session: SessionId) -> Option<S> {
        self.last
            .get(&session)
            .filter(|(_, at)| at.elapsed() < SHOW_FOR)
            .map(|(k, _)| *k)
    }

    /// Icon + text in the host banner row while there was recent activity.
    pub fn indicator(&self, ui: &mut egui::Ui, tx: Tx, session: SessionId) {
        let Some((key, at)) = self.last.get(&session) else {
            return;
        };
        let age = at.elapsed();
        if age >= SHOW_FOR {
            return;
        }
        icons::icon(ui, tx, Icon::Clipboard, 16.0, Color32::WHITE);
        ui.label(RichText::new(tx.t(*key)).color(Color32::WHITE).small());
        ui.ctx().request_repaint_after(SHOW_FOR.saturating_sub(age));
    }
}

/// Settings card: default of the "Sync clipboard" switch for new sessions.
pub fn settings_card(ui: &mut egui::Ui, tx: Tx, settings: &mut Settings, backend: &dyn Backend) {
    theme::card(ui, |ui| {
        theme::column(ui, tx.rtl(), |ui| {
            widgets::icon_heading(ui, tx, Icon::Clipboard, tx.t(S::ClipboardSettings));
            ui.add_space(6.0);
            let mut on = !settings.clipboard_sync_off;
            if super::settings::check_row(ui, tx, &mut on, tx.t(S::ClipboardSyncDefault)) {
                settings.clipboard_sync_off = !on;
                settings.save();
                backend.set_clipboard_default(on);
            }
            ui.label(theme::caption(ui, tx.t(S::ClipboardSyncHint)));
        });
    });
}

impl App {
    /// Routes a clipboard event to the viewer session's toast or the host banner indicator.
    pub(crate) fn clipboard_event(&mut self, session: SessionId, event: ClipboardEvent) {
        match self.session.as_mut().filter(|s| s.id == session) {
            Some(s) => s.clipboard.on_event(&event),
            None => self.host_clipboard.on_event(session, &event),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tlp_desk_files::ClipKind;

    #[test]
    fn events_map_to_messages_in_both_languages() {
        let sent = ClipboardEvent::Synced {
            direction: Direction::Sent,
            kind: ClipKind::Text,
            bytes: 1,
        };
        let big = ClipboardEvent::Refused {
            direction: Direction::Received,
            kind: ClipKind::Image,
            reason: RefusedReason::TooLarge,
        };
        assert_eq!(event_text(&sent), S::ClipboardSent);
        assert_eq!(event_text(&big), S::ClipboardTooLarge);
        for lang in crate::i18n::Lang::ALL {
            let tx = Tx::new(lang, crate::i18n::DigitStyle::Western);
            assert!(!tx.raw(S::ClipboardSent).is_empty());
            assert!(!tx.raw(S::ClipboardReceived).is_empty());
        }
        let ar = Tx::new(crate::i18n::Lang::Ar, crate::i18n::DigitStyle::Western);
        assert_eq!(ar.raw(S::ClipboardSync), "مزامنة الحافظة");
    }

    #[test]
    fn toast_and_indicator_expire() {
        let mut ui = ClipboardUi::new(true);
        assert_eq!(ui.toast(), None);
        ui.on_event(&ClipboardEvent::Synced {
            direction: Direction::Received,
            kind: ClipKind::Html,
            bytes: 9,
        });
        assert_eq!(ui.toast(), Some(S::ClipboardReceived));
        ui.toast = Some((S::ClipboardReceived, Instant::now() - SHOW_FOR));
        assert_eq!(ui.toast(), None);

        let mut host = HostClipboard::default();
        host.on_event(
            SessionId(4),
            &ClipboardEvent::Refused {
                direction: Direction::Sent,
                kind: ClipKind::Text,
                reason: RefusedReason::ClipboardError,
            },
        );
        assert_eq!(host.recent(SessionId(4)), Some(S::ClipboardFailed));
        assert_eq!(host.recent(SessionId(5)), None);
    }
}
