//! The remote computer's sound (TLP-audio): session toolbar toggle and volume, and the
//! settings card.

use eframe::egui;
use tlp_desk_proto::Permissions;

use super::icons::{self, Icon};
use super::{theme, widgets};
use crate::backend::audio::AudioState;
use crate::backend::{Backend, SessionId};
use crate::i18n::{Tx, S};
use crate::settings::Settings;

/// Width of the toolbar volume slider, in points.
const SLIDER_WIDTH: f32 = 72.0;

/// The toolbar's view of one session's sound.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioControls {
    /// The user wants to hear the remote computer.
    pub enabled: bool,
    /// Sound is actually playing (the host agreed and the speakers are open).
    pub playing: bool,
    /// `0.0..=1.0`.
    pub volume: f32,
}

impl Default for AudioControls {
    fn default() -> Self {
        Self::new(true)
    }
}

impl AudioControls {
    /// Initial state for a new session (`enabled` = the "play sound when connecting" setting).
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            playing: false,
            volume: 1.0,
        }
    }

    /// Takes over the state the backend reported.
    pub fn apply(&mut self, state: AudioState) {
        self.enabled = state.enabled;
        self.playing = state.playing;
        self.volume = state.volume;
    }
}

/// Speaker toggle (and a volume slider while sound is on). Disabled, with an explanation,
/// while the host does not grant `AUDIO`.
pub fn toolbar(
    ui: &mut egui::Ui,
    tx: Tx,
    backend: &dyn Backend,
    session: SessionId,
    permissions: Permissions,
    controls: &mut AudioControls,
) {
    let permitted = permissions.contains(Permissions::AUDIO);
    let on = permitted && controls.enabled;
    let (icon, tip) = match (permitted, controls.enabled) {
        (false, _) => (Icon::Mute, S::AudioNotAllowed),
        (true, true) => (Icon::Volume, S::AudioMute),
        (true, false) => (Icon::Mute, S::AudioUnmute),
    };
    let resp = icons::icon_toggle_enabled(ui, tx, permitted, on, icon, tx.t(tip));
    let resp = if permitted {
        resp
    } else {
        resp.on_disabled_hover_text(tx.t(S::AudioNotAllowed))
    };
    if resp.clicked() {
        controls.enabled = !controls.enabled;
        backend.set_audio_enabled(session, controls.enabled);
    }
    if on {
        let mut volume = controls.volume;
        let slider = ui
            .scope(|ui| {
                ui.spacing_mut().slider_width = SLIDER_WIDTH;
                ui.add(egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false))
            })
            .inner
            .on_hover_text(tx.t(S::AudioVolume));
        if slider.changed() {
            controls.volume = volume;
            backend.set_audio_volume(session, volume);
        }
    }
}

/// Settings card: whether new sessions play the remote sound.
pub fn settings_card(ui: &mut egui::Ui, tx: Tx, settings: &mut Settings, backend: &dyn Backend) {
    theme::card(ui, |ui| {
        theme::column(ui, tx.rtl(), |ui| {
            widgets::icon_heading(ui, tx, Icon::Volume, tx.t(S::AudioSettings));
            ui.add_space(6.0);
            let mut on = !settings.mute_remote_audio;
            if ui.checkbox(&mut on, tx.t(S::AudioOnConnect)).changed() {
                settings.mute_remote_audio = !on;
                settings.save();
                backend.set_audio_on_connect(on);
            }
            ui.label(theme::caption(ui, tx.t(S::AudioOnConnectHint)));
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_follow_the_backend() {
        let mut c = AudioControls::new(false);
        assert!(!c.enabled && !c.playing);
        c.apply(AudioState {
            permitted: true,
            enabled: true,
            playing: true,
            volume: 0.4,
        });
        assert!(c.enabled && c.playing);
        assert!((c.volume - 0.4).abs() < 1e-6);
    }
}
