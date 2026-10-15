//! Port forwarding UI (TLP-tunnel):
//!
//! * viewer side: a "Port forwarding" window opened from the session toolbar
//!   ([`TunnelsPanel`]): add a rule (local port -> remote address:port), list the active rules
//!   with connection counts and bytes, stop them;
//! * host side: a switch in the "who is connected" banner that allows or forbids port
//!   forwarding for that viewer, with the number of open forwarded connections
//!   ([`host_banner_controls`]).

use eframe::egui::{self, Color32, RichText};
use tlp_desk_proto::Permissions;

use super::icons::{self, Icon};
use super::{theme, widgets};
use crate::backend::tunnels::{HostTunnelRow, TunnelProblem, TunnelRequest, TunnelRow};
use crate::backend::{Backend, SessionId};
use crate::i18n::format::{file_size, to_western_digits};
use crate::i18n::{Tx, S};

/// Viewer-side state of the port forwarding window for one session.
#[derive(Debug, Default)]
pub struct TunnelsPanel {
    /// Whether the window is shown.
    pub open: bool,
    local_port: String,
    host: String,
    port: String,
    lan_bind: bool,
    /// Active rules (from `BackendEvent::Tunnels`).
    pub rules: Vec<TunnelRow>,
    /// Last problem to show (from `BackendEvent::TunnelProblem` or local validation).
    pub problem: Option<TunnelProblem>,
    /// Local validation error (bad port).
    invalid: bool,
}

/// Parses a port typed by the user (Arabic-Indic digits allowed); `None` if not 1..=65535.
pub fn parse_port(s: &str) -> Option<u16> {
    to_western_digits(s.trim())
        .parse::<u16>()
        .ok()
        .filter(|p| *p != 0)
}

/// Display text for a problem.
pub fn problem_text(tx: Tx, p: &TunnelProblem) -> String {
    match p {
        TunnelProblem::NotAllowed => tx.t(S::TunnelErrNotAllowed).into(),
        TunnelProblem::Revoked => tx.t(S::TunnelErrRevoked).into(),
        TunnelProblem::LocalPort(d) => tx.f(S::TunnelErrLocalPort, &[("detail", d)]),
        TunnelProblem::TargetNotAllowed => tx.t(S::TunnelErrTargetNotAllowed).into(),
        TunnelProblem::BadTarget => tx.t(S::TunnelErrBadTarget).into(),
        TunnelProblem::TooMany => tx.t(S::TunnelErrTooMany).into(),
        TunnelProblem::NoAnswer => tx.t(S::TunnelErrNoAnswer).into(),
        TunnelProblem::SessionEnded => tx.t(S::SessionEnded).into(),
        TunnelProblem::Other(d) => tx.f(S::TunnelErrOther, &[("detail", d)]),
    }
}

impl TunnelsPanel {
    /// The rule the form describes, or `None` (and the form shows why) if a port is invalid.
    /// An empty remote address means the remote PC itself.
    pub fn request(&self) -> Option<TunnelRequest> {
        let local_port = parse_port(&self.local_port)?;
        let target_port = parse_port(&self.port)?;
        let host = to_western_digits(self.host.trim());
        Some(TunnelRequest {
            local_port,
            lan_bind: self.lan_bind,
            target_host: (!host.is_empty()).then_some(host),
            target_port,
        })
    }

    /// Toolbar toggle that shows/hides the window.
    pub fn toolbar_button(&mut self, ui: &mut egui::Ui, tx: Tx) {
        let label = tx.t(S::PortForwarding);
        let selected = self.open || !self.rules.is_empty();
        if icons::icon_toggle(ui, tx, selected, Icon::PortForward, label).clicked() {
            self.open = !self.open;
        }
    }

    /// The window (call every frame; draws nothing while closed). Returns its screen rectangle
    /// while shown, so the session view can keep clicks on it away from the remote PC.
    pub fn window(
        &mut self,
        ctx: &egui::Context,
        tx: Tx,
        backend: &dyn Backend,
        session: SessionId,
        permissions: Permissions,
    ) -> Option<egui::Rect> {
        if !self.open {
            return None;
        }
        let mut open = self.open;
        let shown = egui::Window::new(tx.t(S::PortForwarding))
            .id(egui::Id::new(("port-forwarding", session.0)))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .show(ctx, |ui| {
                theme::column(ui, tx.rtl(), |ui| {
                    self.contents(ui, tx, backend, session, permissions)
                });
            });
        self.open = open;
        shown.map(|r| r.response.rect)
    }

    fn contents(
        &mut self,
        ui: &mut egui::Ui,
        tx: Tx,
        backend: &dyn Backend,
        session: SessionId,
        permissions: Permissions,
    ) {
        let dark = ui.visuals().dark_mode;
        ui.label(theme::caption(ui, tx.t(S::TunnelHint)));
        ui.add_space(6.0);
        let allowed = permissions.contains(Permissions::TUNNEL);
        if !allowed {
            theme::row(ui, tx.rtl(), |ui| {
                icons::icon(ui, tx, Icon::Lock, icons::BUTTON_ICON, theme::danger(dark));
                ui.label(RichText::new(tx.t(S::TunnelErrNotAllowed)).color(theme::danger(dark)));
            });
            ui.add_space(6.0);
        }

        // --- add a rule ---
        ui.add_enabled_ui(allowed, |ui| {
            for (key, value, hint, salt, width) in [
                (
                    S::TunnelLocalPort,
                    &mut self.local_port,
                    "8080",
                    "tunnel-local",
                    80.0,
                ),
                (
                    S::TunnelRemoteHost,
                    &mut self.host,
                    tx.t(S::TunnelRemoteHostHint),
                    "tunnel-host",
                    220.0,
                ),
                (
                    S::TunnelRemotePort,
                    &mut self.port,
                    "80",
                    "tunnel-port",
                    80.0,
                ),
            ] {
                theme::row(ui, tx.rtl(), |ui| {
                    ui.label(tx.t(key));
                    ui.add_space(6.0);
                    widgets::text_field(ui, tx, salt, value, hint, width);
                });
            }
            ui.checkbox(&mut self.lan_bind, tx.t(S::TunnelLanBind));
            if self.lan_bind {
                theme::row(ui, tx.rtl(), |ui| {
                    icons::icon(
                        ui,
                        tx,
                        Icon::Warning,
                        icons::BUTTON_ICON,
                        theme::danger(dark),
                    );
                    ui.label(
                        RichText::new(tx.t(S::TunnelLanBindWarning)).color(theme::danger(dark)),
                    );
                });
            }
            ui.add_space(4.0);
            theme::row(ui, tx.rtl(), |ui| {
                let add = widgets::primary_icon_button(ui, tx, Icon::Plus, tx.t(S::TunnelAdd));
                if ui.add(add).clicked() {
                    match self.request() {
                        Some(r) => {
                            self.invalid = false;
                            self.problem = None;
                            backend.add_tunnel(session, r);
                        }
                        None => self.invalid = true,
                    }
                }
            });
        });
        let error = if self.invalid {
            Some(tx.t(S::TunnelBadPort).to_owned())
        } else {
            self.problem.as_ref().map(|p| problem_text(tx, p))
        };
        if let Some(e) = error {
            theme::row(ui, tx.rtl(), |ui| {
                icons::icon(ui, tx, Icon::Error, icons::BUTTON_ICON, theme::danger(dark));
                ui.label(RichText::new(e).color(theme::danger(dark)));
            });
        }

        // --- active rules ---
        ui.separator();
        if self.rules.is_empty() {
            ui.label(theme::caption(ui, tx.t(S::TunnelNone)));
            return;
        }
        let mut stop = None;
        for r in &self.rules {
            egui::Frame::new()
                .fill(ui.visuals().extreme_bg_color)
                .corner_radius(8)
                .inner_margin(8)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    theme::row(ui, tx.rtl(), |ui| {
                        icons::icon(
                            ui,
                            tx,
                            Icon::PortForward,
                            icons::BUTTON_ICON,
                            theme::accent(dark),
                        );
                        // Addresses are always laid out left to right.
                        ui.label(
                            RichText::new(tx.n(&format!("{} -> {}", r.local, r.target))).strong(),
                        );
                        theme::row_end(ui, tx.rtl(), |ui| {
                            let b = icons::icon_text_button(tx, Icon::Stop, tx.t(S::TunnelStop));
                            if ui.add(b).clicked() {
                                stop = Some(r.id);
                            }
                        });
                    });
                    ui.label(theme::caption(ui, &rule_stats(tx, r)));
                });
            ui.add_space(4.0);
        }
        if let Some(id) = stop {
            backend.stop_tunnel(session, id);
        }
        // Counters move while connections are open.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(1000));
    }
}

/// "2 open, 5 total, sent 1.2 MB, received 30 MB" (+ ", 1 failed").
pub fn rule_stats(tx: Tx, r: &TunnelRow) -> String {
    let n = |v: u64| tx.digits(&v.to_string());
    let mut s = tx.f(
        S::TunnelRuleStats,
        &[
            ("open", &n(r.active as u64)),
            ("total", &n(r.connections)),
            (
                "sent",
                &tx.digits(&file_size(r.bytes_sent, tx.lang, tx.digits)),
            ),
            (
                "received",
                &tx.digits(&file_size(r.bytes_received, tx.lang, tx.digits)),
            ),
        ],
    );
    if r.failed > 0 {
        s = format!(
            "{s} · {}",
            tx.f(S::TunnelFailedCount, &[("n", &n(r.failed))])
        );
    }
    s
}

/// Host side: port forwarding state of one connected viewer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostTunnelState {
    pub allowed: bool,
    pub tunnels: Vec<HostTunnelRow>,
}

/// In the host banner row: "Allow port forwarding" switch and the open connection count.
/// White text: the banner has the accent color as background.
pub fn host_banner_controls(
    ui: &mut egui::Ui,
    tx: Tx,
    backend: &dyn Backend,
    session: SessionId,
    state: Option<&HostTunnelState>,
) {
    let mut allowed = state.is_some_and(|s| s.allowed);
    let open: usize = state.map_or(0, |s| s.tunnels.iter().map(|t| t.active).sum());
    if open > 0 {
        icons::icon(
            ui,
            tx,
            Icon::PortForward,
            icons::BUTTON_ICON,
            Color32::WHITE,
        );
        let text = tx.f(
            S::HostTunnelsActive,
            &[("n", &tx.digits(&open.to_string()))],
        );
        ui.label(RichText::new(text).color(Color32::WHITE));
    }
    let label = RichText::new(tx.t(S::HostTunnelsAllow)).color(Color32::WHITE);
    if ui.checkbox(&mut allowed, label).changed() {
        backend.allow_tunnels(session, allowed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{AttemptId, ConnectTarget, DeviceId, LocalInfo, RequestId};
    use crate::i18n::{DigitStyle, Lang};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder {
        added: Mutex<Vec<TunnelRequest>>,
        stopped: Mutex<Vec<u64>>,
        allowed: Mutex<Vec<bool>>,
    }

    impl Backend for Recorder {
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
        fn add_tunnel(&self, _: SessionId, r: TunnelRequest) {
            self.added.lock().unwrap().push(r);
        }
        fn stop_tunnel(&self, _: SessionId, id: u64) {
            self.stopped.lock().unwrap().push(id);
        }
        fn allow_tunnels(&self, _: SessionId, allowed: bool) {
            self.allowed.lock().unwrap().push(allowed);
        }
    }

    #[test]
    fn ports_accept_arabic_digits_and_reject_nonsense() {
        assert_eq!(parse_port("8080"), Some(8080));
        assert_eq!(parse_port(" ٨٠٨٠ "), Some(8080));
        assert_eq!(parse_port("۳۳۸۹"), Some(3389));
        for bad in ["", "0", "65536", "-1", "http", "80a"] {
            assert_eq!(parse_port(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn form_builds_requests() {
        let mut p = TunnelsPanel {
            local_port: "٨٠٨٠".into(),
            host: " ١٩٢.١٦٨.١.٥ ".into(),
            port: "80".into(),
            ..TunnelsPanel::default()
        };
        assert_eq!(
            p.request(),
            Some(TunnelRequest {
                local_port: 8080,
                lan_bind: false,
                target_host: Some("192.168.1.5".into()),
                target_port: 80,
            })
        );
        p.host.clear();
        p.lan_bind = true;
        let r = p.request().unwrap();
        assert_eq!((r.target_host, r.lan_bind), (None, true));
        p.port = "0".into();
        assert!(p.request().is_none());
    }

    #[test]
    fn every_problem_has_text_in_both_languages() {
        for lang in [Lang::En, Lang::Ar] {
            let tx = Tx::new(lang, DigitStyle::Western);
            for p in [
                TunnelProblem::NotAllowed,
                TunnelProblem::Revoked,
                TunnelProblem::LocalPort("os error 10048".into()),
                TunnelProblem::TargetNotAllowed,
                TunnelProblem::BadTarget,
                TunnelProblem::TooMany,
                TunnelProblem::NoAnswer,
                TunnelProblem::SessionEnded,
                TunnelProblem::Other("خطأ غير متوقع".into()),
            ] {
                let s = problem_text(tx, &p);
                assert!(!s.trim().is_empty() && !s.contains('{'), "{p:?}: {s}");
            }
        }
    }

    fn row() -> TunnelRow {
        TunnelRow {
            id: 4,
            local: "127.0.0.1:8080".parse().unwrap(),
            target: "localhost:80".into(),
            active: 2,
            connections: 5,
            failed: 1,
            bytes_sent: 1536,
            bytes_received: 3 * 1024 * 1024,
        }
    }

    #[test]
    fn rule_stats_format() {
        let en = rule_stats(Tx::new(Lang::En, DigitStyle::Western), &row());
        assert!(en.contains("2 open"), "{en}");
        assert!(
            en.contains("1.5 KB") && en.contains("3.0 MB") && en.contains("1 failed"),
            "{en}"
        );
        let ar = rule_stats(Tx::new(Lang::Ar, DigitStyle::ArabicIndic), &row());
        assert!(ar.contains('٢') && ar.contains("مفتوح"), "{ar}");
    }

    /// Renders the window and the banner headlessly in both languages, clicks every button,
    /// and checks the backend calls.
    #[test]
    fn panel_and_banner_render_and_act() {
        for lang in [Lang::En, Lang::Ar] {
            let ctx = egui::Context::default();
            icons::install(&ctx);
            let backend = Recorder::default();
            let tx = Tx::new(lang, DigitStyle::Western);
            let mut panel = TunnelsPanel {
                open: true,
                local_port: "8080".into(),
                port: "80".into(),
                lan_bind: true,
                rules: vec![row()],
                problem: Some(TunnelProblem::TargetNotAllowed),
                ..TunnelsPanel::default()
            };
            let host = HostTunnelState {
                allowed: false,
                tunnels: vec![HostTunnelRow {
                    id: 1,
                    target: "127.0.0.1:80".parse().unwrap(),
                    active: 3,
                    connections: 3,
                }],
            };
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0));
            let mut run = |events: Vec<egui::Event>| {
                // A click on the window's close button must not end the test.
                panel.open = true;
                let input = egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                };
                let mut out = ctx.run_ui(input, |ui| {
                    egui::Panel::top("banner").show(ui, |ui| {
                        ui.horizontal(|ui| {
                            host_banner_controls(ui, tx, &backend, SessionId(2), Some(&host))
                        });
                    });
                    panel.window(ui.ctx(), tx, &backend, SessionId(1), Permissions::ALL);
                });
                out.textures_delta.clear();
            };
            run(vec![]);
            run(vec![]);
            // Click over a grid of points until every button has been hit.
            for yi in 0..80 {
                for xi in 0..50 {
                    let p = egui::pos2(xi as f32 * 20.0 + 5.0, yi as f32 * 10.0 + 3.0);
                    run(vec![
                        egui::Event::PointerMoved(p),
                        egui::Event::PointerButton {
                            pos: p,
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            modifiers: Default::default(),
                        },
                        egui::Event::PointerButton {
                            pos: p,
                            button: egui::PointerButton::Primary,
                            pressed: false,
                            modifiers: Default::default(),
                        },
                    ]);
                    let done = !backend.added.lock().unwrap().is_empty()
                        && !backend.stopped.lock().unwrap().is_empty()
                        && !backend.allowed.lock().unwrap().is_empty();
                    if done {
                        break;
                    }
                }
            }
            let added = backend.added.lock().unwrap();
            assert!(
                added
                    .iter()
                    .any(|r| r.local_port == 8080 && r.target_port == 80),
                "{lang:?}: {added:?}"
            );
            assert_eq!(
                backend.stopped.lock().unwrap().first(),
                Some(&4),
                "{lang:?}"
            );
            assert_eq!(
                backend.allowed.lock().unwrap().first(),
                Some(&true),
                "{lang:?}"
            );
        }
    }
}
