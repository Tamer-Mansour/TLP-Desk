//! The session toolbar: responsive, with an overflow "More" menu.
//!
//! Every frame each item is measured (drawn once into an invisible child `Ui`), then items are
//! moved from the bar into the "More" popup, least important first, until what is left fits the
//! window next to the connection label and the always-visible **Disconnect** button. A wide
//! window therefore shows everything; a narrow one keeps the essentials (view mode, full
//! screen, view only, stats, files, chat, monitors, quality) and Disconnect. The row is
//! mirrored in Arabic by `theme::row`, so the overflow button sits at the reading end.

use eframe::egui::{
    Align, Layout, Popup, PopupCloseBehavior, Rect, RectAlign, TextWrapMode, UiBuilder,
};

use super::super::recordings::{self, RecordState};
use super::*;

/// One thing on the toolbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    /// Fit-to-window and actual-size toggles.
    Mode,
    /// Full screen.
    Fullscreen,
    /// View-only switch.
    ViewOnly,
    /// Stats overlay.
    Stats,
    /// Privacy mode.
    Privacy,
    /// Remote device info window.
    DeviceInfo,
    /// Port forwarding window.
    Tunnels,
    /// Draw on screen.
    Whiteboard,
    /// Clipboard sync switch.
    Clipboard,
    /// File browser (and paste of remote files).
    Files,
    /// Transfer list.
    Transfers,
    /// Chat with unread badge.
    Chat,
    /// Remote sound switch and volume.
    Audio,
    /// Monitor picker (only with two or more monitors).
    Monitors,
    /// Quality presets.
    Quality,
    /// Restart / lock / Ctrl+Alt+Del.
    System,
    /// "Keys go to the remote PC" indicator (only while the keyboard grab is on).
    Keys,
    /// Session recording.
    Record,
}

/// An item and how it sits in the bar.
struct Entry {
    item: Item,
    /// A separator is drawn before it.
    sep: bool,
    /// Order of moving into "More": lower goes first; `None` = always on the bar.
    rank: Option<u8>,
}

const fn entry(item: Item, sep: bool, rank: Option<u8>) -> Entry {
    Entry { item, sep, rank }
}

/// Bar order (reading direction) and overflow priority.
const ENTRIES: &[Entry] = &[
    entry(Item::Mode, true, None),
    entry(Item::Fullscreen, true, None),
    entry(Item::ViewOnly, false, None),
    entry(Item::Stats, false, None),
    entry(Item::Privacy, false, Some(9)),
    entry(Item::DeviceInfo, false, Some(7)),
    entry(Item::Tunnels, false, Some(6)),
    entry(Item::Whiteboard, false, Some(8)),
    entry(Item::Clipboard, false, Some(5)),
    entry(Item::Files, false, None),
    entry(Item::Transfers, false, Some(4)),
    entry(Item::Chat, false, None),
    entry(Item::Audio, false, Some(1)),
    entry(Item::Monitors, true, None),
    entry(Item::Quality, false, None),
    entry(Item::System, true, Some(3)),
    entry(Item::Keys, false, Some(0)),
    entry(Item::Record, false, Some(2)),
];

/// Narrowest the connection label is squeezed to before items move into "More".
const MIN_NAME_W: f32 = 200.0;
/// Absolute minimum of the connection label.
const MIN_NAME_FLOOR: f32 = 40.0;
/// Width of the overflow popup.
const MORE_WIDTH: f32 = 270.0;

/// Where the last frame put the toolbar's parts (for the responsive tests).
#[derive(Debug, Clone)]
pub struct ToolbarLayout {
    /// The inner area of the toolbar panel (inside its margins).
    pub content: Rect,
    /// Items drawn on the bar, with their rectangles.
    pub inline: Vec<(Item, Rect)>,
    /// Items that moved into the "More" menu, in menu order.
    pub overflow: Vec<Item>,
    /// The "More" button, when anything overflowed.
    pub more: Option<Rect>,
    /// The Disconnect button.
    pub disconnect: Rect,
}

impl Default for ToolbarLayout {
    fn default() -> Self {
        Self {
            content: Rect::NOTHING,
            inline: Vec::new(),
            overflow: Vec::new(),
            more: None,
            disconnect: Rect::NOTHING,
        }
    }
}

/// Draw `f` into an invisible child `Ui` and return the width it took.
fn measure(ui: &mut egui::Ui, f: impl FnOnce(&mut egui::Ui)) -> f32 {
    let rect = Rect::from_min_size(
        ui.max_rect().min,
        egui::vec2(4000.0, ui.spacing().interact_size.y),
    );
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center))
            .invisible(),
    );
    f(&mut child);
    child.min_rect().width()
}

fn text_width(ui: &egui::Ui, text: &str) -> f32 {
    egui::WidgetText::from(RichText::new(text.to_owned()).strong())
        .into_galley(
            ui,
            Some(TextWrapMode::Extend),
            f32::INFINITY,
            egui::TextStyle::Body,
        )
        .size()
        .x
}

/// "Connected to <name>", with the name cut (and an ellipsis added at its logical end) until
/// the whole text fits `max_w`.
fn fit_label(ui: &egui::Ui, tx: Tx, name: &str, max_w: f32) -> String {
    let full = tx.f(S::SessionWith, &[("name", name)]);
    if text_width(ui, &full) <= max_w {
        return full;
    }
    let chars: Vec<char> = name.chars().collect();
    for keep in (0..chars.len()).rev() {
        let cut: String = chars.iter().take(keep).collect::<String>() + "\u{2026}";
        let text = tx.f(S::SessionWith, &[("name", &cut)]);
        if text_width(ui, &text) <= max_w {
            return text;
        }
    }
    tx.f(S::SessionWith, &[("name", "\u{2026}")])
}

impl ViewerSession {
    pub(super) fn toolbar(&mut self, ui: &mut egui::Ui, tx: Tx, backend: &dyn Backend) {
        let dark = ui.visuals().dark_mode;
        let mut disconnect = false;
        let mut layout = ToolbarLayout::default();
        egui::Panel::top("session-toolbar")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().window_fill)
                    .inner_margin(egui::Margin::symmetric(14, 8)),
            )
            .show(ui, |ui| {
                layout.content = ui.max_rect();
                theme::row(ui, tx.rtl(), |ui| {
                    let s = ui.spacing().item_spacing.x;
                    let width = ui.available_width();

                    // Measure everything that could be on the bar.
                    let mut sizes: Vec<(&Entry, f32)> = Vec::new();
                    for e in ENTRIES {
                        let w = measure(ui, |ui| self.draw_item(e.item, ui, tx, backend));
                        if w > 0.5 {
                            sizes.push((e, w));
                        }
                    }
                    let sep_w = measure(ui, |ui| {
                        ui.separator();
                    });
                    let more_w = measure(ui, |ui| {
                        icons::icon_toggle(ui, tx, false, Icon::More, tx.t(S::ToolbarMore));
                    });
                    let disc_w = measure(ui, |ui| {
                        ui.add(widgets::danger_icon_button(
                            ui,
                            tx,
                            Icon::Disconnect,
                            tx.t(S::Disconnect),
                        ));
                    });
                    let label_full = tx.f(S::SessionWith, &[("name", &self.peer.name)]);
                    let natural_name = text_width(ui, &label_full);
                    let dot_w = 10.0;

                    // Move items into "More", least important first, until the rest fits.
                    let mut shown: Vec<bool> = vec![true; sizes.len()];
                    let min_name = MIN_NAME_W.min(natural_name);
                    let needed = |shown: &[bool]| -> f32 {
                        let mut total = dot_w + s + min_name + s + disc_w + s + 6.0;
                        for ((e, w), on) in sizes.iter().zip(shown) {
                            if *on {
                                total += w + s + if e.sep { sep_w + s } else { 0.0 };
                            }
                        }
                        if shown.iter().any(|on| !on) {
                            total += more_w + s;
                        }
                        total
                    };
                    while needed(&shown) > width {
                        let next = sizes
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| shown[*i])
                            .filter_map(|(i, (e, _))| e.rank.map(|r| (r, i)))
                            .min();
                        match next {
                            Some((_, i)) => shown[i] = false,
                            None => break,
                        }
                    }
                    let name_w = (width - (needed(&shown) - min_name))
                        .clamp(MIN_NAME_FLOOR, natural_name.max(MIN_NAME_FLOOR));

                    // The bar.
                    theme::status_dot(ui, theme::success(dark));
                    let label = fit_label(ui, tx, &self.peer.name, name_w);
                    let truncated = label != label_full;
                    let resp = ui.add(
                        egui::Label::new(RichText::new(label).strong())
                            .wrap_mode(TextWrapMode::Extend),
                    );
                    if truncated {
                        resp.on_hover_text(label_full);
                    }
                    for ((e, _), on) in sizes.iter().zip(&shown) {
                        if !*on {
                            continue;
                        }
                        if e.sep {
                            ui.separator();
                        }
                        let rect = ui
                            .scope(|ui| self.draw_item(e.item, ui, tx, backend))
                            .response
                            .rect;
                        layout.inline.push((e.item, rect));
                    }
                    let hidden: Vec<Item> = sizes
                        .iter()
                        .zip(&shown)
                        .filter(|(_, on)| !**on)
                        .map(|((e, _), _)| e.item)
                        .collect();
                    layout.overflow = hidden.clone();
                    theme::row_end(ui, tx.rtl(), |ui| {
                        let b = widgets::danger_icon_button(
                            ui,
                            tx,
                            Icon::Disconnect,
                            tx.t(S::Disconnect),
                        );
                        let resp = ui.add(b);
                        layout.disconnect = resp.rect;
                        if resp.clicked() {
                            disconnect = true;
                        }
                        if !hidden.is_empty() {
                            layout.more = Some(self.more_menu(ui, tx, backend, &hidden));
                        }
                    });
                });
            });
        self.toolbar_layout = layout;
        if disconnect {
            self.release_all(backend);
            backend.disconnect(self.id);
        }
    }

    /// The overflow button and its popup. Returns the button's rectangle.
    fn more_menu(
        &mut self,
        ui: &mut egui::Ui,
        tx: Tx,
        backend: &dyn Backend,
        items: &[Item],
    ) -> Rect {
        let button = icons::icon_toggle(ui, tx, false, Icon::More, tx.t(S::ToolbarMore));
        if items.contains(&Item::Record) && self.recording != RecordState::Off {
            // A running recording must stay visible even when its button moved into the menu.
            let red = recordings::record_red(ui.visuals().dark_mode);
            let corner = if tx.rtl() {
                button.rect.left_top()
            } else {
                button.rect.right_top()
            };
            let at = corner + egui::vec2(if tx.rtl() { 5.0 } else { -5.0 }, 5.0);
            ui.painter().circle_filled(at, 3.5, red);
        }
        let align = if tx.rtl() {
            RectAlign::BOTTOM_START
        } else {
            RectAlign::BOTTOM_END
        };
        Popup::menu(&button)
            .align(align)
            .width(MORE_WIDTH)
            .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                theme::column(ui, tx.rtl(), |ui| {
                    for item in items {
                        theme::row(ui, tx.rtl(), |ui| {
                            self.draw_item(*item, ui, tx, backend);
                            if let Some(text) = self.overflow_label(*item, tx) {
                                ui.label(text);
                            }
                        });
                    }
                });
            });
        button.rect
    }

    /// Text next to an item's button in the "More" menu.
    fn overflow_label(&self, item: Item, tx: Tx) -> Option<&'static str> {
        let key = match item {
            Item::Privacy => S::PrivacyMode,
            Item::DeviceInfo => S::DeviceInfoTitle,
            Item::Tunnels => S::PortForwarding,
            Item::Whiteboard => S::WbDraw,
            Item::Clipboard => S::ClipboardSync,
            Item::Transfers => S::FilesPanelTitle,
            Item::Audio => S::AudioVolume,
            Item::System => S::SystemMenu,
            Item::Keys => S::KeysCaptured,
            Item::Record => S::RecLabel,
            _ => return None,
        };
        Some(tx.t(key))
    }

    /// Draws one toolbar item and applies its clicks.
    fn draw_item(&mut self, item: Item, ui: &mut egui::Ui, tx: Tx, backend: &dyn Backend) {
        let dark = ui.visuals().dark_mode;
        match item {
            Item::Mode => {
                for (mode, icon, key) in [
                    (ViewMode::Fit, Icon::FitToWindow, S::FitToWindow),
                    (ViewMode::Original, Icon::ActualSize, S::OriginalSize),
                ] {
                    let selected = self.mode == mode;
                    if icons::icon_toggle(ui, tx, selected, icon, tx.t(key)).clicked() {
                        self.mode = mode;
                    }
                }
            }
            Item::Fullscreen => {
                let fs = ui.input(|i| i.viewport().fullscreen.unwrap_or(false));
                let (fs_icon, fs_label) = if fs {
                    (Icon::ExitFullscreen, S::ExitFullscreen)
                } else {
                    (Icon::Fullscreen, S::Fullscreen)
                };
                if icons::icon_toggle(ui, tx, fs, fs_icon, tx.t(fs_label)).clicked() {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
                }
            }
            Item::ViewOnly => {
                let control_allowed = self.permissions.contains(Permissions::CONTROL);
                let vo = self.view_only || !control_allowed;
                let resp = icons::icon_toggle_enabled(
                    ui,
                    tx,
                    control_allowed,
                    vo,
                    Icon::ViewOnly,
                    tx.t(S::ViewOnly),
                );
                let resp = if control_allowed {
                    resp
                } else {
                    resp.on_disabled_hover_text(tx.t(S::PermissionViewOnly))
                };
                if resp.clicked() {
                    self.view_only = !self.view_only;
                    if self.view_only {
                        self.release_all(backend);
                    }
                    backend.set_view_only(self.id, self.view_only);
                }
            }
            Item::Stats => {
                if icons::icon_toggle(ui, tx, self.show_stats, Icon::Stats, tx.t(S::Stats))
                    .clicked()
                {
                    self.show_stats = !self.show_stats;
                }
            }
            Item::Privacy => {
                let allowed = self.permissions.contains(Permissions::PRIVACY_MODE);
                if let Some(on) = self.privacy.toolbar(ui, tx, allowed) {
                    backend.set_privacy_mode(self.id, on);
                }
            }
            Item::DeviceInfo => self.device_info.toolbar_button(ui, tx, backend, self.id),
            Item::Tunnels => self.tunnels.toolbar_button(ui, tx),
            Item::Whiteboard => {
                let allowed = self.permissions.contains(Permissions::WHITEBOARD);
                if self.whiteboard.toggle_button(ui, tx, allowed) == Some(true) {
                    // Drawing takes the pointer: nothing may stay pressed on the remote PC.
                    self.set_grab(false);
                    self.release_all(backend);
                }
            }
            Item::Clipboard => {
                let (id, perms) = (self.id, self.permissions);
                self.clipboard.toolbar(ui, tx, id, perms, backend);
            }
            Item::Files => self.files_buttons(ui, tx, backend),
            Item::Transfers => {
                if icons::icon_toggle(
                    ui,
                    tx,
                    self.transfers.open,
                    Icon::Upload,
                    tx.t(S::FilesPanelTitle),
                )
                .clicked()
                {
                    self.transfers.open = !self.transfers.open;
                }
            }
            Item::Chat => {
                if chat::chat_toggle(ui, tx, &self.chat) {
                    self.chat.set_open(!self.chat.is_open());
                }
            }
            Item::Audio => super::super::audio::toolbar(
                ui,
                tx,
                backend,
                self.id,
                self.permissions,
                &mut self.audio,
            ),
            Item::Monitors => {
                if let Some(pick) = self.monitor_picker(ui, tx) {
                    // Nothing may stay pressed on the old screen.
                    self.release_all(backend);
                    match pick {
                        Some(index) => backend.select_monitor(self.id, index),
                        None => backend.select_all_monitors(self.id),
                    }
                }
            }
            Item::Quality => {
                if let Some(q) = perf::quality_menu(ui, tx, self.quality) {
                    self.quality = q;
                    backend.request_quality(self.id, q.to_proto());
                }
            }
            Item::System => {
                self.unattended
                    .menu(ui, tx, self.permissions, backend, self.id);
            }
            Item::Keys => {
                if self.grab_on {
                    // Accent icon = keys are captured; the caption is the tooltip.
                    let hint = format!("{}\n{}", tx.t(S::KeysCaptured), tx.t(S::KeysCapturedHint));
                    icons::icon(
                        ui,
                        tx,
                        Icon::Keyboard,
                        icons::BUTTON_ICON,
                        theme::accent(dark),
                    )
                    .on_hover_text(hint);
                }
            }
            Item::Record => match recordings::record_toggle(ui, tx, self.recording) {
                Some(true) => {
                    self.recording = RecordState::Starting;
                    backend.start_recording(self.id);
                }
                Some(false) => backend.stop_recording(self.id),
                None => {}
            },
        }
    }
}
