//! Window chrome for the integrated title bar: minimize / maximize / close buttons, dragging,
//! double-click to maximize, and resizing from the window edges.
//!
//! Everything goes through `egui::ViewportCommand`, which winit turns into the native
//! operations: `StartDrag` runs the OS move loop (so Windows snapping to screen edges and
//! halves keeps working), `BeginResize` the OS resize loop. Nothing here draws a fake frame
//! the OS does not know about.

use eframe::egui::{
    self, pos2, vec2, Color32, CursorIcon, Rect, ResizeDirection, Response, Sense, Stroke, Ui,
    ViewportCommand,
};

use crate::ui::theme;

/// Width of one caption button (the Windows 11 size at 100 %).
pub const BUTTON_W: f32 = 46.0;
/// How far from a window edge the pointer may be to resize (logical points).
const RESIZE_MARGIN: f32 = 5.0;
/// Corner hot zone: larger than the edge, like the native frame.
const RESIZE_CORNER: f32 = 12.0;
/// Windows' close-button hover red.
const CLOSE_HOVER: Color32 = Color32::from_rgb(0xC4, 0x2B, 0x1C);
const CLOSE_PRESSED: Color32 = Color32::from_rgb(0xA3, 0x26, 0x1A);

/// Which caption button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caption {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// The window's current state, from the viewport info.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowState {
    pub maximized: bool,
    pub fullscreen: bool,
}

impl WindowState {
    pub fn read(ctx: &egui::Context) -> Self {
        ctx.input(|i| {
            let v = i.viewport();
            Self {
                maximized: v.maximized.unwrap_or(false),
                fullscreen: v.fullscreen.unwrap_or(false),
            }
        })
    }
}

/// Draws one caption button filling `height`, returns its response. The glyphs are drawn as
/// hairlines snapped to physical pixels, like the system ones.
pub fn caption_button(ui: &mut Ui, kind: Caption, height: f32, tooltip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(BUTTON_W, height), Sense::click());
    // The tooltip is also the accessible name (screen readers, UI tests).
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tooltip));
    let resp = resp.on_hover_text(tooltip);
    let p = theme::tokens(ui);
    let (fill, fg) = match (kind, resp.is_pointer_button_down_on(), resp.hovered()) {
        (Caption::Close, true, _) => (CLOSE_PRESSED, Color32::WHITE),
        (Caption::Close, false, true) => (CLOSE_HOVER, Color32::WHITE),
        (_, true, _) => (p.surface_pressed, p.text),
        (_, false, true) => (p.surface_hover, p.text),
        _ => (Color32::TRANSPARENT, p.text),
    };
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, fill);
    let ppp = ui.ctx().pixels_per_point();
    let px = 1.0 / ppp;
    let snap = |v: f32| (v * ppp).round() / ppp + px / 2.0;
    let c = rect.center();
    let s = 5.0; // half glyph size
    let stroke = Stroke::new(px, fg);
    match kind {
        Caption::Minimize => {
            let y = snap(c.y);
            painter.line_segment([pos2(c.x - s, y), pos2(c.x + s, y)], stroke);
        }
        Caption::Maximize => {
            let r = Rect::from_min_max(
                pos2(snap(c.x - s), snap(c.y - s)),
                pos2(snap(c.x + s), snap(c.y + s)),
            );
            painter.rect_stroke(r, 0.0, stroke, egui::StrokeKind::Middle);
        }
        Caption::Restore => {
            let o = 2.0;
            let back = Rect::from_min_max(
                pos2(snap(c.x - s + o), snap(c.y - s - o + 1.0)),
                pos2(snap(c.x + s + o - 1.0), snap(c.y + s - o)),
            );
            let front = Rect::from_min_max(
                pos2(snap(c.x - s), snap(c.y - s + 1.0)),
                pos2(snap(c.x + s - 1.0), snap(c.y + s)),
            );
            // Only the visible part of the back square.
            painter.line_segment([back.left_top(), back.right_top()], stroke);
            painter.line_segment([back.right_top(), back.right_bottom()], stroke);
            painter.rect_filled(front, 0.0, fill);
            painter.rect_stroke(front, 0.0, stroke, egui::StrokeKind::Middle);
        }
        Caption::Close => {
            let r = s - 0.5;
            painter.line_segment([pos2(c.x - r, c.y - r), pos2(c.x + r, c.y + r)], stroke);
            painter.line_segment([pos2(c.x + r, c.y - r), pos2(c.x - r, c.y + r)], stroke);
        }
    }
    resp
}

/// Runs the command for a clicked caption button.
pub fn apply_caption(ctx: &egui::Context, kind: Caption) {
    match kind {
        Caption::Minimize => ctx.send_viewport_cmd(ViewportCommand::Minimized(true)),
        Caption::Maximize => ctx.send_viewport_cmd(ViewportCommand::Maximized(true)),
        Caption::Restore => ctx.send_viewport_cmd(ViewportCommand::Maximized(false)),
        // The same close request the system button sends: close-to-tray still applies.
        Caption::Close => ctx.send_viewport_cmd(ViewportCommand::Close),
    }
}

/// Makes `resp` (the empty part of the title bar) behave like a caption: drag moves the window
/// with the OS move loop, double-click maximizes or restores.
pub fn drag_area(ctx: &egui::Context, resp: &Response, state: WindowState) {
    if resp.double_clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!state.maximized));
    } else if resp.drag_started_by(egui::PointerButton::Primary) {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    }
}

/// The resize direction for a pointer at `pos` in a window of `size` (logical points), if it
/// is on an edge or corner.
pub fn resize_direction(pos: egui::Pos2, size: egui::Vec2) -> Option<ResizeDirection> {
    if !(pos.x.is_finite() && pos.y.is_finite()) || size.x <= 0.0 || size.y <= 0.0 {
        return None;
    }
    let near = |v: f32, edge: f32, m: f32| (v - edge).abs() <= m;
    let (w, h) = (size.x, size.y);
    let left = near(pos.x, 0.0, RESIZE_MARGIN);
    let right = near(pos.x, w, RESIZE_MARGIN);
    let top = near(pos.y, 0.0, RESIZE_MARGIN);
    let bottom = near(pos.y, h, RESIZE_MARGIN);
    let left_c = pos.x <= RESIZE_CORNER;
    let right_c = pos.x >= w - RESIZE_CORNER;
    let top_c = pos.y <= RESIZE_CORNER;
    let bottom_c = pos.y >= h - RESIZE_CORNER;
    use ResizeDirection as D;
    Some(match () {
        _ if (top && left_c) || (left && top_c) => D::NorthWest,
        _ if (top && right_c) || (right && top_c) => D::NorthEast,
        _ if (bottom && left_c) || (left && bottom_c) => D::SouthWest,
        _ if (bottom && right_c) || (right && bottom_c) => D::SouthEast,
        _ if top => D::North,
        _ if bottom => D::South,
        _ if left => D::West,
        _ if right => D::East,
        _ => return None,
    })
}

fn cursor_for(dir: ResizeDirection) -> CursorIcon {
    match dir {
        ResizeDirection::North => CursorIcon::ResizeNorth,
        ResizeDirection::South => CursorIcon::ResizeSouth,
        ResizeDirection::East => CursorIcon::ResizeEast,
        ResizeDirection::West => CursorIcon::ResizeWest,
        ResizeDirection::NorthEast => CursorIcon::ResizeNorthEast,
        ResizeDirection::NorthWest => CursorIcon::ResizeNorthWest,
        ResizeDirection::SouthEast => CursorIcon::ResizeSouthEast,
        ResizeDirection::SouthWest => CursorIcon::ResizeSouthWest,
    }
}

/// Edge resizing for a window without the system frame. Call once per frame, after the UI,
/// so the resize cursor wins over whatever is under the edge.
pub fn resize_edges(ctx: &egui::Context, state: WindowState) {
    if state.maximized || state.fullscreen {
        return;
    }
    let (pos, size, pressed) = ctx.input(|i| {
        (
            i.pointer.hover_pos(),
            i.content_rect().size(),
            i.pointer.primary_pressed(),
        )
    });
    let Some(dir) = pos.and_then(|p| resize_direction(p, size)) else {
        return;
    };
    ctx.set_cursor_icon(cursor_for(dir));
    if pressed {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_and_corners_resize_the_right_way() {
        let size = vec2(1000.0, 660.0);
        let d = |x, y| resize_direction(pos2(x, y), size);
        assert_eq!(d(500.0, 1.0), Some(ResizeDirection::North));
        assert_eq!(d(500.0, 659.0), Some(ResizeDirection::South));
        assert_eq!(d(0.0, 300.0), Some(ResizeDirection::West));
        assert_eq!(d(999.0, 300.0), Some(ResizeDirection::East));
        assert_eq!(d(2.0, 2.0), Some(ResizeDirection::NorthWest));
        // The corner zone is wider than the edge, along the edge.
        assert_eq!(d(10.0, 1.0), Some(ResizeDirection::NorthWest));
        assert_eq!(d(998.0, 10.0), Some(ResizeDirection::NorthEast));
        assert_eq!(d(1.0, 655.0), Some(ResizeDirection::SouthWest));
        assert_eq!(d(995.0, 659.0), Some(ResizeDirection::SouthEast));
        // Inside: nothing.
        assert_eq!(d(500.0, 300.0), None);
        assert_eq!(d(20.0, 20.0), None);
        assert_eq!(d(f32::NAN, 1.0), None);
        assert_eq!(resize_direction(pos2(0.0, 0.0), egui::Vec2::ZERO), None);
    }
}
