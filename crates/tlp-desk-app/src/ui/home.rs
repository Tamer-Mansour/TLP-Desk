//! Home screen: this device's identity + connect box + discovered devices.

use eframe::egui::{self, Align, Id, Layout, RichText};

use super::address_book::HomeTab;
use super::icons::{self, Icon};
use super::{shell, theme, widgets};
use crate::app::{App, Screen};
use crate::backend::{ConnectTarget, DiscoveredDevice};
use crate::i18n::{Tx, S};

pub(crate) enum Action {
    Connect(ConnectTarget),
    ConnectAddress,
    NewCode,
    /// Save a discovered device to the address book.
    SaveDevice(DiscoveredDevice),
    /// Switch to the address book tab.
    ShowAddressBook,
    /// Open the Devices page.
    ShowDevices,
}

impl App {
    pub fn home_ui(&mut self, ui: &mut egui::Ui) {
        let tx = self.tx;
        let mut actions = Vec::new();

        shell::canvas(ui, |ui| {
            shell::page_title(ui, tx, tx.t(S::AppTitle), Some(tx.t(S::HomeLede)));
            self.permissions.card(ui, tx);
            self.lan_update_notice(ui);
            self.address_book.offer_ui(ui, tx, self.backend.as_ref());
            ui.spacing_mut().item_spacing.x = theme::space::XL;
            // Identity on the start side (narrower), connect + devices on the end side.
            let total = ui.available_width();
            let left_w = (total * 0.42).clamp(340.0, 460.0);
            let right_w = total - left_w - theme::space::XL;
            // Taken before the row: a row starts one widget tall, so inside it the
            // available height would be that, not the canvas's.
            let col_h = ui.available_height();
            theme::row(ui, tx.rtl(), |ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(left_w, col_h),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.set_width(left_w);
                        self.identity_card(ui, &mut actions);
                    },
                );
                ui.allocate_ui_with_layout(
                    egui::vec2(right_w, col_h),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.set_width(right_w);
                        self.connect_bar(ui, &mut actions);
                        ui.add_space(theme::space::L);
                        self.nearby_preview(ui, &mut actions);
                    },
                );
            });
        });
        self.address_book.dialogs_ui(ui.ctx(), tx);
        self.address_book.toast_ui(ui.ctx(), tx);

        for action in actions {
            match action {
                Action::Connect(t) => self.connect(t),
                Action::ConnectAddress => self.connect_from_address(),
                Action::NewCode => self.backend.refresh_one_time_code(),
                Action::SaveDevice(d) => {
                    self.address_book
                        .save_discovered(&d, self.backend.as_ref(), tx);
                }
                Action::ShowAddressBook => {
                    self.address_book.tab = HomeTab::AddressBook;
                    self.screen = Screen::Devices;
                }
                Action::ShowDevices => self.screen = Screen::Devices,
            }
        }
    }

    /// The identity card: a glass panel with a teal light behind it, the device ID as the
    /// largest thing on the screen, the one-time code under it, and a status pill. It is
    /// the one object people recognise the app by.
    fn identity_card(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let tx = self.tx;
        let dark = ui.visuals().dark_mode;
        let p = theme::tokens(ui);
        // The glow is painted first (under the panel) at the panel's top-start corner.
        let glow_slot = ui.painter().add(egui::Shape::Noop);
        let inner = theme::glass_frame(ui)
            .inner_margin(egui::Margin::same(26))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                theme::column(ui, tx.rtl(), |ui| {
                    // Header row: eyebrow + status pill at the reading end.
                    theme::row(ui, tx.rtl(), |ui| {
                        ui.label(theme::eyebrow(ui, tx.t(S::ThisDevice)));
                        theme::row_end(ui, tx.rtl(), |ui| {
                            let hosting = self.host_sessions.len();
                            let (text, tone) = if self.host_problem.is_some() {
                                (tx.t(S::StatusOffline).to_owned(), theme::Tone::Danger)
                            } else if hosting > 0 {
                                (
                                    tx.f(
                                        S::StatusHosting,
                                        &[("n", &tx.digits(&hosting.to_string()))],
                                    ),
                                    theme::Tone::Accent,
                                )
                            } else {
                                (tx.t(S::StatusReady).to_owned(), theme::Tone::Success)
                            };
                            status_pill(ui, tx, &text, tone);
                        });
                    });
                    ui.add_space(theme::space::XL);

                    ui.label(theme::caption(ui, tx.t(S::DeviceId)));
                    let id = self.local.device_id.to_string();
                    theme::row(ui, tx.rtl(), |ui| {
                        ui.label(
                            RichText::new(tx.n(&id))
                                .font(theme::semibold(theme::font::DISPLAY))
                                .color(p.text),
                        );
                        ui.add_space(theme::space::S);
                        // Clipboard always gets Western digits.
                        widgets::copy_button(ui, tx, Id::new("copy-id"), &id);
                    });
                    ui.add_space(theme::space::L);

                    ui.label(theme::caption(ui, tx.t(S::OneTimeCode)));
                    let code = &self.local.one_time_code;
                    let grouped = if code.len() == 6 {
                        format!("{} {}", &code[..3], &code[3..])
                    } else {
                        code.clone()
                    };
                    theme::row(ui, tx.rtl(), |ui| {
                        ui.label(
                            RichText::new(tx.n(&grouped))
                                .font(theme::semibold(theme::font::CODE))
                                .color(p.accent),
                        );
                        ui.add_space(theme::space::S);
                        widgets::copy_button(ui, tx, Id::new("copy-code"), code);
                        if widgets::ghost_scope(ui, |ui| {
                            icons::icon_button(ui, tx, Icon::Refresh, tx.t(S::NewCode))
                        })
                        .clicked()
                        {
                            actions.push(Action::NewCode);
                        }
                    });
                    ui.label(theme::caption(ui, tx.t(S::OneTimeCodeHint)));
                    ui.add_space(theme::space::XL);
                    widgets::divider(ui);

                    // Name and addresses as a compact key/value list.
                    kv_row(
                        ui,
                        tx,
                        tx.t(S::DeviceName),
                        &tx.d(&self.local.device_name),
                        None,
                    );
                    for addr in &self.local.addresses {
                        let a = addr.to_string();
                        kv_row(ui, tx, tx.t(S::LocalAddress), &tx.n(&a), Some(&a));
                    }
                    if let Some(ip) = self.local.identity_conflict {
                        ui.add_space(theme::space::S);
                        let (fg, bg) = theme::Tone::Warning.colors(dark);
                        egui::Frame::new()
                            .fill(bg)
                            .corner_radius(theme::radius::MD)
                            .inner_margin(egui::Margin::symmetric(12, 8))
                            .show(ui, |ui| {
                                // Isolated so the address keeps its order inside Arabic text.
                                let addr = format!("\u{2066}{ip}\u{2069}");
                                let text = tx.f(S::IdentityConflict, &[("addr", &addr)]);
                                ui.add(egui::Label::new(RichText::new(text).color(fg)).wrap());
                            });
                    }
                });
            });
        let rect = inner.response.rect;
        theme::paint_glass_edge(ui.painter(), rect, dark);
        let corner = if tx.rtl() {
            rect.right_top() + egui::vec2(-40.0, 30.0)
        } else {
            rect.left_top() + egui::vec2(40.0, 30.0)
        };
        let strength = if dark { 0.55 } else { 0.22 };
        ui.painter().set(
            glow_slot,
            theme::glow_shape(corner, 260.0, p.accent, strength),
        );
    }

    /// The connect bar: one wide glass strip with the address field, the sign-in choice and
    /// the primary Connect pill.
    fn connect_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let tx = self.tx;
        theme::glass(ui, |ui| {
            theme::column(ui, tx.rtl(), |ui| {
                ui.label(theme::eyebrow(ui, tx.t(S::ConnectTitle)));
                ui.add_space(theme::space::M);
                theme::row(ui, tx.rtl(), |ui| {
                    let button_w = 140.0;
                    let field_w = (ui.available_width() - button_w - 12.0).max(140.0);
                    let resp = widgets::text_field(
                        ui,
                        tx,
                        "address",
                        &mut self.address,
                        tx.t(S::AddressHint),
                        field_w,
                    );
                    if std::mem::take(&mut self.focus_connect) {
                        resp.request_focus();
                    }
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
                ui.add_space(theme::space::M);
                // One-time code or permanent password (TLP-unattended).
                let enter = self.unattended.connect_fields(ui, tx, &mut self.code);
                if enter && !self.address.trim().is_empty() {
                    actions.push(Action::ConnectAddress);
                }
            });
        });
    }

    /// A short list of nearby devices (the first few) with a link to the Devices page.
    fn nearby_preview(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let tx = self.tx;
        theme::glass(ui, |ui| {
            theme::column(ui, tx.rtl(), |ui| {
                theme::row(ui, tx.rtl(), |ui| {
                    ui.label(theme::eyebrow(ui, tx.t(S::NearbyDevices)));
                    theme::row_end(ui, tx.rtl(), |ui| {
                        if widgets::ghost_scope(ui, |ui| {
                            ui.add(icons::icon_text_button(
                                tx,
                                Icon::Users,
                                tx.t(S::MoreDevices),
                            ))
                        })
                        .clicked()
                        {
                            actions.push(Action::ShowDevices);
                        }
                    });
                });
                ui.add_space(theme::space::M);
                match &self.devices {
                    None => {
                        theme::row(ui, tx.rtl(), |ui| {
                            ui.spinner();
                            ui.label(theme::caption(ui, tx.t(S::Searching)));
                        });
                    }
                    Some(list) if list.is_empty() => {
                        widgets::empty_state(
                            ui,
                            tx,
                            Icon::Radar,
                            tx.t(S::NoDevicesTitle),
                            tx.t(S::NoDevicesHint),
                        );
                    }
                    Some(list) => {
                        // Whole rows only, as many as fit under the header (the first row
                        // always shows; the rest are on the Devices page).
                        let mut row_h = None;
                        for dev in list.iter().take(PREVIEW_ROWS) {
                            if let Some(h) = row_h {
                                if ui.available_height() < h {
                                    break;
                                }
                            }
                            let saved = self.address_book.book.contains(dev.device_id);
                            let top = ui.cursor().min.y;
                            device_row(ui, tx, dev, saved, actions);
                            row_h.get_or_insert(ui.cursor().min.y - top);
                        }
                    }
                }
            });
        });
    }
}

/// How many devices the home preview shows before "See all".
const PREVIEW_ROWS: usize = 4;

/// A small pill with a dot: session / availability status. Painted by hand so it is sized
/// to its text (a frame inside a trailing row would stretch to the row's end).
pub fn status_pill(ui: &mut egui::Ui, tx: Tx, text: &str, tone: theme::Tone) {
    let dark = ui.visuals().dark_mode;
    let (fg, bg) = tone.colors(dark);
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), theme::medium(theme::font::CAPTION), fg);
    let (pad_x, pad_y, dot, gap) = (10.0, 4.0, 7.0, 6.0);
    let size = egui::vec2(
        galley.size().x + dot + gap + pad_x * 2.0,
        (galley.size().y + pad_y * 2.0).max(26.0),
    );
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, theme::radius::PILL, bg);
        let (dot_x, text_x) = if tx.rtl() {
            (rect.max.x - pad_x - dot / 2.0, rect.min.x + pad_x)
        } else {
            (
                rect.min.x + pad_x + dot / 2.0,
                rect.min.x + pad_x + dot + gap,
            )
        };
        painter.circle_filled(egui::pos2(dot_x, rect.center().y), 3.5, fg);
        let text_pos = egui::pos2(text_x, rect.center().y - galley.size().y / 2.0);
        painter.galley(text_pos, galley, fg);
    }
}

/// One "label: value" row with an optional copy button (the clipboard gets `copy`).
fn kv_row(ui: &mut egui::Ui, tx: Tx, label: &str, value: &str, copy: Option<&str>) {
    let p = theme::tokens(ui);
    theme::row(ui, tx.rtl(), |ui| {
        ui.add_sized(
            egui::vec2(110.0, ui.spacing().interact_size.y - 6.0),
            egui::Label::new(theme::caption(ui, label)),
        );
        ui.label(
            RichText::new(value)
                .font(theme::medium(theme::font::BODY))
                .color(p.text),
        );
        if let Some(c) = copy {
            widgets::copy_button(ui, tx, Id::new(("copy-kv", c)), c);
        }
    });
}

/// Longest address shown in a device row before it is elided (the full one is in the tooltip).
const MAX_ADDRESS_CHARS: usize = 22;
/// Longest device name shown in a device row before it is elided.
const MAX_NAME_CHARS: usize = 28;

/// Where the two halves of a device row ended up (for layout tests).
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct RowRects {
    /// Status dot, name and meta line.
    lead: egui::Rect,
    /// Status chip, save star and Connect button.
    trailing: egui::Rect,
}

pub(crate) fn device_row(
    ui: &mut egui::Ui,
    tx: Tx,
    dev: &DiscoveredDevice,
    saved: bool,
    actions: &mut Vec<Action>,
) -> RowRects {
    let dark = ui.visuals().dark_mode;
    let rtl = tx.rtl();
    let (rects, _) = widgets::list_row(ui, |ui| {
        let height = 40.0;
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
        let avatar = widgets::avatar(&mut lead, &dev.name, 34.0);
        // Presence dot on the avatar's reading-end corner.
        let dot_color = if dev.online {
            theme::success(dark)
        } else {
            lead.visuals().weak_text_color()
        };
        let corner = if rtl {
            avatar.rect.left_bottom() + egui::vec2(4.0, -4.0)
        } else {
            avatar.rect.right_bottom() + egui::vec2(-4.0, -4.0)
        };
        lead.painter()
            .circle_filled(corner, 5.5, theme::tokens(&lead).surface_alt);
        lead.painter().circle_filled(corner, 3.5, dot_color);
        lead.add_space(theme::space::XS);
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
            let status = if dev.online { S::Online } else { S::Offline };
            // Build the meta line in logical order, then shape once.
            let meta = widgets::fit(
                ui,
                &format!("{id} · {ip} · {}", tx.raw(status)),
                max_w,
                |s| theme::caption(ui, &tx.p(s)),
            );
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

/// Connect button and the save star, from the reading end.
fn trail_actions(
    ui: &mut egui::Ui,
    tx: Tx,
    dev: &DiscoveredDevice,
    saved: bool,
    actions: &mut Vec<Action>,
) {
    let connect = icons::icon_text_button(tx, Icon::Connect, tx.t(S::Connect));
    if ui.add_enabled(dev.online, connect).clicked() {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::DeviceId;
    use crate::i18n::{DigitStyle, Lang};

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
            app_version: None,
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
