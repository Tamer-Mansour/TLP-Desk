//! Top-level eframe application: owns UI state, drains backend events, routes screens.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use eframe::egui::{self, Id, RichText};
use tlp_desk_proto::RejectReason;

use crate::backend::{
    AttemptId, Backend, BackendEvent, ConnectError, ConnectTarget, DiscoveredDevice, EndReason,
    EventReceiver, HostProblem, LocalInfo, PeerInfo, RequestId, SessionId,
};
use crate::i18n::{format::to_western_digits, Lang, Tx, S};
use crate::keygrab::{self, SystemKeyGrab};
use crate::settings::Settings;
use crate::ui::icons::{self, Icon};
use crate::ui::session::ViewerSession;
use crate::ui::tunnels::{self, HostTunnelState};
use crate::ui::{theme, widgets};

/// Which full-window screen is shown when no viewer session is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Home,
    Settings,
}

/// A connection attempt in progress.
pub struct Pending {
    pub attempt: AttemptId,
    /// Logical-order label of what we dial (ID / name / IP).
    pub target: String,
    pub awaiting_accept: bool,
    /// What was dialed and with which code, to retry after a confirmed key change.
    pub dial: ConnectTarget,
    pub code: Option<String>,
}

/// A connection stopped because the host key changed; the user must decide.
#[derive(Debug, Clone)]
pub struct KeyWarning {
    pub attempt: AttemptId,
    pub target: String,
    pub dial: ConnectTarget,
    pub code: Option<String>,
    pub previous: String,
    pub presented: String,
}

/// Normalize a typed one-time code: Arabic-Indic digits become Western, spaces and dashes
/// are dropped. `Ok(None)` for an empty field, `Err(())` if it is not 6 digits.
#[allow(clippy::result_unit_err)]
pub fn normalize_code(input: &str) -> Result<Option<String>, ()> {
    let digits: String = to_western_digits(input)
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '\u{a0}'))
        .collect();
    if digits.is_empty() {
        Ok(None)
    } else if digits.len() == 6 && digits.chars().all(|c| c.is_ascii_digit()) {
        Ok(Some(digits))
    } else {
        Err(())
    }
}

/// Startup options (from the command line).
#[derive(Debug, Default, Clone)]
pub struct StartOptions {
    pub lang: Option<Lang>,
    pub connect: Option<String>,
    /// One-time code for `connect`.
    pub code: Option<String>,
    pub screen_settings: bool,
}

pub struct App {
    pub backend: Arc<dyn Backend>,
    events: EventReceiver,
    pub settings: Settings,
    pub tx: Tx,
    pub screen: Screen,
    pub local: LocalInfo,
    /// `None` until discovery reported for the first time.
    pub devices: Option<Vec<DiscoveredDevice>>,
    pub address: String,
    /// Optional one-time code typed next to the address box (empty = accept prompt).
    pub code: String,
    pub name_edit: String,
    /// The device name was changed this run (the LAN announcement updates after restart).
    pub renamed: bool,
    pub pending: Option<Pending>,
    /// Display-ready error message.
    pub error: Option<String>,
    /// Loud host-key-changed warning awaiting the user's decision.
    pub key_warning: Option<KeyWarning>,
    /// Why this device cannot accept connections (shown until restart).
    pub host_problem: Option<HostProblem>,
    /// Raw window handle, for the keyboard grab.
    hwnd: Option<isize>,
    pub incoming: VecDeque<(RequestId, PeerInfo)>,
    pub host_sessions: Vec<(SessionId, PeerInfo)>,
    /// Port forwarding state per incoming session (TLP-tunnel).
    pub host_tunnels: HashMap<SessionId, HostTunnelState>,
    pub session: Option<ViewerSession>,
    /// Shown once after a remote end.
    pub notice: Option<String>,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        backend: Arc<dyn Backend>,
        events: EventReceiver,
        mut settings: Settings,
        opts: StartOptions,
    ) -> Self {
        if let Some(lang) = opts.lang {
            settings.lang = lang;
        }
        theme::install(&cc.egui_ctx);
        theme::apply_lang(&cc.egui_ctx, settings.lang);
        let local = backend.local_info();
        let mut app = Self {
            name_edit: local.device_name.clone(),
            backend,
            events,
            tx: Tx::new(settings.lang, settings.digits),
            settings,
            screen: if opts.screen_settings {
                Screen::Settings
            } else {
                Screen::Home
            },
            local,
            devices: None,
            address: String::new(),
            code: String::new(),
            renamed: false,
            pending: None,
            error: None,
            key_warning: None,
            host_problem: None,
            hwnd: keygrab::window_handle(cc),
            incoming: VecDeque::new(),
            host_sessions: Vec::new(),
            host_tunnels: HashMap::new(),
            session: None,
            notice: None,
        };
        if let Some(code) = opts.code {
            app.code = code;
        }
        if let Some(target) = opts.connect {
            app.address = target;
            app.connect_from_address();
        }
        app
    }

    pub fn set_lang(&mut self, ctx: &egui::Context, lang: Lang) {
        self.settings.lang = lang;
        self.tx = Tx::new(lang, self.settings.digits);
        theme::apply_lang(ctx, lang);
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(
            self.tx.raw(S::AppTitle).into(),
        ));
        self.settings.save();
    }

    pub fn set_digits(&mut self, digits: crate::i18n::DigitStyle) {
        self.settings.digits = digits;
        self.tx = Tx::new(self.settings.lang, digits);
        self.settings.save();
    }

    /// Parse the address box and start connecting.
    pub fn connect_from_address(&mut self) {
        // Accept Arabic-Indic digits typed in the address box.
        let input = to_western_digits(&self.address);
        match ConnectTarget::parse(&input) {
            Some(target) => self.connect(target),
            None => self.error = Some(self.tx.t(S::ErrInvalidAddress).to_owned()),
        }
    }

    /// Connect using the one-time code field (empty = ask the host user to accept). The code
    /// is cleared once used: it rotates on the host after each use.
    pub fn connect(&mut self, target: ConnectTarget) {
        let code = match normalize_code(&self.code) {
            Ok(c) => c,
            Err(()) => {
                self.error = Some(self.tx.t(S::ErrBadCode).to_owned());
                return;
            }
        };
        if self.connect_with(target, code.clone()) && code.is_some() {
            self.code.clear();
        }
    }

    /// Start an attempt with an explicit code. Returns `false` if one is already running.
    pub fn connect_with(&mut self, target: ConnectTarget, code: Option<String>) -> bool {
        if self.pending.is_some() || self.session.is_some() {
            return false;
        }
        let label = target.to_string();
        let attempt = self.backend.connect(target.clone(), code.clone());
        self.pending = Some(Pending {
            attempt,
            target: label,
            awaiting_accept: false,
            dial: target,
            code,
        });
        true
    }

    fn error_text(&self, error: &ConnectError) -> String {
        let tx = self.tx;
        match error {
            ConnectError::NotFound => tx.t(S::ErrNotFound).into(),
            ConnectError::Rejected(r) => tx
                .t(match r {
                    RejectReason::UserDeclined => S::ErrDeclined,
                    RejectReason::WrongCredentials => S::ErrWrongCode,
                    RejectReason::RateLimited => S::ErrRateLimited,
                    RejectReason::NotAllowed => S::ErrNotAllowed,
                    RejectReason::VersionMismatch => S::ErrVersion,
                })
                .into(),
            ConnectError::Unreachable => tx.t(S::ErrUnreachable).into(),
            ConnectError::IdentityMismatch => tx.t(S::ErrIdentity).into(),
            // Normally shown as its own warning dialog (see `KeyWarning`).
            ConnectError::HostKeyChanged { .. } => tx.t(S::KeyChangedTitle).into(),
            ConnectError::NotLocalNetwork => tx.t(S::ErrNotLan).into(),
            ConnectError::Timeout => tx.t(S::ErrTimeout).into(),
            ConnectError::Other(detail) => tx.f(S::ErrOther, &[("detail", detail)]),
        }
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                BackendEvent::LocalInfo(info) => {
                    if info.device_name != self.local.device_name {
                        self.name_edit = info.device_name.clone();
                    }
                    self.local = info;
                }
                BackendEvent::Devices(list) => self.devices = Some(list),
                BackendEvent::AwaitingAccept { attempt } => {
                    if let Some(p) = self.pending.as_mut().filter(|p| p.attempt == attempt) {
                        p.awaiting_accept = true;
                    }
                }
                BackendEvent::ConnectFailed { attempt, error } => {
                    let Some(p) = self.pending.take_if(|p| p.attempt == attempt) else {
                        continue;
                    };
                    match error {
                        ConnectError::HostKeyChanged {
                            previous,
                            presented,
                        } => {
                            self.key_warning = Some(KeyWarning {
                                attempt,
                                target: p.target,
                                dial: p.dial,
                                code: p.code,
                                previous,
                                presented,
                            });
                        }
                        other => self.error = Some(self.error_text(&other)),
                    }
                }
                BackendEvent::SessionStarted {
                    attempt,
                    session,
                    peer,
                    permissions,
                    frames,
                } => {
                    if self.pending.as_ref().is_some_and(|p| p.attempt == attempt) {
                        self.pending = None;
                        let mut view = ViewerSession::new(session, peer, permissions, frames);
                        if let Some(grab) =
                            SystemKeyGrab::start(Arc::downgrade(&self.backend), session, self.hwnd)
                        {
                            view.set_key_grab(Box::new(grab));
                        }
                        self.session = Some(view);
                        let title =
                            format!("{} — {}", self.tx.raw(S::AppTitle), self.session_title());
                        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
                    } else {
                        // Attempt was cancelled locally; do not leak the session.
                        self.backend.disconnect(session);
                    }
                }
                BackendEvent::PermissionsChanged {
                    session,
                    permissions,
                } => {
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.permissions = permissions;
                    }
                }
                BackendEvent::Stats { session, stats } => {
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.stats = Some(stats);
                    }
                }
                BackendEvent::HostInputPriority {
                    session,
                    local_active,
                } => {
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.host_input_priority = local_active;
                    }
                }
                BackendEvent::MonitorsChanged {
                    session,
                    monitors,
                    active,
                } => {
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.set_monitors(monitors, active);
                    }
                }
                BackendEvent::SessionEnded { session, reason } => {
                    self.host_sessions.retain(|(id, _)| *id != session);
                    self.host_tunnels.remove(&session);
                    if self.session.as_ref().is_some_and(|s| s.id == session) {
                        self.session = None;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Title(
                            self.tx.raw(S::AppTitle).into(),
                        ));
                        self.notice = match reason {
                            EndReason::Local => None,
                            EndReason::Remote => Some(self.tx.t(S::SessionEnded).into()),
                            EndReason::Error(e) => Some(self.tx.f(S::ErrOther, &[("detail", &e)])),
                        };
                    }
                }
                BackendEvent::IncomingRequest { request, peer } => {
                    self.incoming.push_back((request, peer));
                    bring_to_front(ctx);
                }
                BackendEvent::IncomingRequestCancelled { request } => {
                    self.incoming.retain(|(r, _)| *r != request);
                }
                BackendEvent::HostSessionStarted { session, peer } => {
                    if !self.host_sessions.iter().any(|(id, _)| *id == session) {
                        self.host_sessions.push((session, peer));
                        // The "who is connected" banner must be seen.
                        bring_to_front(ctx);
                    }
                }
                BackendEvent::HostUnavailable(problem) => self.host_problem = Some(problem),
                // --- power: port forwarding (TLP-tunnel) ---
                BackendEvent::Tunnels { session, rules } => {
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.tunnels.rules = rules;
                    }
                }
                BackendEvent::TunnelProblem { session, problem } => {
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.tunnels.problem = Some(problem);
                        s.tunnels.open = true;
                    }
                }
                BackendEvent::HostTunnels {
                    session,
                    allowed,
                    tunnels,
                } => {
                    if self.host_sessions.iter().any(|(id, _)| *id == session) {
                        self.host_tunnels
                            .insert(session, HostTunnelState { allowed, tunnels });
                    }
                }
            }
        }
    }

    fn session_title(&self) -> String {
        self.session
            .as_ref()
            .map(|s| s.peer.name.clone())
            .unwrap_or_default()
    }

    /// Host-side banner: always visible while someone is connected to this device.
    fn host_banner(&mut self, ui: &mut egui::Ui) {
        if self.host_sessions.is_empty() {
            return;
        }
        let tx = self.tx;
        let dark = ui.visuals().dark_mode;
        let mut end = None;
        egui::Panel::top("host-banner")
            .frame(
                egui::Frame::new()
                    .fill(theme::accent(dark))
                    .inner_margin(egui::Margin::symmetric(16, 8)),
            )
            .show(ui, |ui| {
                for (session, peer) in &self.host_sessions {
                    theme::row(ui, tx.rtl(), |ui| {
                        let text = tx.f(S::HostBanner, &[("name", &peer.name)]);
                        icons::icon(ui, tx, Icon::ViewOnly, 18.0, egui::Color32::WHITE);
                        ui.label(RichText::new(text).color(egui::Color32::WHITE).strong());
                        theme::row_end(ui, tx.rtl(), |ui| {
                            let end_button =
                                icons::icon_text_button(tx, Icon::EndSession, tx.t(S::EndSession));
                            if ui.add(end_button).clicked() {
                                end = Some(*session);
                            }
                            tunnels::host_banner_controls(
                                ui,
                                tx,
                                self.backend.as_ref(),
                                *session,
                                self.host_tunnels.get(session),
                            );
                        });
                    });
                }
            });
        if let Some(s) = end {
            self.backend.disconnect(s);
        }
    }

    /// Persistent warning when this device cannot accept connections.
    fn host_problem_banner(&self, ui: &mut egui::Ui) {
        let Some(problem) = &self.host_problem else {
            return;
        };
        let tx = self.tx;
        let text = match problem {
            HostProblem::PortInUse(port) => {
                tx.f(S::HostPortInUse, &[("port", &tx.digits(&port.to_string()))])
            }
            HostProblem::NoNetwork(d) => tx.f(S::NoNetwork, &[("detail", d)]),
            HostProblem::Other(d) => tx.f(S::HostUnavailable, &[("detail", d)]),
        };
        let dark = ui.visuals().dark_mode;
        egui::Panel::top("host-problem")
            .frame(
                egui::Frame::new()
                    .fill(theme::danger(dark))
                    .inner_margin(egui::Margin::symmetric(16, 8)),
            )
            .show(ui, |ui| {
                theme::row(ui, tx.rtl(), |ui| {
                    icons::icon(ui, tx, Icon::Warning, 18.0, egui::Color32::WHITE);
                    ui.label(RichText::new(text).color(egui::Color32::WHITE).strong());
                });
            });
    }

    /// Modal dialogs: incoming request, connecting, error, notice.
    fn dialogs(&mut self, ctx: &egui::Context) -> bool {
        let tx = self.tx;
        let mut any = false;

        if let Some((request, peer)) = self.incoming.front().cloned() {
            any = true;
            let mut answer = None;
            egui::Modal::new(Id::new("incoming-request")).show(ctx, |ui| {
                ui.set_width(400.0);
                theme::column(ui, tx.rtl(), |ui| {
                    widgets::icon_heading(ui, tx, Icon::Monitor, tx.t(S::IncomingTitle));
                    ui.add_space(4.0);
                    ui.label(tx.f(S::IncomingBody, &[("name", &peer.name)]));
                    ui.add_space(6.0);
                    egui::Frame::new()
                        .fill(ui.visuals().extreme_bg_color)
                        .corner_radius(8)
                        .inner_margin(10)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            for (k, v) in [
                                (S::DeviceName, tx.d(&peer.name)),
                                (S::IdLabel, tx.n(&peer.device_id.to_string())),
                                (S::AddressLabel, tx.n(&peer.address.to_string())),
                            ] {
                                theme::row(ui, tx.rtl(), |ui| {
                                    ui.label(theme::caption(ui, tx.t(k)));
                                    // Arabic final forms can overhang their box; keep a gap.
                                    ui.add_space(6.0);
                                    ui.label(RichText::new(v).strong());
                                });
                            }
                        });
                    ui.add_space(10.0);
                    theme::row(ui, tx.rtl(), |ui| {
                        let accept =
                            widgets::primary_icon_button(ui, tx, Icon::Accept, tx.t(S::Accept));
                        if ui.add(accept).clicked() {
                            answer = Some(true);
                        }
                        let decline = icons::icon_text_button(tx, Icon::Reject, tx.t(S::Decline));
                        if ui.add(decline).clicked() {
                            answer = Some(false);
                        }
                    });
                });
            });
            if let Some(accept) = answer {
                self.backend.respond_to_request(request, accept);
                self.incoming.pop_front();
            }
            return any;
        }

        if let Some(w) = self.key_warning.clone() {
            let mut decision = None;
            egui::Modal::new(Id::new("host-key-changed")).show(ctx, |ui| {
                ui.set_width(460.0);
                let dark = ui.visuals().dark_mode;
                theme::column(ui, tx.rtl(), |ui| {
                    theme::row(ui, tx.rtl(), |ui| {
                        icons::icon(ui, tx, Icon::Warning, 22.0, theme::danger(dark));
                        ui.label(
                            RichText::new(tx.t(S::KeyChangedTitle))
                                .heading()
                                .strong()
                                .color(theme::danger(dark)),
                        );
                    });
                    ui.add_space(6.0);
                    let target = tx.digits(&w.target);
                    ui.label(tx.f(S::KeyChangedBody, &[("target", &target)]));
                    ui.add_space(6.0);
                    egui::Frame::new()
                        .fill(ui.visuals().extreme_bg_color)
                        .stroke(egui::Stroke::new(1.5, theme::danger(dark)))
                        .corner_radius(8)
                        .inner_margin(10)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            for (k, v) in [
                                (S::KeyPrevious, &w.previous),
                                (S::KeyPresented, &w.presented),
                            ] {
                                theme::row(ui, tx.rtl(), |ui| {
                                    ui.label(theme::caption(ui, tx.t(k)));
                                    ui.add_space(6.0);
                                    ui.label(RichText::new(tx.n(v)).monospace().strong());
                                });
                            }
                        });
                    ui.add_space(10.0);
                    theme::row(ui, tx.rtl(), |ui| {
                        let cancel = icons::icon_text_button(tx, Icon::Close, tx.t(S::Cancel));
                        if ui.add(cancel).clicked() {
                            decision = Some(false);
                        }
                        let b = widgets::danger_icon_button(
                            ui,
                            tx,
                            Icon::Key,
                            tx.t(S::ForgetKeyAndConnect),
                        );
                        if ui.add(b).clicked() {
                            decision = Some(true);
                        }
                    });
                });
            });
            match decision {
                Some(true) => {
                    self.key_warning = None;
                    if self.backend.forget_host_key(w.attempt) {
                        self.connect_with(w.dial, w.code);
                    } else {
                        self.error = Some(self.tx.t(S::ErrIdentity).to_owned());
                    }
                }
                Some(false) => self.key_warning = None,
                None => {}
            }
            return true;
        }

        if let Some(p) = &self.pending {
            any = true;
            let mut cancel = false;
            let shown_target = tx.digits(&p.target);
            egui::Modal::new(Id::new("connecting")).show(ctx, |ui| {
                ui.set_width(360.0);
                theme::column(ui, tx.rtl(), |ui| {
                    theme::row(ui, tx.rtl(), |ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new(tx.f(S::Connecting, &[("target", &shown_target)]))
                                .strong(),
                        );
                    });
                    if p.awaiting_accept {
                        ui.label(theme::caption(ui, tx.t(S::WaitingForAccept)));
                    }
                    ui.add_space(8.0);
                    theme::row(ui, tx.rtl(), |ui| {
                        let button = icons::icon_text_button(tx, Icon::Close, tx.t(S::Cancel));
                        if ui.add(button).clicked() {
                            cancel = true;
                        }
                    });
                });
            });
            if cancel {
                self.backend.cancel_connect(p.attempt);
                self.pending = None;
            }
            return any;
        }

        for (slot, title) in [
            (&mut self.error, Some(S::ConnectFailedTitle)),
            (&mut self.notice, None),
        ] {
            if let Some(msg) = slot.clone() {
                any = true;
                let mut close = false;
                egui::Modal::new(Id::new(("message", title.is_some()))).show(ctx, |ui| {
                    ui.set_width(360.0);
                    theme::column(ui, tx.rtl(), |ui| {
                        if let Some(t) = title {
                            widgets::icon_heading(ui, tx, Icon::Error, tx.t(t));
                        }
                        ui.label(msg);
                        ui.add_space(8.0);
                        theme::row(ui, tx.rtl(), |ui| {
                            let ok = icons::icon_text_button(tx, Icon::Check, tx.t(S::Ok));
                            if ui.add(ok).clicked() {
                                close = true;
                            }
                        });
                    });
                });
                if close {
                    *slot = None;
                }
                return any;
            }
        }
        any
    }
}

/// Un-minimize, focus and flash the window (incoming request, viewer connected).
fn bring_to_front(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
        egui::UserAttentionType::Critical,
    ));
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain_events(&ctx);
        self.host_banner(ui);
        if self.session.is_none() {
            self.host_problem_banner(ui);
        }
        let modal_open = self.dialogs(&ctx);

        if self.session.is_some() {
            let tx = self.tx;
            let backend = Arc::clone(&self.backend);
            if let Some(session) = self.session.as_mut() {
                session.ui(ui, tx, backend.as_ref(), modal_open);
            }
            return;
        }
        match self.screen {
            Screen::Home => self.home_ui(ui),
            Screen::Settings => self.settings_ui(ui),
        }
    }
}
