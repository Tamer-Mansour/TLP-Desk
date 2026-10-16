//! TLP-service lane UI: the "remote PC is locked / on a secure screen" notice, and the home-screen note that the installed service
//! hosts this device.

use eframe::egui::{self, Color32, RichText};

use crate::backend::service::RemoteDesktop;
use crate::i18n::{Tx, S};
use crate::ui::icons::{self, Icon};
use crate::ui::theme;

/// The notice text for a desktop state (`None` = nothing to say).
pub fn desktop_notice(state: RemoteDesktop) -> Option<S> {
    if state.locked {
        Some(S::RemoteLocked)
    } else if state.secure_desktop {
        Some(S::RemoteSecureDesktop)
    } else {
        None
    }
}

fn info_colors(dark: bool) -> (Color32, Color32) {
    if dark {
        (
            Color32::from_rgb(0x12, 0x2B, 0x45),
            Color32::from_rgb(0xC9, 0xE2, 0xFF),
        )
    } else {
        (
            Color32::from_rgb(0xE3, 0xF0, 0xFF),
            Color32::from_rgb(0x0B, 0x35, 0x63),
        )
    }
}

/// Banner under the session toolbar while the host is locked or on a secure desktop.
pub fn desktop_banner(ui: &mut egui::Ui, tx: Tx, state: RemoteDesktop) {
    let Some(key) = desktop_notice(state) else {
        return;
    };
    let (fill, text) = info_colors(ui.visuals().dark_mode);
    egui::Panel::top("remote-desktop-banner")
        .frame(
            egui::Frame::new()
                .fill(fill)
                .inner_margin(egui::Margin::symmetric(14, 6)),
        )
        .show(ui, |ui| {
            theme::row(ui, tx.rtl(), |ui| {
                icons::icon(ui, tx, Icon::Lock, icons::BUTTON_ICON, text);
                ui.label(RichText::new(tx.t(key)).color(text));
            });
        });
}

/// Home-screen note: the installed service receives connections for this device.
pub fn hosting_banner(ui: &mut egui::Ui, tx: Tx) {
    let (fill, text) = info_colors(ui.visuals().dark_mode);
    egui::Panel::top("service-hosting")
        .frame(
            egui::Frame::new()
                .fill(fill)
                .inner_margin(egui::Margin::symmetric(16, 8)),
        )
        .show(ui, |ui| {
            theme::row(ui, tx.rtl(), |ui| {
                icons::icon(ui, tx, Icon::ShieldCheck, 18.0, text);
                ui.label(RichText::new(tx.t(S::ServiceHosting)).color(text));
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{tr_raw, Lang};

    #[test]
    fn notice_prefers_locked_over_secure() {
        assert_eq!(desktop_notice(RemoteDesktop::default()), None);
        assert_eq!(
            desktop_notice(RemoteDesktop {
                secure_desktop: true,
                locked: false
            }),
            Some(S::RemoteSecureDesktop)
        );
        assert_eq!(
            desktop_notice(RemoteDesktop {
                secure_desktop: true,
                locked: true
            }),
            Some(S::RemoteLocked)
        );
    }

    #[test]
    fn service_strings_exist_in_both_languages() {
        for key in [
            S::ServiceDisplayName,
            S::ServiceDescription,
            S::ServiceHosting,
            S::RemoteLocked,
            S::RemoteSecureDesktop,
        ] {
            assert!(!tr_raw(Lang::En, key).is_empty());
            assert!(!tr_raw(Lang::Ar, key).is_empty());
        }
        // The product name stays in Latin letters in Arabic.
        assert!(tr_raw(Lang::Ar, S::ServiceDisplayName).contains("TLP-Desk"));
        assert!(tr_raw(Lang::Ar, S::RemoteLocked).contains("Ctrl+Alt+Del"));
    }
}
