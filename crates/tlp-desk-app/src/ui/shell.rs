//! The workspace shell: a slim icon rail on the reading-start side (brand mark, the four
//! destinations, the theme toggle) and the canvas the screens draw into.
//!
//! The rail is the app's spine: it is drawn under the title bar on every screen except a
//! session (the session takes the whole window for the remote picture). Each item is an
//! icon with its label as a tooltip; the active one carries a rose glow and a short accent
//! bar on the canvas edge, like a bookmark.

use eframe::egui::{self, vec2, Align, Color32, Layout, Pos2, Rect, Sense, Stroke, Ui};

use super::icons::Icon;
use super::theme::{self, radius, space};
use crate::app::Screen;
use crate::i18n::{Tx, S};
use crate::settings::ThemeChoice;

/// Width of the rail (points).
pub const RAIL_W: f32 = 64.0;
/// Size of one rail item's hit area.
const ITEM: f32 = 44.0;

/// What the rail reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RailAction {
    Go(Screen),
    /// Cycle the theme: system -> light -> dark -> system.
    ToggleTheme,
}

/// The four destinations in rail order.
const DESTINATIONS: [(Screen, Icon, S); 4] = [
    (Screen::Home, Icon::Home, S::MenuHome),
    (Screen::Devices, Icon::Users, S::MenuDevices),
    (Screen::Sessions, Icon::History, S::RailSessions),
    (Screen::Settings, Icon::Settings, S::Settings),
];

/// Draws the rail as a side panel and returns what was clicked.
pub fn rail(
    ui: &mut Ui,
    tx: Tx,
    active: Screen,
    theme_choice: ThemeChoice,
    live_sessions: usize,
) -> Option<RailAction> {
    let dark = ui.visuals().dark_mode;
    let p = theme::tokens(ui);
    let mut action = None;
    let panel = if tx.rtl() {
        egui::Panel::right("rail")
    } else {
        egui::Panel::left("rail")
    };
    panel
        .exact_size(RAIL_W)
        .frame(
            egui::Frame::new()
                .fill(rail_fill(dark))
                .inner_margin(egui::Margin {
                    left: 0,
                    right: 0,
                    top: 14,
                    bottom: 14,
                }),
        )
        .show(ui, |ui| {
            // Hairline between rail and canvas, on the canvas side.
            let r = ui.max_rect();
            let x = if tx.rtl() {
                r.min.x + 0.5
            } else {
                r.max.x - 0.5
            };
            ui.painter()
                .vline(x, r.y_range(), Stroke::new(1.0, p.border));

            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.y = space::S;
                // Brand mark.
                let (mark, resp) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::click());
                theme::brand_mark(ui, mark);
                if resp.on_hover_text(tx.t(S::AppTitle)).clicked() {
                    action = Some(RailAction::Go(Screen::Home));
                }
                ui.add_space(space::L);

                for (screen, icon, label) in DESTINATIONS {
                    let selected = screen == active;
                    let badge =
                        (screen == Screen::Sessions && live_sessions > 0).then_some(live_sessions);
                    if rail_item(ui, tx, icon, tx.t(label), selected, badge).clicked() {
                        action = Some(RailAction::Go(screen));
                    }
                }

                // Theme toggle sits at the bottom.
                let remaining = ui.available_height() - ITEM - space::S;
                if remaining > 0.0 {
                    ui.add_space(remaining);
                }
                let (icon, label) = match theme_choice {
                    ThemeChoice::System => {
                        (if dark { Icon::Moon } else { Icon::Sun }, S::ThemeSystem)
                    }
                    ThemeChoice::Light => (Icon::Sun, S::ThemeLight),
                    ThemeChoice::Dark => (Icon::Moon, S::ThemeDark),
                };
                let tip = format!("{}: {}", tx.t(S::MenuTheme), tx.t(label));
                if rail_item(ui, tx, icon, &tip, false, None).clicked() {
                    action = Some(RailAction::ToggleTheme);
                }
            });
        });
    action
}

/// The rail background: one step darker than the canvas in the dark theme (the rail is the
/// deepest layer), one step lighter in the light theme.
fn rail_fill(dark: bool) -> Color32 {
    theme::chrome_fill(dark)
}

/// One rail item: a rounded square hit area, the icon, a tooltip, the active glow and
/// bookmark bar, and an optional count badge.
fn rail_item(
    ui: &mut Ui,
    tx: Tx,
    icon: Icon,
    label: &str,
    selected: bool,
    badge: Option<usize>,
) -> egui::Response {
    let dark = ui.visuals().dark_mode;
    let p = theme::tokens(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(ITEM, ITEM), Sense::click());
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, label)
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let t = ui.ctx().animate_bool_responsive(resp.id, selected);
        if t > 0.0 {
            // The brand light behind the active item.
            painter.add(theme::glow_shape(rect.center(), 34.0, p.accent, 0.35 * t));
            painter.rect_filled(
                rect,
                egui::CornerRadius::same(radius::LG),
                theme::lerp(Color32::TRANSPARENT, p.accent_soft, t),
            );
            // Bookmark bar on the canvas edge.
            let bar_w = 3.0;
            let bar = if tx.rtl() {
                Rect::from_min_max(
                    Pos2::new(rect.min.x - (RAIL_W - ITEM) / 2.0, rect.min.y + 10.0),
                    Pos2::new(
                        rect.min.x - (RAIL_W - ITEM) / 2.0 + bar_w,
                        rect.max.y - 10.0,
                    ),
                )
            } else {
                Rect::from_min_max(
                    Pos2::new(
                        rect.max.x + (RAIL_W - ITEM) / 2.0 - bar_w,
                        rect.min.y + 10.0,
                    ),
                    Pos2::new(rect.max.x + (RAIL_W - ITEM) / 2.0, rect.max.y - 10.0),
                )
            };
            painter.rect_filled(bar, egui::CornerRadius::same(2), p.accent.gamma_multiply(t));
        } else if resp.hovered() {
            painter.rect_filled(rect, egui::CornerRadius::same(radius::LG), p.surface_hover);
        }
        let color = if selected {
            p.accent
        } else if resp.hovered() {
            p.text
        } else {
            p.text_muted
        };
        let size = 22.0;
        let img = icon.image_dir(tx.rtl(), size, color);
        let img_rect = Rect::from_center_size(rect.center(), vec2(size, size));
        img.paint_at(ui, img_rect);
        if let Some(n) = badge {
            let text = tx.n(&n.to_string());
            let c = rect.right_top() + vec2(-9.0, 9.0);
            painter.circle_filled(c, 8.5, rail_fill(dark));
            painter.circle_filled(c, 7.0, p.accent);
            painter.text(
                c,
                egui::Align2::CENTER_CENTER,
                text,
                theme::medium(10.0),
                p.on_fill,
            );
        }
    }
    resp.on_hover_text(label)
}

/// The canvas a screen draws into: fills the remaining area with the window background and a
/// comfortable margin; the content column is capped so wide windows keep a readable measure.
pub fn canvas<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(ui.visuals().panel_fill)
                .inner_margin(egui::Margin::symmetric(28, 24)),
        )
        .show(ui, |ui| {
            let max_w = 1180.0_f32.min(ui.available_width());
            let x = ui.max_rect().min.x + (ui.available_width() - max_w) / 2.0;
            let rect = Rect::from_min_size(
                Pos2::new(x, ui.max_rect().min.y),
                vec2(max_w, ui.available_height()),
            );
            let mut inner = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect)
                    .layout(Layout::top_down(Align::Min)),
            );
            add(&mut inner)
        })
        .inner
}

/// Screen title in the canvas: a large semibold title and an optional one-line lede.
pub fn page_title(ui: &mut Ui, tx: Tx, title: &str, lede: Option<&str>) {
    theme::column(ui, tx.rtl(), |ui| {
        ui.label(
            egui::RichText::new(title)
                .font(theme::semibold(theme::font::TITLE + 4.0))
                .color(theme::tokens(ui).text),
        );
        if let Some(l) = lede {
            ui.label(theme::caption(ui, l));
        }
    });
    ui.add_space(space::L);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rail_lists_every_destination_once() {
        let screens: Vec<Screen> = DESTINATIONS.iter().map(|d| d.0).collect();
        for s in [
            Screen::Home,
            Screen::Devices,
            Screen::Sessions,
            Screen::Settings,
        ] {
            assert_eq!(screens.iter().filter(|x| **x == s).count(), 1, "{s:?}");
        }
    }

    #[test]
    fn rail_fills_are_the_deepest_layer_in_dark_and_lightest_in_light() {
        assert!(theme::luminance(rail_fill(true)) < theme::luminance(theme::DARK.bg));
        assert!(theme::luminance(rail_fill(false)) > theme::luminance(theme::LIGHT.bg));
    }
}
