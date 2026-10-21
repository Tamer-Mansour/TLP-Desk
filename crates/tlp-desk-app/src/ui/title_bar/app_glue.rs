//! The app's side of the title bar: drawing it with the current state, carrying out
//! [`TitleAction`]s, the About dialog and the window-level keyboard shortcuts.

use eframe::egui::{self, RichText, ViewportCommand};

use super::chrome::{self, WindowState};
use super::{TitleAction, TitleContext};
use crate::app::{App, Screen};
use crate::i18n::S;
use crate::settings::ThemeChoice;
use crate::ui::address_book::HomeTab;
use crate::ui::{theme, widgets};

impl App {
    /// The title bar (hidden in full screen), the window edges and the About dialog. Call
    /// first in the frame so the bar is the top-most panel.
    pub(crate) fn title_bar_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let window = WindowState::read(&ctx);
        let mut actions = Vec::new();
        self.title_shortcuts(&ctx, window, &mut actions);
        if !window.fullscreen {
            let peer = self.session.as_ref().map(|s| s.peer.name.clone());
            let cx = TitleContext {
                integrated: self.settings.integrated_title_bar,
                session_peer: peer.as_deref(),
                on_home: self.screen == Screen::Home,
                theme: self.settings.theme,
                window,
            };
            let tx = self.tx;
            self.title_bar.show(ui, tx, cx, &mut actions);
        }
        for a in actions {
            self.apply_title_action(&ctx, a, window);
        }
        self.about_dialog(&ctx);
    }

    /// Edge resizing for the frameless window; call last in the frame.
    pub(crate) fn window_edges(&self, ctx: &egui::Context) {
        if self.settings.integrated_title_bar {
            chrome::resize_edges(ctx, WindowState::read(ctx));
        }
    }

    /// Ctrl+N, Ctrl+, F1 and F11. Not during a session: there every key goes to the remote
    /// computer (the session toolbar has its own full-screen button).
    fn title_shortcuts(
        &self,
        ctx: &egui::Context,
        window: WindowState,
        actions: &mut Vec<TitleAction>,
    ) {
        if self.session.is_some() || ctx.egui_wants_keyboard_input() {
            return;
        }
        ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::COMMAND, egui::Key::N) {
                actions.push(TitleAction::NewConnection);
            }
            if i.consume_key(egui::Modifiers::COMMAND, egui::Key::Comma) {
                actions.push(TitleAction::Settings);
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::F1) {
                actions.push(TitleAction::OpenUrl(super::guide_url(
                    self.tx.lang,
                    "index",
                )));
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::F11) && !window.maximized {
                actions.push(TitleAction::ToggleFullscreen);
            }
        });
    }

    pub(crate) fn apply_title_action(
        &mut self,
        ctx: &egui::Context,
        action: TitleAction,
        window: WindowState,
    ) {
        match action {
            TitleAction::NewConnection | TitleAction::Home => {
                self.screen = Screen::Home;
                if action == TitleAction::NewConnection {
                    self.focus_connect = true;
                }
            }
            TitleAction::QuickConnect(text) => {
                self.address = text;
                self.screen = Screen::Home;
                self.connect_from_address();
            }
            TitleAction::Exit => {
                let cmds = self.tray.shell.quit();
                crate::tray::glue::apply_window_cmds(ctx, &cmds);
            }
            // Clipboard always gets Western digits.
            TitleAction::CopyDeviceId => ctx.copy_text(self.local.device_id.to_string()),
            TitleAction::CopyCode => ctx.copy_text(self.local.one_time_code.clone()),
            TitleAction::NewCode => self.backend.refresh_one_time_code(),
            TitleAction::Settings => self.screen = Screen::Settings,
            TitleAction::ConnectionLog => self.open_connection_log(),
            TitleAction::Recordings => {
                self.recordings.open = true;
                self.recordings.reload();
            }
            TitleAction::NearbyDevices => {
                self.screen = Screen::Home;
                self.address_book.tab = HomeTab::Nearby;
            }
            TitleAction::AddressBook => {
                self.screen = Screen::Home;
                self.address_book.tab = HomeTab::AddressBook;
            }
            TitleAction::RemoteDevices => self.screen = Screen::Remote,
            TitleAction::Theme(t) => self.set_theme(ctx, t),
            TitleAction::Lang(l) => self.set_lang(ctx, l),
            TitleAction::Disconnect => {
                let backend = std::sync::Arc::clone(&self.backend);
                if let Some(s) = self.session.as_mut() {
                    s.disconnect(backend.as_ref());
                }
            }
            TitleAction::ToggleFullscreen => {
                ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!window.fullscreen));
            }
            TitleAction::CheckUpdates => {
                self.screen = Screen::Settings;
                self.start_update_check(ctx);
            }
            TitleAction::OpenUrl(url) => ctx.open_url(egui::OpenUrl::new_tab(url)),
            TitleAction::About => self.settings_ui.about_open = true,
            TitleAction::Caption(c) => chrome::apply_caption(ctx, c),
        }
    }

    /// Applies and saves a theme choice.
    pub(crate) fn set_theme(&mut self, ctx: &egui::Context, theme: ThemeChoice) {
        ctx.set_theme(theme.preference());
        if self.settings.theme != theme {
            self.settings.theme = theme;
            self.settings.save();
        }
    }

    /// Switches between the integrated title bar and the system frame, live.
    pub(crate) fn set_integrated_title_bar(&mut self, ctx: &egui::Context, on: bool) {
        if self.settings.integrated_title_bar != on {
            self.settings.integrated_title_bar = on;
            self.settings.save();
        }
        ctx.send_viewport_cmd(ViewportCommand::Decorations(!on));
    }

    fn about_dialog(&mut self, ctx: &egui::Context) {
        if !self.settings_ui.about_open {
            return;
        }
        let tx = self.tx;
        let mut open = true;
        let mut close = false;
        egui::Window::new(tx.t(S::MenuAbout))
            .id(egui::Id::new("about-tlp-desk"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                theme::column(ui, tx.rtl(), |ui| {
                    ui.set_width(340.0);
                    theme::row(ui, tx.rtl(), |ui| {
                        let (mark, _) =
                            ui.allocate_exact_size(egui::vec2(36.0, 36.0), egui::Sense::hover());
                        theme::brand_mark(ui, mark);
                        ui.label(
                            RichText::new(tx.t(S::AppTitle))
                                .size(theme::font::TITLE)
                                .strong(),
                        );
                    });
                    ui.add_space(theme::space::S);
                    let ver = crate::update::version::current().to_string();
                    ui.label(tx.f(S::AboutVersion, &[("ver", &tx.digits(&ver))]));
                    ui.label(theme::caption(ui, tx.t(S::AboutBody)));
                    ui.label(theme::caption(ui, tx.t(S::AboutLicense)));
                    ui.add_space(theme::space::M);
                    if widgets::button(
                        ui,
                        tx,
                        widgets::ButtonKind::Secondary,
                        crate::ui::icons::Icon::Check,
                        tx.t(S::Ok),
                    )
                    .clicked()
                    {
                        close = true;
                    }
                });
            });
        if !open || close {
            self.settings_ui.about_open = false;
        }
    }
}
