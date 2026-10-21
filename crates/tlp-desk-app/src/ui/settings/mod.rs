//! Settings screen, one card per section: language and display, this device, incoming
//! connections, allowed networks, picture quality, start-up and notification area.
//!
//! Every change is saved to `settings.json` at once and applied live where possible: host
//! options and the allow-list go to the backend ([`crate::backend::Backend::apply_host_settings`]),
//! the quality preset to a running viewer session, start-up options to the `Run` key.

mod allow_list;
pub(crate) mod incoming;
mod startup;

pub(crate) use incoming::PERMISSION_LABELS;
pub(crate) use startup::quality_key;

use eframe::egui;

use super::icons::Icon;
use super::{shell, theme, widgets};
use crate::app::App;
use crate::i18n::{DigitStyle, Lang, Tx, S};
use crate::settings::allow_list::CidrError;
use crate::settings::ThemeChoice;

/// Transient state of the settings screen (not persisted).
#[derive(Debug, Default)]
pub struct SettingsUi {
    /// Text typed into the allow-list editor.
    pub allow_input: String,
    /// Why the last entry was refused.
    pub allow_error: Option<CidrError>,
    /// A public range waiting for "Add anyway".
    pub pending_public: Option<String>,
    /// The last start-with-Windows change failed (display-ready).
    pub autostart_error: Option<String>,
    /// Scroll the screen to this offset on the next frame (README media only).
    pub scroll_offset: Option<f32>,
    /// Help → About is open.
    pub about_open: bool,
    /// Which section is shown in the two-pane settings page.
    pub section: SettingsSection,
}

/// Sections of the settings page, in nav order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SettingsSection {
    /// Language, digits, theme, title bar, device name.
    #[default]
    General,
    /// Incoming connections, the unattended password, allowed networks.
    Security,
    /// Picture quality, clipboard, typing, sound.
    Session,
    /// Start-up and the notification area.
    Startup,
    /// Update check.
    Updates,
}

impl SettingsSection {
    pub const ALL: [SettingsSection; 5] = [
        Self::General,
        Self::Security,
        Self::Session,
        Self::Startup,
        Self::Updates,
    ];

    fn label(self) -> S {
        match self {
            Self::General => S::SectionGeneral,
            Self::Security => S::SectionSecurity,
            Self::Session => S::SectionSession,
            Self::Startup => S::SettingsStartup,
            Self::Updates => S::UpdSection,
        }
    }

    fn icon(self) -> Icon {
        match self {
            Self::General => Icon::Settings,
            Self::Security => Icon::ShieldCheck,
            Self::Session => Icon::Monitor,
            Self::Startup => Icon::Power,
            Self::Updates => Icon::Download,
        }
    }
}

/// An on/off switch whose label sits on the reading side (RTL-aware); returns `true` if toggled.
/// Accessibility (and the UI tests) still see a checkbox with the label as its name.
pub(crate) fn check_row(ui: &mut egui::Ui, tx: Tx, value: &mut bool, text: &str) -> bool {
    theme::row(ui, tx.rtl(), |ui| {
        widgets::switch(ui, tx, value, text).changed()
    })
}

impl App {
    /// The whole settings screen: a section list on the reading-start side, the chosen
    /// section's cards on the other side (one column when the window is narrow).
    pub fn settings_ui(&mut self, ui: &mut egui::Ui) {
        let tx = self.tx;
        shell::canvas(ui, |ui| {
            shell::page_title(ui, tx, tx.t(S::Settings), None);
            let wide = ui.available_width() >= 760.0;
            let nav_w = if wide { 244.0 } else { ui.available_width() };
            let gap = theme::space::XL;
            let content_w = if wide {
                ui.available_width() - nav_w - gap
            } else {
                ui.available_width()
            };
            let draw_nav = |ui: &mut egui::Ui, this: &mut Self| {
                theme::glass(ui, |ui| {
                    theme::column(ui, tx.rtl(), |ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        for sec in SettingsSection::ALL {
                            let selected = this.settings_ui.section == sec;
                            let resp = nav_item(ui, tx, sec.icon(), tx.t(sec.label()), selected);
                            if resp.clicked() {
                                this.settings_ui.section = sec;
                            }
                        }
                    });
                });
            };
            let draw_content = |ui: &mut egui::Ui, this: &mut Self| {
                let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
                if let Some(y) = this.settings_ui.scroll_offset.take() {
                    area = area.vertical_scroll_offset(y);
                }
                area.show(ui, |ui| {
                    theme::column(ui, tx.rtl(), |ui| {
                        ui.set_width(ui.available_width());
                        this.settings_section(ui);
                    });
                });
            };
            if wide {
                let col_h = ui.available_height();
                theme::row(ui, tx.rtl(), |ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(nav_w, col_h),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_width(nav_w);
                            draw_nav(ui, self);
                        },
                    );
                    ui.add_space(gap - ui.spacing().item_spacing.x);
                    ui.allocate_ui_with_layout(
                        egui::vec2(content_w, col_h),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_width(content_w);
                            draw_content(ui, self);
                        },
                    );
                });
            } else {
                draw_nav(ui, self);
                ui.add_space(theme::space::L);
                draw_content(ui, self);
            }
        });
    }

    /// The cards of the current section.
    fn settings_section(&mut self, ui: &mut egui::Ui) {
        let tx = self.tx;
        let gap = |ui: &mut egui::Ui| ui.add_space(theme::space::L);
        match self.settings_ui.section {
            SettingsSection::General => {
                self.appearance_card(ui);
                gap(ui);
                self.device_card_settings(ui);
            }
            SettingsSection::Security => {
                self.incoming_card(ui);
                gap(ui);
                // Permanent password for unattended access (TLP-unattended).
                self.unattended.settings_card(ui, tx, self.backend.as_ref());
                gap(ui);
                self.allow_list_card(ui);
            }
            SettingsSection::Session => {
                self.quality_card(ui);
                gap(ui);
                crate::ui::clipboard::settings_card(
                    ui,
                    tx,
                    &mut self.settings,
                    self.backend.as_ref(),
                );
                gap(ui);
                crate::ui::typing::settings_card(ui, tx, &mut self.settings);
                if let Some(view) = self.session.as_mut() {
                    view.typing
                        .set_route(crate::text_route::TextRoute::from_layout_setting(
                            self.settings.type_with_my_layout,
                        ));
                }
                gap(ui);
                crate::ui::audio::settings_card(ui, tx, &mut self.settings, self.backend.as_ref());
            }
            SettingsSection::Startup => self.startup_card(ui),
            SettingsSection::Updates => self.updates_card(ui),
        }
    }

    /// Persist the settings and push the host part to the backend.
    pub(crate) fn host_settings_changed(&mut self) {
        self.settings.host.sanitize();
        self.settings.save();
        self.backend.apply_host_settings(&self.settings.host);
    }

    fn appearance_card(&mut self, ui: &mut egui::Ui) {
        let tx = self.tx;
        let mut new_lang = None;
        let mut new_digits = None;
        let mut new_theme = None;
        let mut integrated = self.settings.integrated_title_bar;
        theme::card(ui, |ui| {
            theme::column(ui, tx.rtl(), |ui| {
                widgets::icon_heading(ui, tx, Icon::Language, tx.t(S::Appearance));
                ui.add_space(6.0);
                ui.label(theme::caption(ui, tx.t(S::Language)));
                let mut lang = tx.lang;
                let langs: Vec<_> = Lang::ALL
                    .iter()
                    .map(|l| (*l, None, l.native_name()))
                    .collect();
                if widgets::segmented(ui, tx, &mut lang, &langs) {
                    new_lang = Some(lang);
                }
                ui.add_space(theme::space::S);
                ui.label(theme::caption(ui, tx.t(S::Digits)));
                let mut digits = tx.digits;
                let styles: Vec<_> = DigitStyle::ALL
                    .iter()
                    .map(|d| {
                        let key = match d {
                            DigitStyle::Western => S::DigitsWestern,
                            DigitStyle::ArabicIndic => S::DigitsArabicIndic,
                        };
                        (*d, None, tx.t(key))
                    })
                    .collect();
                if widgets::segmented(ui, tx, &mut digits, &styles) {
                    new_digits = Some(digits);
                }
                ui.label(theme::caption(ui, tx.t(S::DigitsHint)));
                ui.add_space(theme::space::S);
                ui.label(theme::caption(ui, tx.t(S::MenuTheme)));
                let mut choice = self.settings.theme;
                let themes = [
                    (ThemeChoice::System, None, tx.t(S::ThemeSystem)),
                    (ThemeChoice::Light, Some(Icon::Sun), tx.t(S::ThemeLight)),
                    (ThemeChoice::Dark, Some(Icon::Moon), tx.t(S::ThemeDark)),
                ];
                if widgets::segmented(ui, tx, &mut choice, &themes) {
                    new_theme = Some(choice);
                }
                ui.add_space(theme::space::S);
                check_row(ui, tx, &mut integrated, tx.t(S::IntegratedTitleBar));
                ui.label(theme::caption(ui, tx.t(S::IntegratedTitleBarHint)));
            });
        });
        if let Some(t) = new_theme {
            self.set_theme(ui.ctx(), t);
        }
        if integrated != self.settings.integrated_title_bar {
            self.set_integrated_title_bar(ui.ctx(), integrated);
        }
        if let Some(l) = new_lang {
            self.set_lang(ui.ctx(), l);
        }
        if let Some(d) = new_digits {
            self.set_digits(d);
        }
    }

    fn device_card_settings(&mut self, ui: &mut egui::Ui) {
        let tx = self.tx;
        let mut save_name = false;
        theme::card(ui, |ui| {
            theme::column(ui, tx.rtl(), |ui| {
                widgets::icon_heading(ui, tx, Icon::Laptop, tx.t(S::ThisDeviceSettings));
                ui.add_space(6.0);
                ui.label(theme::caption(ui, tx.t(S::DeviceName)));
                theme::row(ui, tx.rtl(), |ui| {
                    let w = (ui.available_width() - 100.0).max(160.0);
                    let hint = tx.t(S::DeviceName);
                    let resp =
                        widgets::text_field(ui, tx, "device-name", &mut self.name_edit, hint, w);
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let changed = self.name_edit.trim() != self.local.device_name
                        && !self.name_edit.trim().is_empty();
                    if ui
                        .add_enabled(
                            changed,
                            widgets::primary_icon_button(ui, tx, Icon::Save, tx.t(S::Save)),
                        )
                        .clicked()
                        || (enter && changed)
                    {
                        save_name = true;
                    }
                    resp
                });
                ui.label(theme::caption(ui, tx.t(S::DeviceNameHint)));
                if self.renamed {
                    ui.label(theme::caption(ui, tx.t(S::DeviceNameRestart)));
                }
            });
        });
        if save_name {
            let name = self.name_edit.trim().to_owned();
            self.settings.device_name = Some(name.clone());
            self.settings.save();
            self.renamed = true;
            self.backend.set_device_name(name);
        }
    }
}

/// One row of the settings navigation: icon, label, accent tint when selected.
fn nav_item(ui: &mut egui::Ui, tx: Tx, icon: Icon, label: &str, selected: bool) -> egui::Response {
    let p = theme::tokens(ui);
    let h = 38.0;
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::click());
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, label)
    });
    if ui.is_rect_visible(rect) {
        let r = egui::CornerRadius::same(theme::radius::MD + 2);
        if selected {
            ui.painter().rect_filled(rect, r, p.accent_soft);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, r, p.surface_hover);
        }
        let color = if selected { p.accent } else { p.text_muted };
        let text_color = if selected { p.text } else { p.text_muted };
        let pad = 12.0;
        let icon_size = 18.0;
        let icon_x = if tx.rtl() {
            rect.max.x - pad - icon_size
        } else {
            rect.min.x + pad
        };
        let icon_rect = egui::Rect::from_min_size(
            egui::pos2(icon_x, rect.center().y - icon_size / 2.0),
            egui::vec2(icon_size, icon_size),
        );
        icon.image_dir(tx.rtl(), icon_size, color)
            .paint_at(ui, icon_rect);
        // The label gets the rest of the row and is cut with an ellipsis if it is longer.
        let text_w = (rect.width() - pad * 2.0 - icon_size - 10.0).max(20.0);
        let mut job = egui::text::LayoutJob::simple_singleline(
            label.to_owned(),
            theme::medium(theme::font::BODY),
            text_color,
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(text_w);
        let galley = ui.fonts_mut(|f| f.layout_job(job));
        let x = if tx.rtl() {
            icon_x - 10.0 - galley.size().x
        } else {
            icon_rect.max.x + 10.0
        };
        let pos = egui::pos2(x, rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(pos, galley, text_color);
    }
    resp
}
