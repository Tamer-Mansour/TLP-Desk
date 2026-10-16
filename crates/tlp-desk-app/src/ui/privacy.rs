//! Privacy mode in the viewer session: toolbar toggle, confirmation dialog, "privacy mode is
//! on" banner, and a short notice when the host ended it (emergency exit, lock screen,
//! revoked permission) or could not turn it on.

use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Id, RichText};

use super::icons::{self, Icon};
use super::{theme, widgets};
use crate::i18n::{Tx, S};

/// How long the "privacy mode is off" notice stays.
const NOTICE_FOR: Duration = Duration::from_secs(6);
/// A request the host never answers (e.g. it dropped it for lack of permission) counts as
/// refused after this long.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);

/// Privacy state of one viewer session, as the UI sees it.
#[derive(Debug, Default)]
pub struct PrivacyUi {
    /// The host's actual state (last `PrivacyChanged`).
    on: bool,
    /// What we asked for and when; cleared by the host's answer.
    pending: Option<(bool, Instant)>,
    /// The confirmation dialog is open.
    confirming: bool,
    /// When privacy mode ended without us asking (or a request was refused).
    notice: Option<Instant>,
}

impl PrivacyUi {
    /// Whether the host screen is private.
    pub fn is_on(&self) -> bool {
        self.on
    }

    /// The confirmation dialog is open (remote input must pause).
    pub fn confirming(&self) -> bool {
        self.confirming
    }

    /// The host reported its actual state.
    pub fn on_state(&mut self, enabled: bool, now: Instant) {
        let asked_off = matches!(self.pending, Some((false, _)));
        let wanted_on = self.on || matches!(self.pending, Some((true, _)));
        if !enabled && wanted_on && !asked_off {
            self.notice = Some(now);
        }
        if enabled {
            self.notice = None;
        }
        self.on = enabled;
        self.pending = None;
    }

    /// The toolbar button was clicked. Returns the request to send, if any: turning off is
    /// immediate, turning on asks for confirmation first.
    pub fn toggle(&mut self, now: Instant) -> Option<bool> {
        if self.on || matches!(self.pending, Some((true, _))) {
            self.pending = Some((false, now));
            Some(false)
        } else {
            self.confirming = true;
            None
        }
    }

    /// The user answered the confirmation dialog. Returns the request to send, if any.
    pub fn confirm(&mut self, yes: bool, now: Instant) -> Option<bool> {
        self.confirming = false;
        yes.then(|| {
            self.pending = Some((true, now));
            self.notice = None;
            true
        })
    }

    /// `PRIVACY_MODE` was revoked: close the dialog (the host turns privacy off by itself and
    /// reports it).
    pub fn permission_lost(&mut self) {
        self.confirming = false;
    }

    /// Housekeeping once per frame: an unanswered request counts as refused.
    pub fn tick(&mut self, now: Instant) {
        if let Some((true, since)) = self.pending {
            if now.saturating_duration_since(since) >= ANSWER_TIMEOUT {
                self.pending = None;
                self.notice = Some(now);
            }
        }
    }

    /// Whether the "privacy mode is off" notice is showing.
    pub fn notice_visible(&self, now: Instant) -> bool {
        self.notice
            .is_some_and(|t| now.saturating_duration_since(t) < NOTICE_FOR)
    }

    /// Toolbar toggle. `allowed` = the host granted `PRIVACY_MODE`. Returns the request to
    /// send.
    pub fn toolbar(&mut self, ui: &mut egui::Ui, tx: Tx, allowed: bool) -> Option<bool> {
        let label = if self.on {
            tx.t(S::PrivacyTurnOff)
        } else {
            tx.t(S::PrivacyMode)
        };
        // Turning off always stays possible, even after a revoke.
        let enabled = allowed || self.on;
        let resp = icons::icon_toggle_enabled(ui, tx, enabled, self.on, Icon::Privacy, label);
        let resp = if enabled {
            resp
        } else {
            resp.on_disabled_hover_text(tx.t(S::PrivacyNotAllowed))
        };
        if resp.clicked() {
            self.toggle(Instant::now())
        } else {
            None
        }
    }

    /// Confirmation dialog (while [`confirming`](Self::confirming)). Returns the request to
    /// send.
    pub fn dialog(&mut self, ctx: &egui::Context, tx: Tx, peer_name: &str) -> Option<bool> {
        if !self.confirming {
            return None;
        }
        let mut answer = None;
        egui::Modal::new(Id::new("privacy-confirm")).show(ctx, |ui| {
            ui.set_width(520.0);
            theme::column(ui, tx.rtl(), |ui| {
                widgets::icon_heading(ui, tx, Icon::Privacy, tx.t(S::PrivacyConfirmTitle));
                ui.add_space(4.0);
                ui.label(tx.f(S::PrivacyConfirmScreen, &[("name", peer_name)]));
                ui.label(tx.t(S::PrivacyConfirmControl));
                ui.add_space(4.0);
                ui.label(theme::caption(ui, tx.t(S::PrivacyConfirmExit)));
                ui.add_space(10.0);
                theme::row(ui, tx.rtl(), |ui| {
                    let on =
                        widgets::primary_icon_button(ui, tx, Icon::Privacy, tx.t(S::PrivacyTurnOn));
                    if ui.add(on).clicked() {
                        answer = Some(true);
                    }
                    let cancel = icons::icon_text_button(tx, Icon::Close, tx.t(S::Cancel));
                    if ui.add(cancel).clicked() {
                        answer = Some(false);
                    }
                });
            });
        });
        answer.and_then(|yes| self.confirm(yes, Instant::now()))
    }

    /// "Privacy mode is on" banner, or the short "it ended" notice.
    pub fn banner(&mut self, ui: &mut egui::Ui, tx: Tx) {
        let now = Instant::now();
        self.tick(now);
        let text = if self.on {
            S::PrivacyOnBanner
        } else if self.notice_visible(now) {
            S::PrivacyEnded
        } else {
            return;
        };
        let dark = ui.visuals().dark_mode;
        let (fill, fg) = if dark {
            (
                Color32::from_rgb(0x1E, 0x2A, 0x44),
                Color32::from_rgb(0xC9, 0xD8, 0xFF),
            )
        } else {
            (
                Color32::from_rgb(0xE3, 0xEB, 0xFF),
                Color32::from_rgb(0x1F, 0x33, 0x66),
            )
        };
        egui::Panel::top("privacy-banner")
            .frame(
                egui::Frame::new()
                    .fill(fill)
                    .inner_margin(egui::Margin::symmetric(14, 6)),
            )
            .show(ui, |ui| {
                theme::row(ui, tx.rtl(), |ui| {
                    icons::icon(ui, tx, Icon::Privacy, icons::BUTTON_ICON, fg);
                    ui.label(RichText::new(tx.t(text)).color(fg));
                });
            });
        // The notice disappears on its own; keep repainting until then.
        ui.ctx().request_repaint_after(Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Clock(Instant);
    impl Clock {
        fn at(&self, ms: u64) -> Instant {
            self.0 + Duration::from_millis(ms)
        }
    }

    #[test]
    fn turning_on_needs_confirmation() {
        let c = Clock(Instant::now());
        let mut p = PrivacyUi::default();
        assert_eq!(p.toggle(c.at(0)), None);
        assert!(p.confirming());
        assert_eq!(p.confirm(false, c.at(1)), None);
        assert!(!p.confirming());
        assert_eq!(p.toggle(c.at(2)), None);
        assert_eq!(p.confirm(true, c.at(3)), Some(true));
        assert!(!p.is_on(), "on only once the host confirms");
        p.on_state(true, c.at(50));
        assert!(p.is_on());
        assert!(!p.notice_visible(c.at(51)));
    }

    #[test]
    fn turning_off_is_immediate_and_silent() {
        let c = Clock(Instant::now());
        let mut p = PrivacyUi::default();
        p.on_state(true, c.at(0));
        assert_eq!(p.toggle(c.at(10)), Some(false));
        assert!(!p.confirming());
        p.on_state(false, c.at(20));
        assert!(!p.is_on());
        assert!(!p.notice_visible(c.at(21)), "we asked for it: no notice");
    }

    #[test]
    fn host_ending_privacy_shows_a_notice() {
        let c = Clock(Instant::now());
        let mut p = PrivacyUi::default();
        p.on_state(true, c.at(0));
        // Emergency exit / lock screen / revoked permission on the host.
        p.on_state(false, c.at(1000));
        assert!(p.notice_visible(c.at(1001)));
        assert!(!p.notice_visible(c.at(1000 + NOTICE_FOR.as_millis() as u64)));
    }

    #[test]
    fn refused_or_unanswered_requests_show_a_notice() {
        let c = Clock(Instant::now());
        let mut p = PrivacyUi::default();
        p.toggle(c.at(0));
        p.confirm(true, c.at(0));
        p.on_state(false, c.at(30));
        assert!(p.notice_visible(c.at(31)));

        let mut p = PrivacyUi::default();
        p.toggle(c.at(0));
        p.confirm(true, c.at(0));
        p.tick(c.at(4999));
        assert!(!p.notice_visible(c.at(4999)));
        p.tick(c.at(5000));
        assert!(p.notice_visible(c.at(5001)));
        // A late "on" still wins.
        p.on_state(true, c.at(6000));
        assert!(p.is_on() && !p.notice_visible(c.at(6001)));
    }

    #[test]
    fn revoke_closes_the_dialog() {
        let mut p = PrivacyUi::default();
        p.toggle(Instant::now());
        p.permission_lost();
        assert!(!p.confirming());
    }
}
