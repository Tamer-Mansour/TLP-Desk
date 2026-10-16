//! The real notification-area icon (`tray-icon` crate; Win32 `Shell_NotifyIcon` underneath).
//!
//! Must be created on the UI thread while the eframe/winit event loop runs: on Windows the
//! icon's hidden message window is pumped by that loop. Menu and icon events arrive on
//! `tray-icon`'s global handlers, which forward them to a channel and wake egui so they are
//! handled even while the main window is in the tray.

use std::sync::mpsc::{channel, Receiver};

use anyhow::Context as _;
use eframe::egui;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use super::model::{menu, TrayAction, TrayEntry};
use crate::i18n::Lang;

/// Edge length of the tray icon bitmap (Windows scales it to the DPI).
const ICON_SIZE: u32 = 32;

enum Raw {
    Menu(MenuId),
    Icon(TrayIconEvent),
}

/// A live tray icon with its menu.
pub struct SystemTray {
    icon: TrayIcon,
    events: Receiver<Raw>,
    ids: Vec<(MenuId, TrayAction)>,
    incoming: Option<CheckMenuItem>,
    lang: Lang,
    tooltip: String,
}

/// Downscale tightly packed RGBA by averaging `factor`x`factor` blocks (premultiplied while
/// averaging, so transparent edges do not darken).
pub fn downscale_rgba(rgba: &[u8], width: u32, height: u32, size: u32) -> Option<Vec<u8>> {
    let expected = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)?;
    if rgba.len() != expected || size == 0 || width < size || height < size {
        return None;
    }
    let (w, h, s) = (width as usize, height as usize, size as usize);
    let mut out = Vec::with_capacity(s * s * 4);
    for oy in 0..s {
        for ox in 0..s {
            let (x0, x1) = (ox * w / s, ((ox + 1) * w / s).max(ox * w / s + 1));
            let (y0, y1) = (oy * h / s, ((oy + 1) * h / s).max(oy * h / s + 1));
            let mut acc = [0u64; 4];
            let mut n = 0u64;
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) * 4;
                    let px = rgba.get(i..i + 4)?;
                    let a = u64::from(px[3]);
                    acc[0] += u64::from(px[0]) * a;
                    acc[1] += u64::from(px[1]) * a;
                    acc[2] += u64::from(px[2]) * a;
                    acc[3] += a;
                    n += 1;
                }
            }
            let alpha_sum = acc[3].max(1);
            for c in &acc[..3] {
                out.push(u8::try_from(c / alpha_sum).unwrap_or(u8::MAX));
            }
            out.push(u8::try_from(acc[3] / n.max(1)).unwrap_or(u8::MAX));
        }
    }
    Some(out)
}

fn tray_bitmap() -> anyhow::Result<tray_icon::Icon> {
    let full =
        eframe::icon_data::from_png_bytes(include_bytes!("../../assets/icon/TLP-Desk-256.png"))
            .unwrap_or_else(|_| crate::ui::theme::app_icon());
    let small = downscale_rgba(&full.rgba, full.width, full.height, ICON_SIZE)
        .context("app icon has an unexpected size")?;
    tray_icon::Icon::from_rgba(small, ICON_SIZE, ICON_SIZE).context("building the tray icon")
}

/// The menu, which action each item id stands for, and the incoming on/off check item.
type BuiltMenu = (Menu, Vec<(MenuId, TrayAction)>, Option<CheckMenuItem>);

fn build_menu(lang: Lang, incoming_enabled: bool) -> anyhow::Result<BuiltMenu> {
    let m = Menu::new();
    let mut ids = Vec::new();
    let mut incoming = None;
    for entry in menu(lang, incoming_enabled) {
        match entry {
            TrayEntry::Separator => m.append(&PredefinedMenuItem::separator())?,
            TrayEntry::Item {
                action,
                label,
                checked: Some(on),
            } => {
                let item = CheckMenuItem::new(label, true, on, None);
                m.append(&item)?;
                ids.push((item.id().clone(), action));
                incoming = Some(item);
            }
            TrayEntry::Item {
                action,
                label,
                checked: None,
            } => {
                let item = MenuItem::new(label, true, None);
                m.append(&item)?;
                ids.push((item.id().clone(), action));
            }
        }
    }
    Ok((m, ids, incoming))
}

impl SystemTray {
    /// Create the icon. `ctx` is woken on every tray event.
    ///
    /// # Errors
    /// The OS refused the icon (e.g. no notification area / Explorer not running).
    pub fn new(
        ctx: &egui::Context,
        lang: Lang,
        incoming_enabled: bool,
        tooltip: &str,
    ) -> anyhow::Result<Self> {
        let (tx, rx) = channel();
        let menu_tx = tx.clone();
        let menu_ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let _ = menu_tx.send(Raw::Menu(e.id));
            menu_ctx.request_repaint();
        }));
        let icon_ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            let _ = tx.send(Raw::Icon(e));
            icon_ctx.request_repaint();
        }));
        let (m, ids, incoming) = build_menu(lang, incoming_enabled)?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(m))
            .with_menu_on_left_click(false)
            .with_tooltip(tooltip)
            .with_icon(tray_bitmap()?)
            .build()
            .context("creating the tray icon")?;
        Ok(Self {
            icon,
            events: rx,
            ids,
            incoming,
            lang,
            tooltip: tooltip.to_owned(),
        })
    }

    /// Actions picked since the last call. A left click on the icon opens the window.
    pub fn poll(&self) -> Vec<TrayAction> {
        let mut out = Vec::new();
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                Raw::Menu(id) => {
                    if let Some((_, a)) = self.ids.iter().find(|(i, _)| *i == id) {
                        out.push(*a);
                    }
                }
                Raw::Icon(TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                })
                | Raw::Icon(TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                }) => out.push(TrayAction::Open),
                Raw::Icon(_) => {}
            }
        }
        out
    }

    /// Keep the menu language, the incoming check mark and the tooltip current. Cheap when
    /// nothing changed.
    pub fn update(&mut self, lang: Lang, incoming_enabled: bool, tooltip: &str) {
        if lang != self.lang {
            match build_menu(lang, incoming_enabled) {
                Ok((m, ids, incoming)) => {
                    self.icon.set_menu(Some(Box::new(m)));
                    self.ids = ids;
                    self.incoming = incoming;
                    self.lang = lang;
                }
                Err(e) => tracing::warn!("cannot rebuild the tray menu: {e:#}"),
            }
        }
        if let Some(item) = &self.incoming {
            if item.is_checked() != incoming_enabled {
                item.set_checked(incoming_enabled);
            }
        }
        if tooltip != self.tooltip {
            if let Err(e) = self.icon.set_tooltip(Some(tooltip)) {
                tracing::debug!("cannot set the tray tooltip: {e}");
            }
            self.tooltip = tooltip.to_owned();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downscale_averages_blocks_and_checks_sizes() {
        // 4x4 opaque red, scaled to 2x2.
        let red: Vec<u8> = [255, 0, 0, 255].repeat(16);
        let out = downscale_rgba(&red, 4, 4, 2).unwrap();
        assert_eq!(out, [255, 0, 0, 255].repeat(4));
        // Half transparent block keeps the colour, halves alpha.
        let mut mixed = [0u8, 0, 255, 255].repeat(2);
        mixed.extend([0u8, 0, 0, 0].repeat(2));
        let out = downscale_rgba(&mixed, 2, 2, 1).unwrap();
        assert_eq!(out, [0, 0, 255, 127]);
        assert!(downscale_rgba(&red, 4, 4, 8).is_none(), "no upscaling");
        assert!(
            downscale_rgba(&red[..10], 4, 4, 2).is_none(),
            "short buffer"
        );
        assert!(downscale_rgba(&red, 4, 4, 0).is_none());
    }

    #[test]
    fn the_app_icon_downscales_to_tray_size() {
        let full =
            eframe::icon_data::from_png_bytes(include_bytes!("../../assets/icon/TLP-Desk-256.png"))
                .unwrap();
        let small = downscale_rgba(&full.rgba, full.width, full.height, ICON_SIZE).unwrap();
        assert_eq!(small.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
    }
}
