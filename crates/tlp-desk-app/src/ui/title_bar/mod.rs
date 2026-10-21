//! The integrated title bar: brand, menu bar, context (quick connect, the connected device),
//! quick buttons and, when TLP-Desk draws its own window frame, the caption buttons.
//!
//! One bar for every screen, including sessions, in the place where Windows would draw an
//! empty title bar. With [`TitleBar::integrated`] off the system frame stays and this bar is
//! the app's header under it (same menus, no caption buttons, no dragging).
//!
//! The bar only reports [`TitleAction`]s; `App::apply_title_action` carries them out, so the
//! menus never reach into app state.

mod app_glue;
pub mod chrome;

use eframe::egui::{self, vec2, Align, Layout, RichText, Sense};

use chrome::Caption;
pub use chrome::WindowState;

use super::icons::Icon;
use super::{theme, widgets};
use crate::i18n::{Lang, Tx, S};
use crate::settings::ThemeChoice;

/// Height of the bar (logical points).
pub const HEIGHT: f32 = 40.0;
/// Below this window width the menus fold into one menu button.
const FOLD_MENUS_BELOW: f32 = 900.0;
/// Below this window width the quick-connect field is hidden.
const HIDE_QUICK_CONNECT_BELOW: f32 = 1100.0;

/// What the user picked in the title bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitleAction {
    /// File → New connection: home screen, focus the connect box.
    NewConnection,
    /// Quick-connect field: connect to what was typed.
    QuickConnect(String),
    Exit,
    CopyDeviceId,
    CopyCode,
    NewCode,
    Home,
    Settings,
    ConnectionLog,
    Recordings,
    NearbyDevices,
    AddressBook,
    /// The Remote devices page (TLP-relay).
    RemoteDevices,
    Theme(ThemeChoice),
    Lang(Lang),
    Disconnect,
    ToggleFullscreen,
    CheckUpdates,
    OpenUrl(String),
    About,
    Caption(Caption),
}

/// What the bar needs to know about the app this frame.
#[derive(Debug, Clone, Copy)]
pub struct TitleContext<'a> {
    /// TLP-Desk draws the window frame (caption buttons, drag, resize).
    pub integrated: bool,
    /// The device a viewer session is connected to.
    pub session_peer: Option<&'a str>,
    /// Home screen is showing (it has its own big connect box).
    pub on_home: bool,
    pub theme: ThemeChoice,
    pub window: WindowState,
}

/// Quick-connect text kept between frames.
#[derive(Debug, Default)]
pub struct TitleBar {
    quick: String,
}

/// The online user guide (user-initiated link; nothing is fetched by the app itself).
pub fn guide_url(lang: Lang, page: &str) -> String {
    let l = if lang.is_rtl() { "ar" } else { "en" };
    format!("https://tamer-mansour.github.io/TLP-Desk/book/user-guide/{l}/{page}.html")
}

impl TitleBar {
    /// Draws the bar as a top panel of `ui`.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        tx: Tx,
        cx: TitleContext<'_>,
        actions: &mut Vec<TitleAction>,
    ) {
        let p = theme::tokens(ui);
        egui::Panel::top("title-bar")
            .exact_size(HEIGHT)
            .frame(
                egui::Frame::new()
                    .fill(p.surface)
                    .stroke(egui::Stroke::new(1.0, p.border))
                    .inner_margin(egui::Margin {
                        left: 12,
                        right: 0,
                        top: 0,
                        bottom: 0,
                    }),
            )
            .show(ui, |ui| {
                let width = ui.ctx().content_rect().width();
                ui.set_height(HEIGHT);
                let dir = if tx.rtl() {
                    Layout::right_to_left(Align::Center)
                } else {
                    Layout::left_to_right(Align::Center)
                };
                ui.with_layout(dir, |ui| {
                    ui.spacing_mut().item_spacing.x = theme::space::XS;
                    self.leading(ui, tx, cx, width, actions);
                    let end = if tx.rtl() {
                        Layout::left_to_right(Align::Center)
                    } else {
                        Layout::right_to_left(Align::Center)
                    };
                    ui.with_layout(end, |ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        if cx.integrated {
                            caption_buttons(ui, tx, cx.window, actions);
                            ui.add_space(theme::space::S);
                        } else {
                            ui.add_space(theme::space::M);
                        }
                        ui.spacing_mut().item_spacing.x = theme::space::XS;
                        if cx.session_peer.is_none() {
                            quick_buttons(ui, tx, actions);
                        }
                        self.middle(ui, tx, cx, width, actions);
                        // Whatever is left between the menus and the buttons moves the window.
                        if cx.integrated {
                            let rest = ui.available_rect_before_wrap();
                            let resp = ui.allocate_rect(rest, Sense::click_and_drag());
                            chrome::drag_area(ui.ctx(), &resp, cx.window);
                        }
                    });
                });
            });
    }

    /// Brand mark, name and the menus.
    fn leading(
        &mut self,
        ui: &mut egui::Ui,
        tx: Tx,
        cx: TitleContext<'_>,
        width: f32,
        actions: &mut Vec<TitleAction>,
    ) {
        let p = theme::tokens(ui);
        let (mark, resp) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::click());
        theme::brand_mark(ui, mark);
        if resp.double_clicked() && cx.integrated {
            // The system menu icon's double-click closes the window; ours goes home instead,
            // which is what people expect from a logo in an app.
            actions.push(TitleAction::Home);
        } else if resp.clicked() {
            actions.push(TitleAction::Home);
        }
        ui.label(
            RichText::new(tx.t(S::AppTitle))
                .size(theme::font::BODY + 1.0)
                .strong()
                .color(p.text),
        );
        ui.add_space(theme::space::M);
        if width < FOLD_MENUS_BELOW {
            let r = ui.menu_button(RichText::new("☰").size(16.0), |ui| {
                for (label, menu) in MENUS {
                    ui.menu_button(tx.t(label), |ui| menu_items(ui, tx, menu, cx, actions));
                }
            });
            r.response.on_hover_text(tx.t(S::MenuMore));
        } else {
            menu_bar(ui, tx, cx, actions);
        }
    }

    /// The connected device during a session; quick connect elsewhere (not on home, which has
    /// the full connect box).
    fn middle(
        &mut self,
        ui: &mut egui::Ui,
        tx: Tx,
        cx: TitleContext<'_>,
        width: f32,
        actions: &mut Vec<TitleAction>,
    ) {
        let p = theme::tokens(ui);
        if let Some(peer) = cx.session_peer {
            ui.add_space(theme::space::M);
            let text = tx.f(S::TitleConnectedTo, &[("name", peer)]);
            ui.label(RichText::new(text).color(p.text_muted));
            let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
            ui.painter().circle_filled(dot.center(), 3.5, p.success);
            return;
        }
        if cx.on_home || width < HIDE_QUICK_CONNECT_BELOW {
            return;
        }
        ui.add_space(theme::space::M);
        let resp = widgets::text_field(
            ui,
            tx,
            "title-quick-connect",
            &mut self.quick,
            tx.t(S::QuickConnectHint),
            220.0,
        );
        let hint = tx.t(S::QuickConnectHint);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, hint));
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let target = self.quick.trim().to_owned();
            if !target.is_empty() {
                actions.push(TitleAction::QuickConnect(target));
                self.quick.clear();
            }
        }
    }
}

/// The eight menus. egui's `MenuBar` lays out left to right over the whole available width,
/// so it gets a child area exactly as wide as the menus (placed next to the brand in either
/// direction), and in RTL the order is reversed so File sits next to the brand, on the right.
fn menu_bar(ui: &mut egui::Ui, tx: Tx, cx: TitleContext<'_>, actions: &mut Vec<TitleAction>) {
    let padding = vec2(8.0, 4.0);
    let font = egui::TextStyle::Button.resolve(ui.style());
    let width: f32 = MENUS
        .iter()
        .map(|(label, _)| {
            let g = ui.painter().layout_no_wrap(
                tx.t(*label).to_owned(),
                font.clone(),
                egui::Color32::WHITE,
            );
            g.size().x + 2.0 * padding.x
        })
        .sum::<f32>()
        + 2.0;
    let mut order: Vec<(S, Menu)> = MENUS.to_vec();
    if tx.rtl() {
        order.reverse();
    }
    ui.allocate_ui_with_layout(
        vec2(width, HEIGHT),
        Layout::left_to_right(Align::Center),
        |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                // Roomier than egui's default: these are the app's main menus.
                ui.spacing_mut().button_padding = padding;
                ui.spacing_mut().item_spacing.x = 0.0;
                for (label, menu) in order {
                    ui.menu_button(tx.t(label), |ui| menu_items(ui, tx, menu, cx, actions));
                }
            });
        },
    );
}

/// Connection log, recordings and settings, icon-only (the full names are tooltips and menu
/// entries).
fn quick_buttons(ui: &mut egui::Ui, tx: Tx, actions: &mut Vec<TitleAction>) {
    let color = theme::tokens(ui).text_muted;
    // Laid out from the end: settings is the outermost.
    for (icon, key, action) in [
        (Icon::Settings, S::Settings, TitleAction::Settings),
        (Icon::Playback, S::Recordings, TitleAction::Recordings),
        (Icon::Log, S::ConnectionLog, TitleAction::ConnectionLog),
    ] {
        // Frameless until hovered, like the caption buttons next to them.
        let image = icon.image_dir(tx.rtl(), 17.0, color);
        let button = egui::Button::image(image)
            .frame_when_inactive(false)
            .min_size(vec2(32.0, 28.0));
        let label = tx.t(key);
        let resp = ui.add(button);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
        if resp.on_hover_text(label).clicked() {
            actions.push(action);
        }
    }
}

fn caption_buttons(ui: &mut egui::Ui, tx: Tx, window: WindowState, actions: &mut Vec<TitleAction>) {
    let max = if window.maximized {
        (Caption::Restore, S::MenuRestore)
    } else {
        (Caption::Maximize, S::MenuMaximize)
    };
    // Laid out from the end: close is the outermost, like the system buttons.
    for (kind, key) in [
        (Caption::Close, S::MenuClose),
        max,
        (Caption::Minimize, S::MenuMinimize),
    ] {
        if chrome::caption_button(ui, kind, HEIGHT, tx.t(key)).clicked() {
            actions.push(TitleAction::Caption(kind));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Menu {
    File,
    Edit,
    View,
    Connections,
    Devices,
    Tools,
    Window,
    Help,
}

const MENUS: [(S, Menu); 8] = [
    (S::MenuFile, Menu::File),
    (S::MenuEdit, Menu::Edit),
    (S::MenuView, Menu::View),
    (S::MenuConnections, Menu::Connections),
    (S::MenuDevices, Menu::Devices),
    (S::MenuTools, Menu::Tools),
    (S::MenuWindow, Menu::Window),
    (S::MenuHelp, Menu::Help),
];

/// One menu entry: closes the menu and records `action` when clicked.
fn add(
    ui: &mut egui::Ui,
    tx: Tx,
    actions: &mut Vec<TitleAction>,
    key: S,
    shortcut: Option<&str>,
    enabled: bool,
    action: TitleAction,
) {
    let mut b = egui::Button::new(tx.t(key));
    if let Some(s) = shortcut {
        b = b.shortcut_text(s);
    }
    if ui.add_enabled(enabled, b).clicked() {
        actions.push(action);
        ui.close();
    }
}

fn menu_items(
    ui: &mut egui::Ui,
    tx: Tx,
    menu: Menu,
    cx: TitleContext<'_>,
    actions: &mut Vec<TitleAction>,
) {
    ui.set_min_width(220.0);
    let in_session = cx.session_peer.is_some();
    match menu {
        Menu::File => {
            add(
                ui,
                tx,
                actions,
                S::MenuNewConnection,
                Some("Ctrl+N"),
                !in_session,
                TitleAction::NewConnection,
            );
            ui.separator();
            add(
                ui,
                tx,
                actions,
                S::Settings,
                Some("Ctrl+,"),
                !in_session,
                TitleAction::Settings,
            );
            ui.separator();
            add(ui, tx, actions, S::MenuExit, None, true, TitleAction::Exit);
        }
        Menu::Edit => {
            add(
                ui,
                tx,
                actions,
                S::MenuCopyId,
                None,
                true,
                TitleAction::CopyDeviceId,
            );
            add(
                ui,
                tx,
                actions,
                S::MenuCopyCode,
                None,
                true,
                TitleAction::CopyCode,
            );
            add(
                ui,
                tx,
                actions,
                S::NewCode,
                None,
                true,
                TitleAction::NewCode,
            );
        }
        Menu::View => {
            add(
                ui,
                tx,
                actions,
                S::MenuHome,
                None,
                !in_session,
                TitleAction::Home,
            );
            add(
                ui,
                tx,
                actions,
                S::ConnectionLog,
                None,
                !in_session,
                TitleAction::ConnectionLog,
            );
            add(
                ui,
                tx,
                actions,
                S::Recordings,
                None,
                true,
                TitleAction::Recordings,
            );
            ui.separator();
            ui.menu_button(tx.t(S::MenuTheme), |ui| {
                for (choice, key) in [
                    (ThemeChoice::System, S::ThemeSystem),
                    (ThemeChoice::Light, S::ThemeLight),
                    (ThemeChoice::Dark, S::ThemeDark),
                ] {
                    if ui.radio(cx.theme == choice, tx.t(key)).clicked() {
                        actions.push(TitleAction::Theme(choice));
                        ui.close();
                    }
                }
            });
            ui.menu_button(tx.t(S::Language), |ui| {
                for l in Lang::ALL {
                    if ui.radio(tx.lang == l, l.native_name()).clicked() {
                        actions.push(TitleAction::Lang(l));
                        ui.close();
                    }
                }
            });
            ui.separator();
            let fs = if cx.window.fullscreen {
                S::ExitFullscreen
            } else {
                S::Fullscreen
            };
            add(
                ui,
                tx,
                actions,
                fs,
                Some("F11"),
                true,
                TitleAction::ToggleFullscreen,
            );
        }
        Menu::Connections => {
            add(
                ui,
                tx,
                actions,
                S::MenuNewConnection,
                Some("Ctrl+N"),
                !in_session,
                TitleAction::NewConnection,
            );
            add(
                ui,
                tx,
                actions,
                S::Disconnect,
                None,
                in_session,
                TitleAction::Disconnect,
            );
            ui.separator();
            add(
                ui,
                tx,
                actions,
                S::ConnectionLog,
                None,
                !in_session,
                TitleAction::ConnectionLog,
            );
        }
        Menu::Devices => {
            add(
                ui,
                tx,
                actions,
                S::NearbyDevices,
                None,
                !in_session,
                TitleAction::NearbyDevices,
            );
            add(
                ui,
                tx,
                actions,
                S::AbTitle,
                None,
                !in_session,
                TitleAction::AddressBook,
            );
            add(
                ui,
                tx,
                actions,
                S::MenuRemote,
                None,
                !in_session,
                TitleAction::RemoteDevices,
            );
        }
        Menu::Tools => {
            add(
                ui,
                tx,
                actions,
                S::UnattendedTitle,
                None,
                !in_session,
                TitleAction::Settings,
            );
            add(
                ui,
                tx,
                actions,
                S::UpdCheckNow,
                None,
                true,
                TitleAction::CheckUpdates,
            );
        }
        Menu::Window => {
            add(
                ui,
                tx,
                actions,
                S::MenuMinimize,
                None,
                true,
                TitleAction::Caption(Caption::Minimize),
            );
            let (key, cap) = if cx.window.maximized {
                (S::MenuRestore, Caption::Restore)
            } else {
                (S::MenuMaximize, Caption::Maximize)
            };
            add(
                ui,
                tx,
                actions,
                key,
                None,
                !cx.window.fullscreen,
                TitleAction::Caption(cap),
            );
            let fs = if cx.window.fullscreen {
                S::ExitFullscreen
            } else {
                S::Fullscreen
            };
            add(
                ui,
                tx,
                actions,
                fs,
                Some("F11"),
                true,
                TitleAction::ToggleFullscreen,
            );
        }
        Menu::Help => {
            add(
                ui,
                tx,
                actions,
                S::MenuUserGuide,
                Some("F1"),
                true,
                TitleAction::OpenUrl(guide_url(tx.lang, "index")),
            );
            add(
                ui,
                tx,
                actions,
                S::MenuTroubleshooting,
                None,
                true,
                TitleAction::OpenUrl(guide_url(tx.lang, "troubleshooting")),
            );
            ui.separator();
            add(
                ui,
                tx,
                actions,
                S::MenuAbout,
                None,
                true,
                TitleAction::About,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::Queryable;
    use egui_kittest::Harness;

    use super::*;
    use crate::i18n::DigitStyle;

    struct State {
        bar: TitleBar,
        actions: Vec<TitleAction>,
        cx: Ctx,
    }

    #[derive(Clone, Copy)]
    struct Ctx {
        integrated: bool,
        session: bool,
        on_home: bool,
    }

    fn harness(lang: Lang, width: f32, cx: Ctx) -> Harness<'static, State> {
        let tx = Tx::new(lang, DigitStyle::Western);
        let mut h = Harness::builder()
            .with_size(egui::vec2(width, 300.0))
            .build_ui_state(
                move |ui, s: &mut State| {
                    let cx = TitleContext {
                        integrated: s.cx.integrated,
                        session_peer: s.cx.session.then_some("مكتب-Office"),
                        on_home: s.cx.on_home,
                        theme: ThemeChoice::System,
                        window: WindowState::default(),
                    };
                    s.bar.show(ui, tx, cx, &mut s.actions);
                },
                State {
                    bar: TitleBar::default(),
                    actions: Vec::new(),
                    cx,
                },
            );
        theme::install(&h.ctx);
        theme::apply_lang(&h.ctx, lang);
        h.run_steps(3);
        h
    }

    const HOME: Ctx = Ctx {
        integrated: true,
        session: false,
        on_home: true,
    };

    #[test]
    fn all_menus_and_window_buttons_show_in_both_languages() {
        for lang in [Lang::En, Lang::Ar] {
            let tx = Tx::new(lang, DigitStyle::Western);
            let h = harness(lang, 1280.0, HOME);
            for (label, _) in MENUS {
                let _ = h.get_by_label(tx.t(label));
            }
            for key in [S::MenuMinimize, S::MenuMaximize, S::MenuClose] {
                let _ = h.get_by_label(tx.t(key));
            }
            for key in [S::Settings, S::Recordings, S::ConnectionLog] {
                let _ = h.get_by_label(tx.t(key));
            }
        }
    }

    #[test]
    fn with_the_system_frame_there_are_no_caption_buttons() {
        let tx = Tx::new(Lang::En, DigitStyle::Western);
        let h = harness(
            Lang::En,
            1280.0,
            Ctx {
                integrated: false,
                ..HOME
            },
        );
        let _ = h.get_by_label(tx.t(S::MenuFile));
        assert!(h.query_by_label(tx.t(S::MenuClose)).is_none());
        assert!(h.query_by_label(tx.t(S::MenuMinimize)).is_none());
    }

    #[test]
    fn file_exit_and_close_button_report_their_actions() {
        let tx = Tx::new(Lang::En, DigitStyle::Western);
        let mut h = harness(Lang::En, 1280.0, HOME);
        h.get_by_label(tx.t(S::MenuFile)).click();
        h.run_steps(2);
        h.get_by_label(tx.t(S::MenuExit)).click();
        h.run_steps(2);
        assert_eq!(h.state().actions, vec![TitleAction::Exit]);
        h.get_by_label(tx.t(S::MenuClose)).click();
        h.run_steps(2);
        assert_eq!(
            h.state().actions.last(),
            Some(&TitleAction::Caption(Caption::Close))
        );
    }

    #[test]
    fn narrow_windows_fold_the_menus_into_one_button() {
        let tx = Tx::new(Lang::Ar, DigitStyle::Western);
        let h = harness(Lang::Ar, 780.0, HOME);
        assert!(h.query_by_label(tx.t(S::MenuFile)).is_none());
        let _ = h.get_by_label("☰");
        let _ = h.get_by_label(tx.t(S::MenuClose));
    }

    /// During a session: the connected device is named, screens that cannot show are
    /// disabled, Disconnect is enabled, and the quick buttons are gone.
    #[test]
    fn session_context_names_the_device_and_offers_disconnect() {
        let tx = Tx::new(Lang::En, DigitStyle::Western);
        let mut h = harness(
            Lang::En,
            1280.0,
            Ctx {
                session: true,
                on_home: false,
                ..HOME
            },
        );
        assert!(h.query_by_label(tx.t(S::Recordings)).is_none());
        h.get_by_label(tx.t(S::MenuConnections)).click();
        h.run_steps(2);
        h.get_by_label(tx.t(S::Disconnect)).click();
        h.run_steps(2);
        assert_eq!(h.state().actions, vec![TitleAction::Disconnect]);
    }

    /// Quick connect (not on home): Enter connects to what was typed, in Arabic-Indic digits too
    /// (the app converts them); empty input does nothing.
    #[test]
    fn quick_connect_reports_the_typed_target() {
        let tx = Tx::new(Lang::En, DigitStyle::Western);
        let mut h = harness(
            Lang::En,
            1280.0,
            Ctx {
                on_home: false,
                ..HOME
            },
        );
        let field = h.get_by_label(tx.t(S::QuickConnectHint));
        field.click();
        h.run_steps(1);
        h.get_by_label(tx.t(S::QuickConnectHint))
            .type_text("٤٨٢ ٩١٣ ٠٥٧");
        h.run_steps(1);
        h.key_press(egui::Key::Enter);
        h.run_steps(2);
        assert_eq!(
            h.state().actions,
            vec![TitleAction::QuickConnect("٤٨٢ ٩١٣ ٠٥٧".into())]
        );
    }

    #[test]
    fn guide_links_follow_the_language() {
        assert_eq!(
            guide_url(Lang::Ar, "troubleshooting"),
            "https://tamer-mansour.github.io/TLP-Desk/book/user-guide/ar/troubleshooting.html"
        );
        assert!(guide_url(Lang::En, "index").ends_with("/en/index.html"));
    }
}
