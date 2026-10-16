//! Connects the tray, the main-window state machine and the host banner window to [`App`].
//!
//! The real tray icon exists only after [`App::enable_tray`] (called by `main`), so tests and
//! the README media generator never create one.

use eframe::egui;

use super::model::{tooltip, TrayAction};
use super::shell::{Shell, WindowCmd};
use crate::app::{App, Screen};
use crate::banner::{view as banner_view, BannerModel};

/// Tray and window-mode state owned by the app.
pub struct TrayState {
    /// Where the main window is.
    pub shell: Shell,
    /// The icon, once created.
    #[cfg(windows)]
    pub system: Option<super::SystemTray>,
    /// The first frame ran (start-minimized is applied once).
    started: bool,
    /// This run was started to open a session (`--connect`): the first frame shows the
    /// window normally instead of applying start-minimized.
    pub show_on_start: bool,
    /// Name of the newest unanswered incoming request, for the tooltip.
    pub request: Option<String>,
}

impl Default for TrayState {
    fn default() -> Self {
        Self {
            shell: Shell::new(false),
            #[cfg(windows)]
            system: None,
            started: false,
            show_on_start: false,
            request: None,
        }
    }
}

/// Turn window commands into viewport commands.
pub fn apply_window_cmds(ctx: &egui::Context, cmds: &[WindowCmd]) {
    for cmd in cmds {
        match cmd {
            WindowCmd::Restore => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false)),
            WindowCmd::Focus => ctx.send_viewport_cmd(egui::ViewportCommand::Focus),
            WindowCmd::Attention => ctx.send_viewport_cmd(
                egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Critical),
            ),
            // Minimized, not hidden: a hidden eframe window stops running its UI pass, and the
            // host banner window is drawn by that pass.
            WindowCmd::ToTray => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
            WindowCmd::CancelClose => ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose),
            WindowCmd::Close => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }
}

impl TrayState {
    /// A real tray icon exists (so the window can live in the tray).
    pub fn system_present(&self) -> bool {
        #[cfg(windows)]
        {
            self.system.is_some()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}

impl App {
    /// Create the notification-area icon (the GUI binary only). Without it, close always quits.
    #[cfg(windows)]
    pub fn enable_tray(&mut self, ctx: &egui::Context) {
        let tip = self.tray_tooltip();
        match super::SystemTray::new(
            ctx,
            self.settings.lang,
            self.settings.host.incoming_enabled,
            &tip,
        ) {
            Ok(t) => {
                self.tray.system = Some(t);
                self.tray.shell = Shell::new(true);
            }
            Err(e) => tracing::warn!("no tray icon: {e:#}"),
        }
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    fn tray_tooltip(&self) -> String {
        let id = self.local.device_id.to_string();
        tooltip(
            self.settings.lang,
            &id,
            self.settings.host.incoming_enabled,
            self.tray.request.as_deref(),
        )
    }

    /// An Accept prompt arrived: show the window (it must be seen), or flash it if it is open.
    pub(crate) fn tray_incoming_request(&mut self, ctx: &egui::Context, name: &str) {
        self.tray.request = Some(crate::banner::sanitize_name(name));
        let cmds = self.tray.shell.incoming_request();
        apply_window_cmds(ctx, &cmds);
    }

    /// Once per frame: first-frame start-minimized, close requests, tray clicks and the
    /// tooltip. Returns without effect when there is no tray icon.
    pub(crate) fn tray_frame(&mut self, ctx: &egui::Context) {
        if !self.tray.started {
            self.tray.started = true;
            let cmds = if self.tray.show_on_start {
                self.tray.shell.startup_for_session()
            } else {
                self.tray
                    .shell
                    .startup(self.settings.startup.start_minimized || self.start_hidden)
            };
            apply_window_cmds(ctx, &cmds);
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            let cmds = self
                .tray
                .shell
                .close_requested(self.settings.startup.close_to_tray);
            apply_window_cmds(ctx, &cmds);
        }
        if self.incoming.is_empty() {
            self.tray.request = None;
        }
        #[cfg(windows)]
        {
            let actions = self
                .tray
                .system
                .as_ref()
                .map(|t| t.poll())
                .unwrap_or_default();
            for a in actions {
                self.tray_action(ctx, a);
            }
            let tip = self.tray_tooltip();
            let (lang, incoming) = (self.settings.lang, self.settings.host.incoming_enabled);
            if let Some(t) = self.tray.system.as_mut() {
                t.update(lang, incoming, &tip);
            }
        }
    }

    /// Run one tray menu choice.
    pub fn tray_action(&mut self, ctx: &egui::Context, action: TrayAction) {
        match action {
            TrayAction::Open => {
                let cmds = self.tray.shell.open();
                apply_window_cmds(ctx, &cmds);
            }
            TrayAction::CopyId => {
                let id = self.local.device_id.to_string();
                ctx.copy_text(id.chars().filter(char::is_ascii_digit).collect());
            }
            TrayAction::NewCode => self.backend.refresh_one_time_code(),
            TrayAction::ToggleIncoming => {
                self.settings.host.incoming_enabled = !self.settings.host.incoming_enabled;
                self.host_settings_changed();
            }
            TrayAction::Settings => {
                self.screen = Screen::Settings;
                let cmds = self.tray.shell.open();
                apply_window_cmds(ctx, &cmds);
            }
            TrayAction::ConnectionLog => {
                self.open_connection_log();
                let cmds = self.tray.shell.open();
                apply_window_cmds(ctx, &cmds);
            }
            TrayAction::Quit => {
                let cmds = self.tray.shell.quit();
                apply_window_cmds(ctx, &cmds);
            }
        }
    }

    /// The always-visible host banner window, shown while a viewer is connected and the main
    /// window is not in front (in the tray or minimized). With the main window visible, the
    /// banner strip inside it does the same job.
    pub(crate) fn banner_window(&mut self, ctx: &egui::Context) {
        let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        let hidden = minimized || self.tray.shell.mode() == super::WindowMode::InTray;
        if !hidden {
            return;
        }
        let Some(model) = BannerModel::from_sessions(&self.host_sessions) else {
            return;
        };
        if let Some(session) = banner_view::show(ctx, self.tx, &model) {
            self.backend.disconnect(session);
        }
    }
}
