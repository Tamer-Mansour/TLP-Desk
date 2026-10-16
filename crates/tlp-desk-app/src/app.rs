//! Top-level eframe application: owns UI state, drains backend events, routes screens.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

/// How long the host banner keeps showing "remote input is blocked" after the last refusal.
const HOST_BLOCKED_NOTICE: std::time::Duration = std::time::Duration::from_secs(15);

use eframe::egui::{self, Id, RichText};
use tlp_desk_proto::RejectReason;

use crate::backend::chat::ChatLine;
use crate::backend::{
    AttemptId, Backend, BackendEvent, ConnectError, ConnectTarget, DiscoveredDevice, EndReason,
    EventReceiver, HostProblem, LocalInfo, PeerInfo, RequestId, SessionId,
};
use crate::i18n::{format::to_western_digits, Lang, Tx, S};
use crate::keygrab::{self, SystemKeyGrab};
use crate::settings::Settings;
use crate::text_route::TextRoute;
use crate::ui::address_book::AddressBookUi;
use crate::ui::audio::AudioControls;
use crate::ui::chat::{self as chat_ui, ChatState};
use crate::ui::icons::{self, Icon};
use crate::ui::recordings::{self, RecordState, RecordingsUi};
use crate::ui::session::ViewerSession;
use crate::ui::tunnels::{self, HostTunnelState};
use crate::ui::{theme, widgets};

/// Which full-window screen is shown when no viewer session is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Home,
    Settings,
    /// Connection log (TLP-settings-tray).
    ConnectionLog,
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
    /// Keep the address book in memory only: nothing is read from or written to
    /// `%APPDATA%` (screenshots, tests).
    pub address_book_in_memory: bool,
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
    /// A viewer has blanked this device's screen (privacy mode).
    pub host_privacy: bool,
    /// Port forwarding state per incoming session (TLP-tunnel).
    pub host_tunnels: HashMap<SessionId, HostTunnelState>,
    /// Host side: chat with each connected viewer (window opens when the viewer writes).
    pub host_chats: HashMap<SessionId, ChatState>,
    /// When the OS last refused a remote viewer's input (elevated window in front).
    host_input_blocked: Option<std::time::Instant>,
    pub session: Option<ViewerSession>,
    /// Shown once after a remote end.
    pub notice: Option<String>,
    /// TLP-service: the installed service hosts this device.
    pub service_hosting: bool,
    /// Permanent password card and password sign-in (TLP-unattended).
    pub unattended: crate::ui::unattended::UnattendedUi,
    /// Saved devices (home screen tab).
    pub address_book: AddressBookUi,
    /// macOS Screen Recording / Accessibility guidance (never shown on Windows).
    pub permissions: crate::ui::permissions::PermissionsUi,
    // --- TLP-settings-tray ---
    /// Settings screen editor state.
    pub settings_ui: crate::ui::settings::SettingsUi,
    /// "Start with Windows" (the HKCU Run key; a fake in tests).
    pub autostart: Box<dyn crate::settings::autostart::AutoStart>,
    /// Notification-area icon and main-window mode.
    pub tray: crate::tray::TrayState,
    /// Connection log screen state.
    pub conn_log: crate::ui::connection_log::ConnLogUi,
    /// Recordings window and playback windows (TLP-recording).
    pub recordings: RecordingsUi,
    /// Recent clipboard activity of host sessions (banner indicator, TLP-clipboard).
    pub host_clipboard: crate::ui::clipboard::HostClipboard,
    /// Per-viewer Pause input / View only toggles in the banner (TLP-multiviewer).
    pub host_input: crate::ui::viewer_input::HostInputControls,
    /// "These files already exist" questions (viewer downloads, viewer uploads to this host).
    pub conflicts: crate::ui::transfer_conflict::ConflictDialogs,
    /// Files moving to and from this host: list, window and notices (TLP-transfer-ux).
    pub host_transfers: crate::ui::host_transfers::HostTransfers,
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
        let unattended =
            crate::ui::unattended::UnattendedUi::new(backend.unattended_password_state());
        let book_tx = Tx::new(settings.lang, settings.digits);
        let address_book = if opts.address_book_in_memory {
            AddressBookUi::load(None, book_tx)
        } else {
            AddressBookUi::load_default(book_tx)
        };
        let mut app = Self {
            unattended,
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
            host_privacy: false,
            host_tunnels: HashMap::new(),
            host_chats: HashMap::new(),
            host_input_blocked: None,
            session: None,
            notice: None,
            service_hosting: false,
            address_book,
            permissions: crate::ui::permissions::PermissionsUi::new(),
            settings_ui: Default::default(),
            autostart: crate::settings::autostart::system(),
            tray: Default::default(),
            conn_log: Default::default(),
            recordings: RecordingsUi::default(),
            host_clipboard: Default::default(),
            host_input: Default::default(),
            conflicts: Default::default(),
            host_transfers: Default::default(),
        };
        // The Run key is the truth for "start with Windows" (it can be removed outside the app).
        if let Ok(on) = app.autostart.is_enabled() {
            app.settings.startup.start_with_windows = on;
        }
        app.backend
            .set_clipboard_default(!app.settings.clipboard_sync_off);
        app.backend
            .set_audio_on_connect(!app.settings.mute_remote_audio);
        if let Some(code) = opts.code {
            app.code = code;
        }
        if let Some(target) = opts.connect {
            app.address = target;
            app.connect_from_address();
            // Started to open a session: never start minimized (the setting, or the show state
            // a console or script launcher passed to the process).
            app.tray.show_on_start = true;
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
        if self.unattended.use_password {
            self.connect_password(target);
            return;
        }
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

    /// Connect with the permanent password typed in the connect box (TLP-unattended). The
    /// password stays in the field until the attempt ends, so a confirmed key change can retry.
    fn connect_password(&mut self, target: ConnectTarget) {
        if self.unattended.password.is_empty() {
            self.error = Some(self.tx.t(S::ErrPasswordEmpty).to_owned());
            return;
        }
        if self.pending.is_some() || self.session.is_some() {
            return;
        }
        let label = target.to_string();
        let attempt = self
            .backend
            .connect_with_password(target.clone(), self.unattended.password.clone());
        self.pending = Some(Pending {
            attempt,
            target: label,
            awaiting_accept: false,
            dial: target,
            code: None,
        });
    }

    fn error_text(&self, error: &ConnectError) -> String {
        let tx = self.tx;
        match error {
            ConnectError::Rejected(RejectReason::RateLimited) => {
                crate::ui::unattended::locked_out_text(tx)
            }
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
                BackendEvent::Devices(list) => {
                    self.address_book
                        .on_devices(&list, self.backend.as_ref(), self.tx);
                    self.devices = Some(list);
                }
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
                        other => {
                            self.unattended.password.clear();
                            self.error = Some(self.error_text(&other));
                        }
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
                        self.unattended.password.clear();
                        self.address_book
                            .on_session_started(&peer, self.backend.as_ref(), self.tx);
                        let mut view = ViewerSession::new(session, peer, permissions, frames);
                        view.clipboard.sync = !self.settings.clipboard_sync_off;
                        view.quality = self.settings.quality;
                        view.typing.set_route(TextRoute::from_layout_setting(
                            self.settings.type_with_my_layout,
                        ));
                        self.backend
                            .request_quality(session, self.settings.quality.to_proto());
                        view.audio = AudioControls::new(!self.settings.mute_remote_audio);
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
                        if !permissions.contains(tlp_desk_proto::Permissions::PRIVACY_MODE) {
                            s.privacy.permission_lost();
                        }
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
                    self.host_input.retain(|s| s != session);
                    if self.host_sessions.is_empty() {
                        // Privacy mode never outlives the last session.
                        self.host_privacy = false;
                    }
                    self.host_tunnels.remove(&session);
                    self.host_chats.remove(&session);
                    if self.session.as_ref().is_some_and(|s| s.id == session) {
                        let reconnect_failed = self
                            .session
                            .take()
                            .and_then(|mut s| s.unattended.failed.take());
                        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Title(
                            self.tx.raw(S::AppTitle).into(),
                        ));
                        self.notice = match (reconnect_failed, reason) {
                            (Some(detail), _) => {
                                Some(self.tx.f(S::ReconnectFailedText, &[("detail", &detail)]))
                            }
                            (None, EndReason::Local) => None,
                            (None, EndReason::Remote) => Some(self.tx.t(S::SessionEnded).into()),
                            (None, EndReason::Error(e)) => {
                                Some(self.tx.f(S::ErrOther, &[("detail", &e)]))
                            }
                        };
                    }
                }
                BackendEvent::IncomingRequest { request, peer } => {
                    let name = peer.name.clone();
                    self.incoming.push_back((request, peer));
                    if self.tray.system_present() {
                        self.tray_incoming_request(ctx, &name);
                    } else {
                        bring_to_front(ctx);
                    }
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
                BackendEvent::HostInputBlocked { session } => {
                    if self.host_sessions.iter().any(|(id, _)| *id == session) {
                        self.host_input_blocked = Some(std::time::Instant::now());
                    }
                }
                BackendEvent::HostUnavailable(problem) => self.host_problem = Some(problem),
                // --- privacy mode (TLP-privacy) ---
                BackendEvent::PrivacyChanged { session, enabled } => {
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.privacy.on_state(enabled, std::time::Instant::now());
                    }
                }
                BackendEvent::HostPrivacyChanged { active, .. } => self.host_privacy = active,
                // --- service (TLP-service) ---
                BackendEvent::RemoteDesktopState { session, state } => {
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.remote_desktop = Some(state);
                        s.unattended.host_service = true;
                    }
                }
                BackendEvent::ServiceHosting(on) => self.service_hosting = on,
                // --- unattended (TLP-unattended) ---
                BackendEvent::PasswordState(state) => self.unattended.state = state,
                BackendEvent::PasswordChanged(result) => {
                    self.unattended.on_password_changed(self.tx, result);
                }
                BackendEvent::RemoteRestarting { session } => {
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.unattended.on_restarting();
                    }
                }
                BackendEvent::Reconnecting { session } => {
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.unattended.on_network_lost();
                    }
                }
                BackendEvent::ReconnectAttempt { session, attempt } => {
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.unattended.attempt = attempt;
                    }
                }
                BackendEvent::Reconnected {
                    session,
                    permissions,
                } => {
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.unattended.on_reconnected();
                        s.permissions = permissions;
                    }
                }
                BackendEvent::ReconnectFailed { session, error } => {
                    let text = self.error_text(&error);
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.unattended.failed = Some(text);
                    }
                }
                // --- device info (TLP-cli-devinfo) ---
                BackendEvent::DeviceInfo { session, info } => {
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.device_info.set_info(info);
                    }
                }
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
                // --- audio (TLP-audio) ---
                BackendEvent::AudioState { session, state } => {
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.audio.apply(state);
                    }
                }
                BackendEvent::AudioDeviceFailed { session } => {
                    if self.session.as_ref().is_some_and(|s| s.id == session) {
                        self.notice = Some(self.tx.t(S::AudioDeviceFailed).into());
                    }
                }
                // whiteboard (TLP-whiteboard)
                BackendEvent::WhiteboardRemote { session, op } => {
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.whiteboard.remote(&op, std::time::Instant::now());
                    }
                }
                // recording (TLP-recording)
                BackendEvent::RecordingStarted { session, .. } => {
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.recording = RecordState::On {
                            since: std::time::Instant::now(),
                        };
                    }
                }
                BackendEvent::RecordingStopped {
                    session, problem, ..
                } => {
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.recording = RecordState::Off;
                    }
                    if let Some(p) = problem {
                        self.notice = Some(recordings::problem_text(self.tx, &p));
                    }
                    self.recordings.reload();
                }
                // chat (TLP-chat)
                BackendEvent::ChatReceived { session, line } => {
                    self.chat_received(ctx, session, line)
                }
                BackendEvent::ChatAvailability { session, allowed } => {
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.chat.set_allowed(allowed);
                    } else if self.host_sessions.iter().any(|(id, _)| *id == session) {
                        self.host_chats
                            .entry(session)
                            .or_insert_with(|| ChatState::new(allowed))
                            .set_allowed(allowed);
                    }
                }
                BackendEvent::Clipboard { session, event } => self.clipboard_event(session, event),
                // --- files (TLP-files) ---
                BackendEvent::FsResult {
                    side,
                    request,
                    result,
                } => {
                    if let Some(s) = self.session.as_mut() {
                        s.files.on_result(side, request, result);
                    }
                }
                BackendEvent::RemoteClipboardFiles { session, files } => {
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.files.remote_clipboard = files;
                    }
                }
                BackendEvent::FilesNotice { session, notice } => {
                    let tx = self.tx;
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.files.on_notice(tx, &notice);
                        // Progress of the copy shows in the transfer list.
                        if matches!(
                            notice,
                            crate::backend::files::FilesNotice::TransferStarted { .. }
                        ) {
                            s.transfers.open = true;
                        }
                    }
                }
                BackendEvent::Transfer { session, view } => {
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.transfers.update(view);
                    } else if let Some(who) = self.host_peer_label(session) {
                        if self.host_transfers.update(view, &who) {
                            chat_ui::notify_if_unfocused(ctx);
                        }
                    }
                }
                BackendEvent::TransferConflict { session, prompt } => {
                    let who = self.host_peer_label(session).unwrap_or_default();
                    self.conflicts.push(prompt, &who);
                    bring_to_front(ctx);
                }
                BackendEvent::TransferConflictClosed { id } => self.conflicts.close(id),
                BackendEvent::TransferNotStarted { session, error } => {
                    if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
                        s.transfers.not_started(error);
                    }
                }
            }
        }
    }

    /// "Name (123 456 789)" of the viewer behind a host session, name sanitized.
    fn host_peer_label(&self, session: SessionId) -> Option<String> {
        self.host_sessions
            .iter()
            .find(|(id, _)| *id == session)
            .map(|(_, p)| {
                let name = crate::banner::sanitize_name(&p.name);
                if name.is_empty() {
                    p.device_id.to_string()
                } else {
                    format!("{name} ({})", p.device_id)
                }
            })
    }

    /// Host side: the transfer window, the notices, and the user's requests in them.
    fn host_transfer_ui(&mut self, ctx: &egui::Context) {
        let tx = self.tx;
        let backend = Arc::clone(&self.backend);
        for action in self.host_transfers.show(ctx, tx, backend.as_ref()) {
            crate::ui::session::apply_queue_action(backend.as_ref(), SessionId(0), action);
        }
    }

    /// A chat message arrived: into the viewer session's panel, or the host chat window (which
    /// opens). The taskbar button flashes when the window is in the background.
    fn chat_received(&mut self, ctx: &egui::Context, session: SessionId, line: ChatLine) {
        if let Some(s) = self.session.as_mut().filter(|s| s.id == session) {
            s.chat.push(line);
        } else if self.host_sessions.iter().any(|(id, _)| *id == session) {
            let chat = self
                .host_chats
                .entry(session)
                .or_insert_with(|| ChatState::new(true));
            chat.push(line);
            chat.set_open(true);
        } else {
            return;
        }
        chat_ui::notify_if_unfocused(ctx);
    }

    /// Host side: one chat window per connected viewer that has one open.
    fn host_chat_windows(&mut self, ctx: &egui::Context) {
        let tx = self.tx;
        for (session, peer) in &self.host_sessions {
            if let Some(chat) = self.host_chats.get_mut(session) {
                chat_ui::host_window(ctx, tx, chat, &peer.name, self.backend.as_ref(), *session);
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
        let mut toggle_chat = None;
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
                        if self.host_privacy {
                            ui.separator();
                            icons::icon(ui, tx, Icon::Privacy, 18.0, egui::Color32::WHITE);
                            ui.label(
                                RichText::new(tx.t(S::PrivacyHostOn)).color(egui::Color32::WHITE),
                            );
                        }
                        self.host_clipboard.indicator(ui, tx, *session);
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
                            // several viewers at once (TLP-multiviewer)
                            if self.host_sessions.len() > 1
                                || self.host_input.is_paused(*session)
                                || self.host_input.is_view_only(*session)
                            {
                                self.host_input
                                    .controls(ui, tx, self.backend.as_ref(), *session);
                            }
                            // chat (TLP-chat)
                            if let Some(chat) = self.host_chats.get(session) {
                                if chat_ui::chat_toggle(ui, tx, chat) {
                                    toggle_chat = Some(*session);
                                }
                            }
                        });
                    });
                }
                // Files moving to or from this PC are never invisible.
                if self.host_transfers.has_activity() {
                    theme::row(ui, tx.rtl(), |ui| {
                        if self.host_transfers.chip(ui, tx) {
                            self.host_transfers.open = !self.host_transfers.open;
                        }
                    });
                }
                // The remote person types into the void otherwise: say why.
                if self
                    .host_input_blocked
                    .is_some_and(|t| t.elapsed() < HOST_BLOCKED_NOTICE)
                {
                    ui.label(
                        RichText::new(tx.t(S::HostInputBlocked))
                            .color(egui::Color32::WHITE)
                            .small(),
                    );
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_secs(1));
                }
            });
        if let Some(s) = end {
            self.backend.disconnect(s);
        }
        if let Some(chat) = toggle_chat.and_then(|s| self.host_chats.get_mut(&s)) {
            chat.set_open(!chat.is_open());
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

        if let Some((id, answer)) = self.conflicts.show(ctx, tx) {
            self.backend.resolve_conflict(id, answer);
        }
        if self.conflicts.is_open() {
            any = true;
        }

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
                        if self.unattended.use_password && !self.unattended.password.is_empty() {
                            self.connect_password(w.dial);
                        } else {
                            self.connect_with(w.dial, w.code);
                        }
                    } else {
                        self.error = Some(self.tx.t(S::ErrIdentity).to_owned());
                    }
                }
                Some(false) => {
                    self.key_warning = None;
                    self.unattended.password.clear();
                }
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
        self.tray_frame(&ctx);
        self.banner_window(&ctx);
        self.host_banner(ui);
        if self.session.is_none() {
            self.host_problem_banner(ui);
            if self.service_hosting {
                crate::ui::service::hosting_banner(ui, self.tx);
            }
        }
        let modal_open = self.dialogs(&ctx);
        let recording_now = self
            .session
            .as_ref()
            .is_some_and(|s| s.recording != RecordState::Off);
        self.recordings.show(&ctx, self.tx, recording_now);
        self.host_chat_windows(&ctx);
        self.host_transfer_ui(&ctx);

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
            Screen::ConnectionLog => self.connection_log_ui(ui),
        }
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // A drag out of the file browser ran the OS drag loop, which took the mouse-up.
        crate::ui::file_browser::release_pointer_if_requested(ctx, raw_input);
    }
}
