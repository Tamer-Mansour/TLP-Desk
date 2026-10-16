//! Home screen: this device's identity + connect box + discovered devices.

use eframe::egui::{self, Align, Id, Layout, RichText};

use super::address_book::{BookCommand, HomeTab};
use super::icons::{self, Icon};
use super::{theme, widgets};
use crate::app::{App, Screen};
use crate::backend::{ConnectTarget, DiscoveredDevice};
use crate::i18n::{Lang, Tx, S};

enum Action {
    Connect(ConnectTarget),
    ConnectAddress,
    NewCode,
    OpenSettings,
    /// connection log (TLP-settings-tray)
    OpenConnectionLog,
    SetLang(Lang),
    /// Save a discovered device to the address book.
    SaveDevice(DiscoveredDevice),
    /// Switch to the address book tab.
    ShowAddressBook,
    /// recording (TLP-recording)
    OpenRecordings,
}

/// Top bar shared by home and settings: brand at the start, settings + language at the end.
fn top_bar(ui: &mut egui::Ui, tx: Tx, actions: &mut Vec<Action>, show_settings_button: bool) {
    let p = theme::tokens(ui);
    egui::Panel::top("top-bar")
        .frame(
            egui::Frame::new()
                .fill(p.surface)
                .inner_margin(egui::Margin::symmetric(24, 10)),
        )
        .show(ui, |ui| {
            theme::row(ui, tx.rtl(), |ui| {
                let dark = ui.visuals().dark_mode;
                let (mark, _) =
                    ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::hover());
                theme::paint_brand_mark(ui.painter(), mark, dark);
                ui.label(
                    RichText::new(tx.t(S::AppTitle))
                        .size(theme::font::HEADING + 1.0)
                        .strong()
                        .color(p.text),
                );
                theme::row_end(ui, tx.rtl(), |ui| {
                    ui.spacing_mut().item_spacing.x = theme::space::XS;
                    let mut lang = tx.lang;
                    egui::ComboBox::from_id_salt("lang-picker")
                        .selected_text(lang.native_name())
                        .show_ui(ui, |ui| {
                            for l in Lang::ALL {
                                ui.selectable_value(&mut lang, l, l.native_name());
                            }
                        });
                    let weak = ui.visuals().weak_text_color();
                    icons::icon(ui, tx, Icon::Language, 18.0, weak);
                    if lang != tx.lang {
                        actions.push(Action::SetLang(lang));
                    }
                    if show_settings_button {
                        ui.add_space(theme::space::S);
                        let (sep, _) =
                            ui.allocate_exact_size(egui::vec2(1.0, 20.0), egui::Sense::hover());
                        ui.painter().vline(
                            sep.center().x,
                            sep.y_range(),
                            egui::Stroke::new(1.0, p.border),
                        );
                        ui.add_space(theme::space::S);
                    }
                    let ghost = widgets::ButtonKind::Ghost;
                    if show_settings_button
                        && widgets::button(ui, tx, ghost, Icon::Settings, tx.t(S::Settings))
                            .clicked()
                    {
                        actions.push(Action::OpenSettings);
                    }
                    // recording (TLP-recording)
                    if show_settings_button
                        && widgets::button(ui, tx, ghost, Icon::Playback, tx.t(S::Recordings))
                            .clicked()
                    {
                        actions.push(Action::OpenRecordings);
                    }
                    // connection log (TLP-settings-tray)
                    if show_settings_button
                        && widgets::button(ui, tx, ghost, Icon::Log, tx.t(S::ConnectionLog))
                            .clicked()
                    {
                        actions.push(Action::OpenConnectionLog);
                    }
                });
            });
        });
}

impl App {
    pub fn home_ui(&mut self, ui: &mut egui::Ui) {
        let tx = self.tx;
        let mut actions = Vec::new();
        top_bar(ui, tx, &mut actions, true);

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(20),
            )
            .show(ui, |ui| {
                self.permissions.card(ui, tx);
                self.address_book.offer_ui(ui, tx, self.backend.as_ref());
                ui.spacing_mut().item_spacing.x = theme::space::L;
                ui.columns(2, |cols| {
                    let (first, second) = if tx.rtl() { (1, 0) } else { (0, 1) };
                    for col in cols.iter_mut() {
                        col.spacing_mut().item_spacing.x = theme::space::S + 2.0;
                    }
                    self.device_card(&mut cols[first], &mut actions);
                    self.connect_card(&mut cols[second], &mut actions);
                });
            });
        self.address_book.dialogs_ui(ui.ctx(), tx);
        self.address_book.toast_ui(ui.ctx(), tx);

        for action in actions {
            match action {
                Action::Connect(t) => self.connect(t),
                Action::ConnectAddress => self.connect_from_address(),
                Action::NewCode => self.backend.refresh_one_time_code(),
                Action::OpenSettings => self.screen = Screen::Settings,
                Action::OpenConnectionLog => self.open_connection_log(),
                Action::SetLang(l) => self.set_lang(ui.ctx(), l),
                Action::SaveDevice(d) => {
                    self.address_book
                        .save_discovered(&d, self.backend.as_ref(), tx);
                }
                Action::ShowAddressBook => self.address_book.tab = HomeTab::AddressBook,
                Action::OpenRecordings => {
                    self.recordings.open = true;
                    self.recordings.reload();
                }
            }
        }
    }

    pub fn settings_top_bar(&mut self, ui: &mut egui::Ui) {
        let mut actions = Vec::new();
        top_bar(ui, self.tx, &mut actions, false);
        for action in actions {
            if let Action::SetLang(l) = action {
                self.set_lang(ui.ctx(), l);
            }
        }
    }

    fn device_card(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let tx = self.tx;
        theme::card(ui, |ui| {
            theme::column(ui, tx.rtl(), |ui| {
                widgets::section_header(ui, tx, Icon::Laptop, tx.t(S::ThisDevice), None);
                ui.add_space(theme::space::S);

                // Hero tile: the ID people read out loud, then the one-time code.
                let p = theme::tokens(ui);
                egui::Frame::new()
                    .fill(p.surface_alt)
                    .stroke(egui::Stroke::new(1.0, p.border))
                    .corner_radius(theme::radius::LG)
                    .inner_margin(egui::Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        theme::column(ui, tx.rtl(), |ui| {
                            ui.label(theme::caption(ui, tx.t(S::DeviceId)));
                            let id = self.local.device_id.to_string();
                            theme::row(ui, tx.rtl(), |ui| {
                                ui.label(
                                    theme::mono(&tx.n(&id), theme::font::DISPLAY)
                                        .strong()
                                        .color(p.text),
                                );
                                // Clipboard always gets Western digits.
                                widgets::copy_button(ui, tx, Id::new("copy-id"), &id);
                            });
                            widgets::divider(ui);
                            ui.label(theme::caption(ui, tx.t(S::OneTimeCode)));
                            let code = &self.local.one_time_code;
                            let grouped = if code.len() == 6 {
                                format!("{} {}", &code[..3], &code[3..])
                            } else {
                                code.clone()
                            };
                            theme::row(ui, tx.rtl(), |ui| {
                                ui.label(
                                    theme::mono(&tx.n(&grouped), theme::font::CODE).color(p.accent),
                                );
                                widgets::copy_button(ui, tx, Id::new("copy-code"), code);
                                let ghost = widgets::ButtonKind::Ghost;
                                if widgets::button(ui, tx, ghost, Icon::Refresh, tx.t(S::NewCode))
                                    .clicked()
                                {
                                    actions.push(Action::NewCode);
                                }
                            });
                            ui.label(theme::caption(ui, tx.t(S::OneTimeCodeHint)));
                        });
                    });
                ui.add_space(theme::space::L);

                theme::row(ui, tx.rtl(), |ui| {
                    ui.label(theme::caption(ui, tx.t(S::DeviceName)));
                    ui.add_space(theme::space::XS);
                    ui.label(RichText::new(tx.d(&self.local.device_name)).strong());
                });
                ui.add_space(theme::space::XS);
                ui.label(theme::caption(ui, tx.t(S::LocalAddress)));
                for addr in &self.local.addresses {
                    let a = addr.to_string();
                    theme::row(ui, tx.rtl(), |ui| {
                        egui::Frame::new()
                            .fill(p.surface_alt)
                            .corner_radius(theme::radius::SM)
                            .inner_margin(egui::Margin::symmetric(8, 3))
                            .show(ui, |ui| {
                                ui.label(theme::mono(&tx.n(&a), theme::font::MONO).color(p.text));
                            });
                        widgets::copy_button(ui, tx, Id::new(("copy-ip", &a)), &a);
                    });
                }
            });
        });
    }

    fn connect_card(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let tx = self.tx;
        theme::card(ui, |ui| {
            theme::column(ui, tx.rtl(), |ui| {
                widgets::section_header(ui, tx, Icon::Monitor, tx.t(S::ConnectTitle), None);
                ui.add_space(theme::space::S);
                theme::row(ui, tx.rtl(), |ui| {
                    let button_w = 130.0;
                    let field_w = (ui.available_width() - button_w - 12.0).max(120.0);
                    // In RTL the row flows right-to-left: field first (rightmost), then button.
                    let resp = widgets::text_field(
                        ui,
                        tx,
                        "address",
                        &mut self.address,
                        tx.t(S::AddressHint),
                        field_w,
                    );
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let has_text = !self.address.trim().is_empty();
                    let button =
                        widgets::primary_icon_button(ui, tx, Icon::Connect, tx.t(S::Connect));
                    let clicked = ui.add_enabled(has_text, button).clicked();
                    if (enter || clicked) && has_text {
                        actions.push(Action::ConnectAddress);
                    }
                    resp
                });
                ui.add_space(theme::space::S);
                // One-time code or permanent password (TLP-unattended).
                let enter = self.unattended.connect_fields(ui, tx, &mut self.code);
                if enter && !self.address.trim().is_empty() {
                    actions.push(Action::ConnectAddress);
                }
            });
        });
        ui.add_space(theme::space::L);
        theme::card(ui, |ui| {
            theme::column(ui, tx.rtl(), |ui| {
                // Tabs: devices on this network | address book.
                widgets::segmented(
                    ui,
                    tx,
                    &mut self.address_book.tab,
                    &[
                        (HomeTab::Nearby, Some(Icon::Network), tx.t(S::NearbyDevices)),
                        (
                            HomeTab::AddressBook,
                            Some(Icon::AddressBook),
                            tx.t(S::AbTitle),
                        ),
                    ],
                );
                ui.add_space(theme::space::S);
                if self.address_book.tab == HomeTab::AddressBook {
                    let backend = self.backend.clone();
                    if let Some(BookCommand::Connect(t)) =
                        self.address_book.list_ui(ui, tx, backend.as_ref())
                    {
                        actions.push(Action::Connect(t));
                    }
                    return;
                }
                match &self.devices {
                    None => {
                        theme::row(ui, tx.rtl(), |ui| {
                            ui.spinner();
                            ui.label(theme::caption(ui, tx.t(S::Searching)));
                        });
                    }
                    Some(list) => {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for dev in list {
                                    let saved = self.address_book.book.contains(dev.device_id);
                                    device_row(ui, tx, dev, saved, actions);
                                }
                            });
                    }
                }
            });
        });
    }
}

/// Longest address shown in a device row before it is elided (the full one is in the tooltip).
const MAX_ADDRESS_CHARS: usize = 22;
/// Longest device name shown in a device row before it is elided.
const MAX_NAME_CHARS: usize = 28;

/// Where the two halves of a device row ended up (for layout tests).
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(test), allow(dead_code))]
struct RowRects {
    /// Status dot, name and meta line.
    lead: egui::Rect,
    /// Status chip, save star and Connect button.
    trailing: egui::Rect,
}

fn device_row(
    ui: &mut egui::Ui,
    tx: Tx,
    dev: &DiscoveredDevice,
    saved: bool,
    actions: &mut Vec<Action>,
) -> RowRects {
    let dark = ui.visuals().dark_mode;
    let rtl = tx.rtl();
    let (rects, _) = widgets::list_row(ui, |ui| {
        let height = 36.0;
        let (row, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::hover(),
        );
        let (start, end) = if rtl {
            (
                Layout::right_to_left(Align::Center),
                Layout::left_to_right(Align::Center),
            )
        } else {
            (
                Layout::left_to_right(Align::Center),
                Layout::right_to_left(Align::Center),
            )
        };
        // Trailing actions first, so the name and the address get only what is left and can
        // never run underneath the buttons, whatever the address length.
        let mut trail = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(end));
        trail_actions(&mut trail, tx, dev, saved, actions);
        let trailing = trail.min_rect();
        let gap = theme::space::M;
        let lead_rect = if rtl {
            egui::Rect::from_min_max(egui::pos2(trailing.right() + gap, row.top()), row.max)
        } else {
            egui::Rect::from_min_max(row.min, egui::pos2(trailing.left() - gap, row.bottom()))
        };
        let mut lead = ui.new_child(egui::UiBuilder::new().max_rect(lead_rect).layout(start));
        lead.set_clip_rect(lead_rect.intersect(ui.clip_rect()));
        let color = if dev.online {
            theme::success(dark)
        } else {
            ui.visuals().weak_text_color()
        };
        theme::status_dot(&mut lead, color);
        theme::column(&mut lead, rtl, |ui| {
            let max_w = ui.available_width();
            let name = widgets::fit(ui, &widgets::elide(&dev.name, MAX_NAME_CHARS), max_w, |s| {
                RichText::new(tx.d(s)).strong()
            });
            let r = ui.label(RichText::new(tx.d(&name)).strong());
            if name != dev.name {
                r.on_hover_text(tx.d(&dev.name));
            }
            let id = tx.digits(&dev.device_id.to_string());
            let full = dev.address.to_string();
            let ip = tx.digits(&widgets::elide(&full, MAX_ADDRESS_CHARS));
            // Build the meta line in logical order, then shape once.
            let meta = widgets::fit(ui, &format!("{id} · {ip}"), max_w, |s| {
                theme::caption(ui, &tx.p(s))
            });
            ui.label(theme::caption(ui, &tx.p(&meta)))
                .on_hover_text(tx.digits(&full));
        });
        RowRects {
            lead: lead.min_rect(),
            trailing,
        }
    });
    rects
}

/// Connect button, save star and the availability chip, from the reading end.
fn trail_actions(
    ui: &mut egui::Ui,
    tx: Tx,
    dev: &DiscoveredDevice,
    saved: bool,
    actions: &mut Vec<Action>,
) {
    if ui
        .add_enabled(
            dev.online,
            icons::icon_text_button(tx, Icon::Connect, tx.t(S::Connect)),
        )
        .clicked()
    {
        actions.push(Action::Connect(ConnectTarget::Id(dev.device_id)));
    }
    // Address book: a selected star means saved (click opens the book).
    let tip = if saved {
        S::AbSavedDevice
    } else {
        S::AbSaveDevice
    };
    if widgets::ghost_scope(ui, |ui| {
        icons::icon_toggle(ui, tx, saved, Icon::Star, tx.t(tip))
    })
    .clicked()
    {
        actions.push(if saved {
            Action::ShowAddressBook
        } else {
            Action::SaveDevice(dev.clone())
        });
    }
    let (status, tone) = if dev.online {
        (S::Online, theme::Tone::Success)
    } else {
        (S::Offline, theme::Tone::Neutral)
    };
    widgets::chip(ui, tx.t(status), tone);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::DeviceId;
    use crate::i18n::DigitStyle;

    fn tx(lang: Lang) -> Tx {
        Tx::new(lang, DigitStyle::Western)
    }

    /// Regression: a long IPv6 link-local address used to push the status under the star and
    /// Connect buttons. The name/address block and the actions must never overlap, in English
    /// and Arabic, light and dark, even in a narrow card.
    #[test]
    fn device_row_with_long_ipv6_and_arabic_name_never_overlaps_actions() {
        let dev = DiscoveredDevice {
            device_id: DeviceId(306_117_942),
            name: "حاسوب-المكتبة-الرئيسية-في-الطابق-الثاني".into(),
            address: "fe80::1c2b:3d4e:5f60:7a8b".parse().unwrap(),
            online: true,
        };
        for lang in [Lang::En, Lang::Ar] {
            for dark in [false, true] {
                for width in [360.0, 520.0] {
                    let ctx = egui::Context::default();
                    theme::install(&ctx);
                    theme::apply_lang(&ctx, lang);
                    ctx.set_theme(if dark {
                        egui::Theme::Dark
                    } else {
                        egui::Theme::Light
                    });
                    let input = egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 200.0),
                        )),
                        ..Default::default()
                    };
                    let mut rects = None;
                    // Two passes: the first one measures, like a real frame after startup.
                    for _ in 0..2 {
                        let mut out = ctx.run_ui(input.clone(), |ui| {
                            let mut actions = Vec::new();
                            rects = Some(device_row(ui, tx(lang), &dev, false, &mut actions));
                        });
                        out.textures_delta.clear();
                    }
                    let r = rects.unwrap();
                    let what = format!("{lang:?} dark={dark} w={width}: {r:?}");
                    assert!(!r.lead.intersects(r.trailing), "{what}");
                    if lang.is_rtl() {
                        assert!(r.lead.left() > r.trailing.right(), "{what}");
                    } else {
                        assert!(r.lead.right() < r.trailing.left(), "{what}");
                    }
                    assert!(r.trailing.width() > 0.0 && r.lead.width() > 0.0, "{what}");
                    assert!(r.lead.right() <= width && r.lead.left() >= 0.0, "{what}");
                }
            }
        }
    }
}
