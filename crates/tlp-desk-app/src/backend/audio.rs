//! Audio lane glue (TLP-audio): registers the `tlp-desk-audio` features with the real
//! backend, maps their events to [`BackendEvent`]s, and simulates audio in the fake backend.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tlp_desk_audio::device::fake::{FakeCapture, FakePlayback, FakeSignal};
use tlp_desk_audio::{
    AudioCapture, AudioEvent, AudioHost, AudioPlayback, AudioViewer, SystemCapture, SystemPlayback,
    FEATURE_NAME,
};
use tlp_desk_net::{FeatureEvent, FeatureSet};

use super::{BackendEvent, SessionId};

/// A viewer session's sound state (toolbar toggle, volume).
pub use tlp_desk_audio::ViewerAudioState as AudioState;

/// The sound devices the real backend uses.
#[derive(Clone)]
pub struct AudioDevices {
    /// Host side: what viewers hear (loopback of the default output device).
    pub capture: Arc<dyn AudioCapture>,
    /// Viewer side: where the remote sound plays.
    pub playback: Arc<dyn AudioPlayback>,
}

impl Default for AudioDevices {
    /// The system's default output device, for both loopback capture and playback.
    fn default() -> Self {
        Self {
            capture: Arc::new(SystemCapture),
            playback: Arc::new(SystemPlayback),
        }
    }
}

impl AudioDevices {
    /// Silent in-memory devices: tests never record or play real sound.
    pub fn fake() -> Self {
        Self {
            capture: Arc::new(FakeCapture::new(48_000, 2, FakeSignal::Silence)),
            playback: Arc::new(FakePlayback::new(48_000, 2)),
        }
    }
}

/// The audio features of one backend: one host feature for all incoming sessions, one
/// viewer feature for all outgoing ones.
pub(crate) struct AudioBridge {
    host: Arc<AudioHost>,
    viewer: Arc<AudioViewer>,
}

impl AudioBridge {
    pub(crate) fn new(devices: &AudioDevices) -> Self {
        Self {
            host: Arc::new(AudioHost::new(devices.capture.clone())),
            // The setting is applied right after start (`Backend::set_audio_on_connect`).
            viewer: Arc::new(AudioViewer::new(devices.playback.clone(), true)),
        }
    }

    /// `set` plus the host audio feature.
    pub(crate) fn host_features(&self, set: FeatureSet) -> FeatureSet {
        set.with(self.host.clone())
    }

    /// `set` plus the viewer audio feature.
    pub(crate) fn viewer_features(&self, set: FeatureSet) -> FeatureSet {
        set.with(self.viewer.clone())
    }

    pub(crate) fn viewer(&self) -> &AudioViewer {
        &self.viewer
    }
}

/// The [`BackendEvent`] for an audio feature event of viewer session `session`, if it is one.
pub(crate) fn viewer_event(session: SessionId, event: &FeatureEvent) -> Option<BackendEvent> {
    if event.feature != FEATURE_NAME {
        return None;
    }
    match event.payload::<AudioEvent>()? {
        AudioEvent::Viewer(state) => Some(BackendEvent::AudioState {
            session,
            state: *state,
        }),
        AudioEvent::DeviceError(detail) => {
            tracing::warn!(session = session.0, %detail, "audio output unavailable");
            Some(BackendEvent::AudioDeviceFailed { session })
        }
        AudioEvent::HostStreaming(_) => None,
    }
}

/// Fake backend: per-session simulated audio state (the fake host always permits audio).
#[derive(Default)]
pub(crate) struct FakeAudio {
    sessions: Mutex<HashMap<SessionId, AudioState>>,
}

impl FakeAudio {
    /// Applies `f` to the session's state and returns the event to emit.
    pub(crate) fn update(
        &self,
        session: SessionId,
        f: impl FnOnce(&mut AudioState),
    ) -> BackendEvent {
        let mut map = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let state = map.entry(session).or_insert(AudioState {
            permitted: true,
            enabled: true,
            playing: true,
            volume: 1.0,
        });
        f(state);
        state.playing = state.enabled;
        BackendEvent::AudioState {
            session,
            state: *state,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tlp_desk_audio::ViewerAudioState;

    #[test]
    fn maps_only_audio_viewer_events() {
        let s = SessionId(3);
        let state = ViewerAudioState {
            permitted: true,
            enabled: true,
            playing: false,
            volume: 0.5,
        };
        let ev = FeatureEvent::new(FEATURE_NAME, 9, AudioEvent::Viewer(state));
        assert!(matches!(
            viewer_event(s, &ev),
            Some(BackendEvent::AudioState { session, state: st }) if session == s && st == state
        ));
        let ev = FeatureEvent::new(FEATURE_NAME, 9, AudioEvent::DeviceError("x".into()));
        assert!(matches!(
            viewer_event(s, &ev),
            Some(BackendEvent::AudioDeviceFailed { .. })
        ));
        let ev = FeatureEvent::new(FEATURE_NAME, 9, AudioEvent::HostStreaming(true));
        assert!(viewer_event(s, &ev).is_none());
        let ev = FeatureEvent::new("files", 9, AudioEvent::Viewer(state));
        assert!(viewer_event(s, &ev).is_none());
    }

    #[test]
    fn fake_audio_follows_the_toggle() {
        let fake = FakeAudio::default();
        let s = SessionId(1);
        let BackendEvent::AudioState { state, .. } = fake.update(s, |a| a.enabled = false) else {
            panic!("not an audio event");
        };
        assert!(!state.enabled && !state.playing && state.permitted);
        let BackendEvent::AudioState { state, .. } = fake.update(s, |a| a.volume = 0.3) else {
            panic!("not an audio event");
        };
        assert!(!state.enabled && (state.volume - 0.3).abs() < 1e-6);
    }
}
