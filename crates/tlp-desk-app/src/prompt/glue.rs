//! App side of the Accept window: open a popup for each incoming request, apply its answer,
//! close it when the viewer gives up, and draw the same card inside the main window when the
//! popup is switched off or could not start.

use eframe::egui;

use super::launcher::{AcceptPopups, PopupResult, ProcessSpawner};
use super::protocol::{Outcome, PromptAuth, PromptRequest, PromptTheme};
use crate::app::App;
use crate::backend::{PeerInfo, RequestId};
use crate::settings::host::PermissionSet;
use crate::ui::accept_window::AcceptCard;

impl App {
    /// Let incoming requests open the stand-alone Accept window (only the real GUI does;
    /// tests and screenshots keep the in-app dialog).
    pub fn enable_accept_popup(&mut self, ctx: &egui::Context) {
        let ctx = ctx.clone();
        let repaint: crate::backend::RepaintFn = std::sync::Arc::new(move || ctx.request_repaint());
        match ProcessSpawner::current(repaint) {
            Ok(s) => self.accept_popups = AcceptPopups::new(Box::new(s)),
            Err(e) => tracing::warn!(%e, "accept popup unavailable: cannot find this executable"),
        }
    }

    /// What the popup shows for `peer`.
    pub fn prompt_request(&self, ctx: &egui::Context, peer: &PeerInfo) -> PromptRequest {
        PromptRequest {
            peer_name: peer.name.clone(),
            device_id: peer.device_id.0,
            address: peer.address.to_string(),
            auth: PromptAuth::AcceptPrompt,
            permissions: self.settings.host.default_permissions,
            timeout_secs: self.settings.host.accept_timeout_secs,
            lang: self.settings.lang,
            digits: self.settings.digits,
            theme: match ctx.options(|o| o.theme_preference) {
                egui::ThemePreference::Dark => PromptTheme::Dark,
                egui::ThemePreference::Light => PromptTheme::Light,
                egui::ThemePreference::System => PromptTheme::System,
            },
            sound: self.settings.accept_popup.sound,
            permissions_locked: false,
        }
    }

    /// A request arrived: show it in a popup if enabled. `false` = use the in-app dialog.
    pub(crate) fn popup_incoming(
        &mut self,
        ctx: &egui::Context,
        request: RequestId,
        peer: &PeerInfo,
    ) -> bool {
        if !self.settings.accept_popup.enabled || !self.accept_popups.is_enabled() {
            return false;
        }
        let mut prompt = self.prompt_request(ctx, peer);
        prompt.permissions_locked = !self.backend.can_choose_permissions(request);
        self.accept_popups.open(request, &prompt)
    }

    /// The request is gone (viewer gave up, or the host timed it out).
    pub(crate) fn popup_cancelled(&mut self, request: RequestId) {
        self.accept_popups.cancel(request);
        if self
            .accept_card
            .as_ref()
            .is_some_and(|(r, _)| *r == request)
        {
            self.accept_card = None;
        }
    }

    /// Once per frame: apply popup answers; fall back to the in-app dialog for crashed ones.
    pub(crate) fn popup_frame(&mut self, ctx: &egui::Context) {
        for (request, result) in self.accept_popups.poll() {
            match result {
                PopupResult::Decided(d) => {
                    if d.outcome != Outcome::Cancelled {
                        self.answer_request(request, d.outcome, d.permissions, d.trust);
                    }
                }
                PopupResult::Failed => {
                    let name = self
                        .incoming
                        .iter()
                        .find(|(r, _)| *r == request)
                        .map(|(_, p)| p.name.clone());
                    if let Some(name) = name {
                        tracing::warn!("accept popup closed without an answer; asking in the app");
                        self.notify_incoming(ctx, &name);
                    }
                }
            }
        }
    }

    /// Send the decision to the backend and forget the request.
    fn answer_request(
        &mut self,
        request: RequestId,
        outcome: Outcome,
        permissions: PermissionSet,
        trust: bool,
    ) {
        let Some(pos) = self.incoming.iter().position(|(r, _)| *r == request) else {
            return;
        };
        let peer = self.incoming.remove(pos).map(|(_, p)| p);
        let accept = outcome == Outcome::Accept;
        tracing::info!(?outcome, "accept prompt answered");
        self.backend
            .respond_to_request_with(request, accept, accept.then_some(permissions.0));
        if let (true, Some(peer)) = (trust && accept, peer) {
            self.settings.accept_popup.trust(peer.device_id.0);
            // Unit tests must never touch the user's real settings.json.
            #[cfg(not(test))]
            self.settings.save();
        }
        if self
            .accept_card
            .as_ref()
            .is_some_and(|(r, _)| *r == request)
        {
            self.accept_card = None;
        }
    }

    /// The in-app Accept dialog for the first request not shown in a popup. Returns whether
    /// a dialog is open.
    pub(crate) fn incoming_dialog(&mut self, ctx: &egui::Context) -> bool {
        let Some((request, peer)) = self
            .incoming
            .iter()
            .find(|(r, _)| !self.accept_popups.showing(*r))
            .cloned()
        else {
            self.accept_card = None;
            return false;
        };
        if self.accept_card.as_ref().is_none_or(|(r, _)| *r != request) {
            let mut prompt = self.prompt_request(ctx, &peer);
            prompt.sound = false;
            prompt.permissions_locked = !self.backend.can_choose_permissions(request);
            self.accept_card = Some((request, AcceptCard::new(prompt)));
        }
        let tx = self.tx;
        let mut answer = None;
        let frame = egui::Frame::popup(&ctx.global_style())
            .inner_margin(0)
            .corner_radius(16);
        egui::Modal::new(egui::Id::new("incoming-request"))
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_width(420.0);
                if let Some((_, card)) = self.accept_card.as_mut() {
                    answer = card.show(ui, tx, 16);
                }
            });
        if let Some(outcome) = answer {
            let (perms, trust) = self
                .accept_card
                .as_ref()
                .map(|(_, c)| {
                    let d = c.decision(outcome);
                    (d.permissions, d.trust)
                })
                .unwrap_or_default();
            self.answer_request(request, outcome, perms, trust);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::launcher::fake::{FakeLog, FakeSpawner};
    use super::super::launcher::PromptPoll;
    use super::super::protocol::PromptDecision;
    use super::*;
    use crate::app::StartOptions;
    use crate::backend::DeviceId;
    use crate::settings::Settings;
    use tlp_desk_proto::Permissions;

    fn peer() -> PeerInfo {
        PeerInfo {
            device_id: DeviceId(950_442_118),
            name: "حاسوب-المختبر".into(),
            address: "192.168.1.81".parse().unwrap(),
        }
    }

    fn app_with_fake() -> (
        egui_kittest::Harness<'static, App>,
        Arc<Mutex<FakeLog>>,
        Arc<dyn crate::backend::Backend>,
        tokio::runtime::Runtime,
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (backend, events) =
            crate::backend::fake::FakeBackend::start(rt.handle(), Arc::new(|| {}));
        let log = Arc::new(Mutex::new(FakeLog::default()));
        let spawner_log = Arc::clone(&log);
        let b2 = Arc::clone(&backend);
        let h = egui_kittest::Harness::builder().build_eframe(move |cc| {
            let mut app = App::new(
                cc,
                b2,
                events,
                Settings::default(),
                StartOptions {
                    address_book_in_memory: true,
                    ..Default::default()
                },
            );
            app.accept_popups = AcceptPopups::new(Box::new(FakeSpawner(spawner_log)));
            app
        });
        (h, log, backend, rt)
    }

    #[test]
    fn request_opens_popup_and_its_answer_reaches_the_backend() {
        let (mut h, log, _backend, _rt) = app_with_fake();
        let ctx = h.ctx.clone();
        let opened = h.state_mut().popup_incoming(&ctx, RequestId(1), &peer());
        assert!(opened);
        h.state_mut().incoming.push_back((RequestId(1), peer()));
        {
            let l = log.lock().unwrap();
            assert_eq!(l.requests.len(), 1);
            assert_eq!(l.requests[0].peer_name, "حاسوب-المختبر");
            assert_eq!(l.requests[0].device_id, 950_442_118);
        }
        // While the popup is up, the main window shows no dialog for it.
        assert!(!h.state_mut().incoming_dialog(&ctx));
        let mut perms = PermissionSet(Permissions::VIEW);
        perms.set(Permissions::FILES, true);
        log.lock().unwrap().answer = Some(PromptPoll::Done(PromptDecision {
            outcome: Outcome::Accept,
            permissions: perms,
            trust: true,
        }));
        h.state_mut().popup_frame(&ctx);
        assert!(h.state().incoming.is_empty());
        assert_eq!(h.state().settings.accept_popup.trusted, vec![950_442_118]);
    }

    #[test]
    fn crashed_popup_falls_back_to_the_in_app_dialog() {
        let (mut h, log, _backend, _rt) = app_with_fake();
        let ctx = h.ctx.clone();
        assert!(h.state_mut().popup_incoming(&ctx, RequestId(2), &peer()));
        h.state_mut().incoming.push_back((RequestId(2), peer()));
        log.lock().unwrap().answer = Some(PromptPoll::Failed);
        h.state_mut().popup_frame(&ctx);
        assert_eq!(h.state().incoming.len(), 1, "still waiting for an answer");
        assert!(!h.state().accept_popups.showing(RequestId(2)));
    }

    #[test]
    fn viewer_giving_up_cancels_the_popup() {
        let (mut h, log, _backend, _rt) = app_with_fake();
        let ctx = h.ctx.clone();
        h.state_mut().popup_incoming(&ctx, RequestId(3), &peer());
        h.state_mut().popup_cancelled(RequestId(3));
        assert_eq!(log.lock().unwrap().cancelled, 1);
    }

    #[test]
    fn disabled_setting_keeps_the_in_app_dialog() {
        let (mut h, log, _backend, _rt) = app_with_fake();
        let ctx = h.ctx.clone();
        h.state_mut().settings.accept_popup.enabled = false;
        assert!(!h.state_mut().popup_incoming(&ctx, RequestId(4), &peer()));
        assert!(log.lock().unwrap().requests.is_empty());
    }
}
