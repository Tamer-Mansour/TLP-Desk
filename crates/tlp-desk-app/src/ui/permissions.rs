//! macOS permission guidance: a card on the home screen that explains the missing Screen
//! Recording / Accessibility permissions and opens the right System Settings pane.
//!
//! It never appears on Windows (every permission reports "not applicable"). The state is re-read
//! every couple of seconds so the card disappears by itself once the user ticks the box.

use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use tlp_desk_input::permissions::{self, HostPermissions, Permission};

use super::theme;
use crate::i18n::{Tx, S};

/// How often the OS state is re-read while the card is visible.
const RECHECK: Duration = Duration::from_secs(2);

/// Home-screen permission card state.
#[derive(Debug)]
pub struct PermissionsUi {
    state: HostPermissions,
    checked: Instant,
}

impl Default for PermissionsUi {
    fn default() -> Self {
        Self::new()
    }
}

/// The strings (title key, explanation key) that describe a missing permission.
pub fn texts(permission: Permission) -> (S, S) {
    match permission {
        Permission::ScreenRecording => (S::PermScreenTitle, S::PermScreenBody),
        Permission::Accessibility => (S::PermAccessTitle, S::PermAccessBody),
    }
}

impl PermissionsUi {
    /// Reads the OS state now.
    pub fn new() -> Self {
        Self {
            state: permissions::host_permissions(),
            checked: Instant::now(),
        }
    }

    /// Whether the card has anything to say.
    pub fn visible(&self) -> bool {
        !self.state.all_granted()
    }

    fn refresh(&mut self, now: Instant) {
        if now.saturating_duration_since(self.checked) >= RECHECK {
            self.state = permissions::host_permissions();
            self.checked = now;
        }
    }

    /// Draws the card above the home columns when something is missing.
    pub fn card(&mut self, ui: &mut egui::Ui, tx: Tx) {
        self.refresh(Instant::now());
        let missing = self.state.missing();
        if missing.is_empty() {
            return;
        }
        let warn = ui.visuals().warn_fg_color;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(tx.t(S::PermHeading)).strong().color(warn));
            ui.label(tx.t(S::PermIntro));
            for permission in missing {
                let (title, body) = texts(permission);
                ui.add_space(6.0);
                ui.label(RichText::new(tx.t(title)).strong());
                ui.label(tx.t(body));
                theme::row(ui, tx.rtl(), |ui| {
                    if ui.button(tx.t(S::PermOpenSettings)).clicked() {
                        // Ask macOS to list the app first (so it appears in the pane), then
                        // open the pane; failures only mean the user opens Settings by hand.
                        let _ = permissions::request(permission);
                        let _ = permissions::open_settings(permission);
                    }
                });
            }
            ui.add_space(4.0);
            ui.label(RichText::new(tx.t(S::PermRestartHint)).weak());
        });
        ui.add_space(10.0);
        ui.ctx().request_repaint_after(RECHECK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

    #[test]
    fn card_is_hidden_when_nothing_is_required() {
        // Windows and CI Linux: every permission is "not applicable".
        #[cfg(not(target_os = "macos"))]
        assert!(!PermissionsUi::new().visible());
    }

    #[test]
    fn both_permissions_have_arabic_and_english_text() {
        for p in [Permission::ScreenRecording, Permission::Accessibility] {
            let (title, body) = texts(p);
            for lang in Lang::ALL {
                assert!(!crate::i18n::tr(lang, title).is_empty());
                assert!(!crate::i18n::tr(lang, body).is_empty());
            }
        }
    }
}
