//! Unattended access UI (TLP-unattended): the permanent-password card in Settings, the
//! "One-time code / Password" choice next to the address box, and the session toolbar's
//! "System" menu (restart, lock, Ctrl+Alt+Del) with its confirmations and the
//! "Restarting… waiting for the remote PC" overlay.
//!
//! Passwords live only in these `String`s while typed; they are cleared as soon as they are
//! handed to the backend and never logged.

use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Color32, Id, RichText, Ui};
use tlp_desk_crypto::{password_strength, PasswordStrength, MIN_PASSWORD_CHARS};
use tlp_desk_proto::Permissions;

use super::icons::{self, Icon};
use super::{theme, widgets};
use crate::backend::unattended::{
    lockout_minutes, PasswordChange, PasswordError, PasswordState, SystemAction,
};
use crate::backend::{Backend, PeerInfo, SessionId};
use crate::i18n::{Tx, S};

/// How long "Restart requested" is shown while the remote PC has not gone down yet.
const REQUESTED_NOTICE: Duration = Duration::from_secs(90);

/// Text for a rejected login because of the lockout ("locked out for N minutes").
pub fn locked_out_text(tx: Tx) -> String {
    let minutes = tx.digits(&lockout_minutes().to_string());
    tx.f(S::ErrLockedOut, &[("minutes", &minutes)])
}

/// Masked single-line field with a show/hide toggle. Revealed text goes through
/// [`widgets::text_field`] so Arabic shows in the right order.
fn password_field(
    ui: &mut Ui,
    tx: Tx,
    id_salt: &str,
    value: &mut String,
    hint: &str,
    width: f32,
    reveal: &mut bool,
) -> egui::Response {
    theme::row(ui, tx.rtl(), |ui| {
        let resp = if *reveal {
            widgets::text_field(ui, tx, id_salt, value, hint, width)
        } else {
            ui.add(
                egui::TextEdit::singleline(value)
                    .id(ui.make_persistent_id(id_salt))
                    .password(true)
                    .hint_text(hint)
                    .desired_width(width)
                    .horizontal_align(if tx.rtl() { Align::Max } else { Align::Min }),
            )
        };
        let (icon, tip) = if *reveal {
            (Icon::Hidden, S::HidePassword)
        } else {
            (Icon::ViewOnly, S::ShowPassword)
        };
        if icons::icon_button(ui, tx, icon, tx.t(tip)).clicked() {
            *reveal = !*reveal;
        }
        resp
    })
}

fn strength_text(tx: Tx, s: PasswordStrength) -> (String, bool) {
    let (level, ok) = match s {
        PasswordStrength::TooShort => (
            tx.raw(S::StrengthTooShort)
                .replace("{n}", &tx.digits(&MIN_PASSWORD_CHARS.to_string())),
            false,
        ),
        PasswordStrength::Weak => (tx.raw(S::StrengthWeak).to_owned(), true),
        PasswordStrength::Fair => (tx.raw(S::StrengthFair).to_owned(), true),
        PasswordStrength::Strong => (tx.raw(S::StrengthStrong).to_owned(), true),
    };
    (tx.f(S::PasswordStrength, &[("level", &level)]), ok)
}

/// Four-segment strength bar under the new-password field.
fn strength_meter(ui: &mut Ui, tx: Tx, password: &str) {
    if password.is_empty() {
        return;
    }
    let s = password_strength(password);
    let dark = ui.visuals().dark_mode;
    let (filled, color) = match s {
        PasswordStrength::TooShort => (1, theme::danger(dark)),
        PasswordStrength::Weak => (2, Color32::from_rgb(0xE0, 0x9A, 0x2B)),
        PasswordStrength::Fair => (3, theme::accent(dark)),
        PasswordStrength::Strong => (4, theme::success(dark)),
    };
    theme::row(ui, tx.rtl(), |ui| {
        for i in 0..4 {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(34.0, 6.0), egui::Sense::hover());
            let c = if i < filled {
                color
            } else {
                ui.visuals().extreme_bg_color
            };
            ui.painter().rect_filled(rect, 3.0, c);
        }
        let (text, _) = strength_text(tx, s);
        ui.label(theme::caption(ui, &text));
    });
}

/// App-level unattended state: the Settings password card and the connect-box choice.
#[derive(Default)]
pub struct UnattendedUi {
    /// Last state pushed by the backend.
    pub state: PasswordState,
    current: String,
    new: String,
    repeat: String,
    reveal: bool,
    busy: bool,
    /// Result of the last change: (success, display text).
    message: Option<(bool, String)>,
    /// Connect with the permanent password instead of the one-time code.
    pub use_password: bool,
    /// Password typed next to the address box (cleared once used).
    pub password: String,
    reveal_connect: bool,
}

impl UnattendedUi {
    /// Initial state from the backend.
    pub fn new(state: PasswordState) -> Self {
        Self {
            state,
            ..Self::default()
        }
    }

    fn clear_fields(&mut self) {
        self.current.clear();
        self.new.clear();
        self.repeat.clear();
        self.reveal = false;
    }

    /// The backend answered a password change.
    pub fn on_password_changed(&mut self, tx: Tx, result: Result<PasswordChange, PasswordError>) {
        self.busy = false;
        self.message = Some(match result {
            Ok(PasswordChange::Set) => {
                self.clear_fields();
                (true, tx.t(S::PasswordSaved).to_owned())
            }
            Ok(PasswordChange::Removed) => {
                self.clear_fields();
                (true, tx.t(S::PasswordRemoved).to_owned())
            }
            Err(PasswordError::WrongCurrent) => {
                self.current.clear();
                (false, tx.t(S::ErrCurrentPassword).to_owned())
            }
            Err(PasswordError::TooShort) => {
                (false, strength_text(tx, PasswordStrength::TooShort).0)
            }
            Err(PasswordError::Save(d)) => (false, tx.f(S::ErrPasswordSave, &[("detail", &d)])),
            Err(PasswordError::Unavailable) => (false, tx.t(S::ErrPasswordUnavailable).to_owned()),
        });
    }

    /// The Settings card: status, current/new/repeat fields, strength meter, save/remove.
    pub fn settings_card(&mut self, ui: &mut Ui, tx: Tx, backend: &dyn Backend) {
        theme::card(ui, |ui| {
            theme::column(ui, tx.rtl(), |ui| {
                widgets::icon_heading(ui, tx, Icon::Key, tx.t(S::UnattendedTitle));
                ui.add_space(4.0);
                ui.label(theme::caption(ui, tx.t(S::UnattendedHint)));
                ui.add_space(6.0);
                let dark = ui.visuals().dark_mode;
                theme::row(ui, tx.rtl(), |ui| {
                    let (icon, color, key) = if self.state.set {
                        (Icon::ShieldCheck, theme::success(dark), S::PasswordIsSet)
                    } else {
                        (
                            Icon::Shield,
                            ui.visuals().weak_text_color(),
                            S::PasswordNotSet,
                        )
                    };
                    icons::icon(ui, tx, icon, 18.0, color);
                    ui.label(RichText::new(tx.t(key)).strong());
                });
                if self.state.set && !self.state.machine_wide {
                    ui.label(theme::caption(ui, tx.t(S::PasswordUserScope)));
                }
                ui.add_space(6.0);
                let w = (ui.available_width() - 60.0).clamp(160.0, 360.0);
                let need_current = self.state.set && !self.state.admin;
                if need_current {
                    ui.label(theme::caption(ui, tx.t(S::CurrentPassword)));
                    password_field(
                        ui,
                        tx,
                        "pw-current",
                        &mut self.current,
                        tx.t(S::CurrentPassword),
                        w,
                        &mut self.reveal,
                    );
                } else if self.state.set {
                    ui.label(theme::caption(ui, tx.t(S::PasswordAdminNote)));
                }
                ui.label(theme::caption(ui, tx.t(S::NewPassword)));
                password_field(
                    ui,
                    tx,
                    "pw-new",
                    &mut self.new,
                    tx.t(S::NewPassword),
                    w,
                    &mut self.reveal,
                );
                strength_meter(ui, tx, &self.new);
                ui.label(theme::caption(ui, tx.t(S::RepeatPassword)));
                password_field(
                    ui,
                    tx,
                    "pw-repeat",
                    &mut self.repeat,
                    tx.t(S::RepeatPassword),
                    w,
                    &mut self.reveal,
                );
                let differ = !self.repeat.is_empty() && self.repeat != self.new;
                if differ {
                    ui.label(
                        RichText::new(tx.t(S::PasswordsDiffer))
                            .small()
                            .color(theme::danger(dark)),
                    );
                }
                ui.add_space(6.0);
                let strong_enough = password_strength(&self.new) != PasswordStrength::TooShort;
                let current_ok = !need_current || !self.current.is_empty();
                let can_save =
                    !self.busy && strong_enough && !differ && self.repeat == self.new && current_ok;
                theme::row(ui, tx.rtl(), |ui| {
                    let label = if self.state.set {
                        S::ChangePassword
                    } else {
                        S::SetPassword
                    };
                    let save = widgets::primary_icon_button(ui, tx, Icon::Save, tx.t(label));
                    if ui.add_enabled(can_save, save).clicked() {
                        self.busy = true;
                        self.message = None;
                        let current = need_current.then(|| std::mem::take(&mut self.current));
                        backend.set_unattended_password(current, Some(self.new.clone()));
                    }
                    if self.state.set {
                        let remove =
                            icons::icon_text_button(tx, Icon::Trash, tx.t(S::RemovePassword));
                        if ui.add_enabled(!self.busy && current_ok, remove).clicked() {
                            self.busy = true;
                            self.message = None;
                            let current = need_current.then(|| std::mem::take(&mut self.current));
                            backend.set_unattended_password(current, None);
                        }
                    }
                    if self.busy {
                        ui.spinner();
                    }
                });
                if let Some((ok, text)) = &self.message {
                    let color = if *ok {
                        theme::success(dark)
                    } else {
                        theme::danger(dark)
                    };
                    ui.label(RichText::new(text.as_str()).color(color));
                }
            });
        });
    }

    /// The sign-in choice and its field under the address box. Returns `true` when Enter was
    /// pressed in the field (connect now).
    pub fn connect_fields(&mut self, ui: &mut Ui, tx: Tx, code: &mut String) -> bool {
        let mut enter = false;
        theme::row(ui, tx.rtl(), |ui| {
            ui.label(theme::caption(ui, tx.t(S::SignInWith)));
            if ui
                .selectable_label(!self.use_password, tx.t(S::SignInCode))
                .clicked()
            {
                self.use_password = false;
                self.password.clear();
            }
            if ui
                .selectable_label(self.use_password, tx.t(S::SignInPassword))
                .clicked()
            {
                self.use_password = true;
                code.clear();
            }
        });
        ui.add_space(4.0);
        if self.use_password {
            let resp = password_field(
                ui,
                tx,
                "connect-password",
                &mut self.password,
                tx.t(S::PasswordFieldHint),
                240.0,
                &mut self.reveal_connect,
            );
            enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.label(theme::caption(ui, tx.t(S::PasswordFieldNote)));
        } else {
            theme::row(ui, tx.rtl(), |ui| {
                let resp =
                    widgets::text_field(ui, tx, "one-time-code", code, tx.t(S::CodeHint), 200.0);
                enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            });
            ui.label(theme::caption(ui, tx.t(S::CodeFieldHint)));
        }
        enter
    }
}

/// Per viewer session: the System menu, its confirmations and the restart progress.
#[derive(Debug, Default)]
pub struct SessionUnattended {
    confirm: Option<SystemAction>,
    /// The user confirmed a restart at this time (the PC has not gone down yet).
    requested_at: Option<Instant>,
    /// The remote PC went down; the backend is reconnecting.
    pub restarting: bool,
    /// Reconnect try number (0 = waiting for the first one).
    pub attempt: u32,
    /// Why the reconnect failed (display text), shown when the session then ends.
    pub failed: Option<String>,
    /// The host runs the TLP-Desk service (it reported `DesktopState`), so Ctrl+Alt+Del works.
    pub host_service: bool,
}

impl SessionUnattended {
    /// The backend started waiting for the restarted PC.
    pub fn on_restarting(&mut self) {
        self.restarting = true;
        self.attempt = 0;
        self.requested_at = None;
    }

    /// Back in after the restart.
    pub fn on_reconnected(&mut self) {
        self.restarting = false;
        self.attempt = 0;
        self.requested_at = None;
    }

    /// While restarting, local input must not go anywhere.
    pub fn blocks_input(&self) -> bool {
        self.restarting || self.confirm.is_some()
    }

    /// Toolbar "System" menu (icon only, tooltip "System"). Restart and lock ask for
    /// confirmation first; Ctrl+Alt+Del is sent at once.
    pub fn menu(
        &mut self,
        ui: &mut Ui,
        tx: Tx,
        perms: Permissions,
        backend: &dyn Backend,
        id: SessionId,
    ) {
        let weak = ui.visuals().text_color();
        let resp = ui.menu_image_button(Icon::Power.image(icons::BUTTON_ICON, weak), |ui| {
            theme::column(ui, tx.rtl(), |ui| {
                for (action, icon, key) in [
                    (SystemAction::Restart, Icon::Restart, S::RestartRemote),
                    (SystemAction::Lock, Icon::Lock, S::LockRemote),
                    (
                        SystemAction::CtrlAltDel,
                        Icon::CtrlAltDel,
                        S::SendCtrlAltDel,
                    ),
                ] {
                    let allowed = perms.contains(action.required()) && !self.restarting;
                    let b = icons::icon_text_button(tx, icon, tx.t(key));
                    let mut r = ui.add_enabled(allowed, b);
                    if action == SystemAction::CtrlAltDel && !self.host_service {
                        r = r.on_hover_text(tx.t(S::CtrlAltDelHint));
                    }
                    let r = r.on_disabled_hover_text(tx.t(S::SystemNotAllowed));
                    if r.clicked() {
                        match action {
                            SystemAction::CtrlAltDel => backend.system_action(id, action),
                            other => self.confirm = Some(other),
                        }
                        ui.close();
                    }
                }
            });
        });
        resp.response.on_hover_text(tx.t(S::SystemMenu));
    }

    /// Confirmation dialogs. Returns `true` while one is open.
    pub fn dialogs(
        &mut self,
        ctx: &egui::Context,
        tx: Tx,
        backend: &dyn Backend,
        id: SessionId,
        peer: &PeerInfo,
    ) -> bool {
        let Some(action) = self.confirm else {
            return false;
        };
        let (title, body, button, icon) = match action {
            SystemAction::Restart => (
                S::RestartConfirmTitle,
                S::RestartConfirmBody,
                S::RestartConfirm,
                Icon::Restart,
            ),
            _ => (
                S::LockConfirmTitle,
                S::LockConfirmBody,
                S::LockConfirm,
                Icon::Lock,
            ),
        };
        let mut decision = None;
        egui::Modal::new(Id::new(("system-confirm", id.0))).show(ctx, |ui| {
            ui.set_width(420.0);
            theme::column(ui, tx.rtl(), |ui| {
                widgets::icon_heading(ui, tx, icon, tx.t(title));
                ui.add_space(4.0);
                ui.label(tx.f(body, &[("name", &peer.name)]));
                if action == SystemAction::Restart {
                    ui.add_space(4.0);
                    ui.label(theme::caption(ui, tx.t(S::RestartingNote)));
                }
                ui.add_space(10.0);
                theme::row(ui, tx.rtl(), |ui| {
                    if ui
                        .add(icons::icon_text_button(tx, Icon::Close, tx.t(S::Cancel)))
                        .clicked()
                    {
                        decision = Some(false);
                    }
                    if ui
                        .add(widgets::danger_icon_button(ui, tx, icon, tx.t(button)))
                        .clicked()
                    {
                        decision = Some(true);
                    }
                });
            });
        });
        match decision {
            Some(true) => {
                self.confirm = None;
                if action == SystemAction::Restart {
                    self.requested_at = Some(Instant::now());
                }
                backend.system_action(id, action);
            }
            Some(false) => self.confirm = None,
            None => {}
        }
        true
    }

    /// Banner / overlay over the remote screen while a restart is underway. Returns `true` if
    /// the user chose to stop waiting.
    pub fn overlay(&mut self, ui: &mut Ui, tx: Tx, area: egui::Rect) -> bool {
        if let Some(at) = self.requested_at {
            if at.elapsed() > REQUESTED_NOTICE {
                self.requested_at = None;
            }
        }
        if !self.restarting {
            if self.requested_at.is_some() {
                let galley = ui.painter().layout_no_wrap(
                    tx.t(S::RestartRequested).to_owned(),
                    egui::FontId::proportional(14.0),
                    Color32::WHITE,
                );
                let size = galley.size() + egui::vec2(24.0, 14.0);
                let rect = egui::Rect::from_center_size(
                    egui::pos2(area.center().x, area.min.y + 12.0 + size.y / 2.0),
                    size,
                );
                ui.painter()
                    .rect_filled(rect, 8.0, Color32::from_black_alpha(200));
                ui.painter()
                    .galley(rect.min + egui::vec2(12.0, 7.0), galley, Color32::WHITE);
                ui.ctx().request_repaint_after(Duration::from_millis(500));
            }
            return false;
        }
        ui.painter()
            .rect_filled(area, 0.0, Color32::from_black_alpha(170));
        let mut stop = false;
        let panel = egui::Rect::from_center_size(area.center(), egui::vec2(440.0, 170.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(panel), |ui| {
            egui::Frame::new()
                .fill(ui.visuals().window_fill)
                .corner_radius(12)
                .inner_margin(16)
                .show(ui, |ui| {
                    theme::column(ui, tx.rtl(), |ui| {
                        theme::row(ui, tx.rtl(), |ui| {
                            ui.spinner();
                            ui.label(RichText::new(tx.t(S::Restarting)).strong());
                        });
                        if self.attempt > 0 {
                            let n = tx.digits(&self.attempt.to_string());
                            ui.label(theme::caption(
                                ui,
                                &tx.f(S::RestartingAttempt, &[("n", &n)]),
                            ));
                        }
                        ui.label(theme::caption(ui, tx.t(S::RestartingNote)));
                        ui.add_space(8.0);
                        if ui
                            .add(icons::icon_text_button(
                                tx,
                                Icon::Close,
                                tx.t(S::StopWaiting),
                            ))
                            .clicked()
                        {
                            stop = true;
                        }
                    });
                });
        });
        ui.ctx().request_repaint_after(Duration::from_millis(500));
        stop
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{AttemptId, ConnectTarget, DeviceId, LocalInfo, RequestId};
    use crate::i18n::{DigitStyle, Lang};
    use std::sync::Mutex;
    use tlp_desk_proto::InputEvent;

    /// Records what the UI asks the backend for.
    #[derive(Default)]
    struct Rec {
        passwords: Mutex<Vec<(Option<String>, Option<String>)>>,
        actions: Mutex<Vec<SystemAction>>,
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
        fn send_input(&self, _: SessionId, _: InputEvent) {}
        fn set_view_only(&self, _: SessionId, _: bool) {}
        fn disconnect(&self, _: SessionId) {}
        fn select_monitor(&self, _: SessionId, _: u32) {}
        fn set_unattended_password(&self, current: Option<String>, new: Option<String>) {
            self.passwords.lock().unwrap().push((current, new));
        }
        fn system_action(&self, _: SessionId, action: SystemAction) {
            self.actions.lock().unwrap().push(action);
        }
    }

    fn run(ctx: &egui::Context, mut f: impl FnMut(&mut Ui)) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 700.0),
            )),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| f(ui));
        });
        out.textures_delta.clear();
    }

    #[test]
    fn password_card_renders_in_both_languages_and_results_show() {
        for lang in [Lang::En, Lang::Ar] {
            let tx = Tx::new(lang, DigitStyle::Western);
            let ctx = egui::Context::default();
            crate::ui::theme::install(&ctx);
            let backend = Rec::default();
            for state in [
                PasswordState::default(),
                PasswordState {
                    set: true,
                    machine_wide: false,
                    admin: false,
                },
                PasswordState {
                    set: true,
                    machine_wide: true,
                    admin: true,
                },
            ] {
                let mut u = UnattendedUi::new(state);
                u.new = "كلمة سر قوية 2026".into();
                u.repeat = u.new.clone();
                run(&ctx, |ui| u.settings_card(ui, tx, &backend));
                u.on_password_changed(tx, Err(PasswordError::WrongCurrent));
                assert!(matches!(u.message, Some((false, _))));
                u.on_password_changed(tx, Ok(PasswordChange::Set));
                assert!(matches!(u.message, Some((true, _))));
                assert!(u.new.is_empty() && u.repeat.is_empty(), "fields cleared");
                run(&ctx, |ui| u.settings_card(ui, tx, &backend));
            }
            assert!(backend.passwords.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn connect_fields_and_system_menu_render_in_both_languages() {
        let backend = Rec::default();
        for lang in [Lang::En, Lang::Ar] {
            let tx = Tx::new(lang, DigitStyle::ArabicIndic);
            let ctx = egui::Context::default();
            crate::ui::theme::install(&ctx);
            let mut u = UnattendedUi::default();
            let mut code = "123456".to_owned();
            run(&ctx, |ui| {
                assert!(!u.connect_fields(ui, tx, &mut code));
            });
            u.use_password = true;
            u.password = "كلمة سر".into();
            run(&ctx, |ui| {
                assert!(!u.connect_fields(ui, tx, &mut code));
            });
            let mut s = SessionUnattended::default();
            for perms in [Permissions::ALL, Permissions::VIEW] {
                run(&ctx, |ui| s.menu(ui, tx, perms, &backend, SessionId(1)));
            }
        }
        // Rendering alone never sends anything.
        assert!(backend.actions.lock().unwrap().is_empty());
    }

    #[test]
    fn strength_text_mentions_minimum_length() {
        let tx = Tx::new(Lang::En, DigitStyle::Western);
        let (text, ok) = strength_text(tx, PasswordStrength::TooShort);
        assert!(!ok);
        assert!(text.contains(&MIN_PASSWORD_CHARS.to_string()), "{text}");
        let ar = Tx::new(Lang::Ar, DigitStyle::ArabicIndic);
        let (text, _) = strength_text(ar, PasswordStrength::Strong);
        assert!(crate::i18n::has_rtl(&text));
    }

    #[test]
    fn lockout_message_names_the_minutes() {
        let en = locked_out_text(Tx::new(Lang::En, DigitStyle::Western));
        assert!(en.contains("10 minutes"), "{en}");
        let ar = locked_out_text(Tx::new(Lang::Ar, DigitStyle::ArabicIndic));
        assert!(ar.contains('١') && ar.contains('٠'), "{ar}");
    }

    #[test]
    fn confirmed_restart_is_sent_and_overlay_renders() {
        let tx = Tx::new(Lang::Ar, DigitStyle::Western);
        let ctx = egui::Context::default();
        crate::ui::theme::install(&ctx);
        let backend = Rec::default();
        let peer = PeerInfo {
            device_id: DeviceId(2),
            name: "حاسوب المكتب".into(),
            address: "192.168.1.2".parse().unwrap(),
        };
        let mut s = SessionUnattended {
            confirm: Some(SystemAction::Restart),
            ..SessionUnattended::default()
        };
        assert!(s.blocks_input());
        // Render the dialog, then answer it like the button would.
        let mut open = false;
        let input = egui::RawInput::default();
        let mut out = ctx.run_ui(input, |ui| {
            open = s.dialogs(ui.ctx(), tx, &backend, SessionId(1), &peer);
        });
        out.textures_delta.clear();
        assert!(open);
        s.confirm = None;
        s.requested_at = Some(Instant::now());
        backend.system_action(SessionId(1), SystemAction::Restart);
        assert_eq!(
            *backend.actions.lock().unwrap(),
            vec![SystemAction::Restart]
        );
        s.on_restarting();
        s.attempt = 3;
        assert!(s.blocks_input());
        run(&ctx, |ui| {
            let r = ui.max_rect();
            assert!(!s.overlay(ui, tx, r));
        });
        s.on_reconnected();
        assert!(!s.blocks_input());
    }
}
