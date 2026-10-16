//! Host banner controls for several viewers at once (TLP-multiviewer): per connected viewer,
//! **Pause input** (mute that viewer's mouse and keyboard, permissions unchanged) and
//! **View only** (take mouse and keyboard away). The other viewers keep control.

use std::collections::HashSet;

use eframe::egui::{self, Color32, RichText};

use crate::backend::{Backend, SessionId};
use crate::i18n::{Tx, S};

/// Which viewers the host user paused or made view-only from the banner.
#[derive(Debug, Default)]
pub struct HostInputControls {
    paused: HashSet<SessionId>,
    view_only: HashSet<SessionId>,
}

impl HostInputControls {
    /// Whether `session`'s input is paused from the banner.
    pub fn is_paused(&self, session: SessionId) -> bool {
        self.paused.contains(&session)
    }

    /// Whether `session` was made view-only from the banner.
    pub fn is_view_only(&self, session: SessionId) -> bool {
        self.view_only.contains(&session)
    }

    /// Forget sessions that are gone.
    pub fn retain(&mut self, alive: impl Fn(SessionId) -> bool) {
        self.paused.retain(|s| alive(*s));
        self.view_only.retain(|s| alive(*s));
    }

    /// The two toggles for one banner row (white text on the accent strip).
    pub fn controls(
        &mut self,
        ui: &mut egui::Ui,
        tx: Tx,
        backend: &dyn Backend,
        session: SessionId,
    ) {
        let mut view_only = self.is_view_only(session);
        let label = RichText::new(tx.t(S::HostViewOnly)).color(Color32::WHITE);
        if ui
            .checkbox(&mut view_only, label)
            .on_hover_text(tx.t(S::HostViewOnlyHint))
            .changed()
        {
            self.set_view_only(backend, session, view_only);
        }
        let mut paused = self.is_paused(session);
        let label = RichText::new(tx.t(S::HostPauseInput)).color(Color32::WHITE);
        if ui
            .checkbox(&mut paused, label)
            .on_hover_text(tx.t(S::HostPauseInputHint))
            .changed()
        {
            self.set_paused(backend, session, paused);
        }
    }

    fn set_paused(&mut self, backend: &dyn Backend, session: SessionId, paused: bool) {
        if paused {
            self.paused.insert(session);
        } else {
            self.paused.remove(&session);
        }
        backend.host_set_input_paused(session, paused);
    }

    fn set_view_only(&mut self, backend: &dyn Backend, session: SessionId, on: bool) {
        if on {
            self.view_only.insert(session);
        } else {
            self.view_only.remove(&session);
        }
        backend.host_set_view_only(session, on);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{AttemptId, ConnectTarget, DeviceId, LocalInfo, RequestId};
    use crate::i18n::{DigitStyle, Lang};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Rec {
        paused: Mutex<Vec<(SessionId, bool)>>,
        view_only: Mutex<Vec<(SessionId, bool)>>,
    }

    impl Backend for Rec {
        fn local_info(&self) -> LocalInfo {
            LocalInfo {
                device_id: DeviceId(1),
                device_name: String::new(),
                addresses: vec![],
                one_time_code: String::new(),
            }
        }
        fn set_device_name(&self, _: String) {}
        fn refresh_one_time_code(&self) {}
        fn connect(&self, _: ConnectTarget, _: Option<String>) -> AttemptId {
            AttemptId(0)
        }
        fn cancel_connect(&self, _: AttemptId) {}
        fn forget_host_key(&self, _: AttemptId) -> bool {
            false
        }
        fn respond_to_request(&self, _: RequestId, _: bool) {}
        fn send_input(&self, _: SessionId, _: tlp_desk_proto::InputEvent) {}
        fn set_view_only(&self, _: SessionId, _: bool) {}
        fn disconnect(&self, _: SessionId) {}
        fn select_monitor(&self, _: SessionId, _: u32) {}
        fn host_set_input_paused(&self, session: SessionId, paused: bool) {
            self.paused.lock().unwrap().push((session, paused));
        }
        fn host_set_view_only(&self, session: SessionId, view_only: bool) {
            self.view_only.lock().unwrap().push((session, view_only));
        }
    }

    #[test]
    fn toggles_are_per_viewer_and_reach_the_backend() {
        let rec = Rec::default();
        let mut c = HostInputControls::default();
        let (a, b) = (SessionId(1), SessionId(2));
        c.set_paused(&rec, a, true);
        c.set_view_only(&rec, b, true);
        assert!(c.is_paused(a) && !c.is_paused(b));
        assert!(c.is_view_only(b) && !c.is_view_only(a));
        c.set_paused(&rec, a, false);
        assert!(!c.is_paused(a));
        assert_eq!(*rec.paused.lock().unwrap(), vec![(a, true), (a, false)]);
        assert_eq!(*rec.view_only.lock().unwrap(), vec![(b, true)]);
        c.retain(|s| s == a);
        assert!(!c.is_view_only(b), "ended sessions are forgotten");
    }

    #[test]
    fn controls_render_in_arabic_and_english() {
        let rec = Rec::default();
        for lang in [Lang::Ar, Lang::En] {
            let tx = Tx::new(lang, DigitStyle::Western);
            assert!(!tx.t(S::HostPauseInput).is_empty());
            assert!(!tx.t(S::HostViewOnly).is_empty());
            let ctx = egui::Context::default();
            let mut c = HostInputControls::default();
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                c.controls(ui, tx, &rec, SessionId(7));
            });
            out.textures_delta.clear();
        }
        let ar = Tx::new(Lang::Ar, DigitStyle::Western);
        assert!(ar.t(S::HostPauseInput).contains("الإدخال"));
    }
}
