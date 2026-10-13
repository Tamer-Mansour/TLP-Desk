//! Whiteboard in the viewer session: a "draw on screen" toggle in the session toolbar, a tools
//! bar (pen, highlighter, arrow, rectangle, eraser, colors, widths, fade, undo/redo, clear),
//! pointer handling while drawing (no input reaches the remote PC), and local rendering of the
//! strokes over the video.
//!
//! Strokes are rendered with the same `tiny-skia` code as the host overlay into a texture the
//! size of the decoded frame, updated incrementally (only the damaged rectangle is uploaded).
//! The host's overlay is excluded from capture, so this local copy is what the viewer sees,
//! without waiting for a round trip through capture and encoding.

use std::time::{Duration, Instant};

use eframe::egui::{
    self, Color32, ColorImage, Rect, Sense, TextureHandle, TextureOptions, WidgetInfo, WidgetType,
};
use tlp_desk_input::whiteboard::{
    damage_region, norm_to_px, render_full, render_region, ChannelOrder, Damage, Editor, Rgba,
    Style, Tool, WhiteboardOp,
};
use tlp_desk_proto::Permissions;

use super::icons::{self, Icon};
use super::theme;
use crate::backend::{Backend, SessionId};
use crate::i18n::{Tx, S};
use crate::input_map;

/// How long finished strokes stay when fading is on.
pub const FADE_AFTER: Duration = Duration::from_secs(5);
/// How long the "the other side cleared" notice stays.
const NOTICE_FOR: Duration = Duration::from_secs(4);

/// The color palette, with the names used as tooltips.
pub const PALETTE: [(Rgba, S); 8] = [
    (Rgba::new(0xE5, 0x39, 0x35, 0xFF), S::WbRed),
    (Rgba::new(0xFB, 0x8C, 0x00, 0xFF), S::WbOrange),
    (Rgba::new(0xFD, 0xD8, 0x35, 0xFF), S::WbYellow),
    (Rgba::new(0x43, 0xA0, 0x47, 0xFF), S::WbGreen),
    (Rgba::new(0x1E, 0x88, 0xE5, 0xFF), S::WbBlue),
    (Rgba::new(0x8E, 0x24, 0xAA, 0xFF), S::WbPurple),
    (Rgba::new(0x21, 0x21, 0x21, 0xFF), S::WbBlack),
    (Rgba::new(0xFF, 0xFF, 0xFF, 0xFF), S::WbWhite),
];

/// Line widths (1/10000 of the host monitor height) and their tooltips.
pub const WIDTHS: [(u16, S); 3] = [(30, S::WbThin), (60, S::WbMedium), (120, S::WbThick)];

const TOOLS: [(Tool, Icon, S); 5] = [
    (Tool::Pen, Icon::Rename, S::WbPen),
    (Tool::Highlighter, Icon::Minus, S::WbHighlighter),
    (Tool::Arrow, Icon::Forward, S::WbArrow),
    (Tool::Rectangle, Icon::Stop, S::WbRectangle),
    (Tool::Eraser, Icon::Close, S::WbEraser),
];

/// The whiteboard state of one viewer session.
pub struct WhiteboardUi {
    /// Drawing mode: the pointer draws instead of controlling the remote PC.
    active: bool,
    style: Style,
    editor: Editor,
    texture: Option<TextureHandle>,
    tex_size: [usize; 2],
    notice_until: Option<Instant>,
    /// The streamed monitor changed: wipe both sides at the next update.
    reset_pending: bool,
}

impl Default for WhiteboardUi {
    fn default() -> Self {
        Self::new()
    }
}

impl WhiteboardUi {
    pub fn new() -> Self {
        Self {
            active: false,
            style: Style::default(),
            editor: Editor::new(),
            texture: None,
            tex_size: [0, 0],
            notice_until: None,
            reset_pending: false,
        }
    }

    /// Whether drawing mode is on.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// The current tool, color and width.
    pub fn style(&self) -> Style {
        self.style
    }

    /// The drawing document (strokes as the host shows them).
    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    /// The streamed monitor changed: drawings belong to the old screen, so they are wiped on
    /// both sides at the next [`update`](Self::update).
    pub fn monitor_changed(&mut self) {
        if !self.editor.board().is_empty() || self.editor.is_drawing() {
            self.reset_pending = true;
        }
    }

    /// The host sent an op (its user cleared, or host-side strokes).
    pub fn remote(&mut self, op: &WhiteboardOp, now: Instant) {
        if matches!(op, WhiteboardOp::Clear) && !self.editor.board().is_empty() {
            self.notice_until = Some(now + NOTICE_FOR);
        }
        self.editor.remote(op);
    }

    /// Per-frame housekeeping: follows the permission, streams and fades strokes, sends the
    /// pending ops.
    pub fn update(
        &mut self,
        permissions: Permissions,
        backend: &dyn Backend,
        session: SessionId,
        now: Instant,
    ) {
        if !permissions.contains(Permissions::WHITEBOARD) {
            // Revoked: the host already wiped its screen; nothing may be sent any more.
            self.active = false;
            if !self.editor.board().is_empty() || self.editor.is_drawing() {
                self.editor.reset_local();
            }
            self.editor.take_outbox();
            self.reset_pending = false;
            return;
        }
        if self.reset_pending {
            self.reset_pending = false;
            self.editor.clear_all();
        }
        self.editor.tick(now);
        self.flush(backend, session);
    }

    fn flush(&mut self, backend: &dyn Backend, session: SessionId) {
        let ops = self.editor.take_outbox();
        if !ops.is_empty() {
            backend.send_whiteboard(session, ops);
        }
    }

    /// The "draw on screen" toggle for the session toolbar. Returns `Some(on)` when the user
    /// switched drawing mode.
    pub fn toggle_button(&mut self, ui: &mut egui::Ui, tx: Tx, allowed: bool) -> Option<bool> {
        let resp = icons::icon_toggle_enabled(
            ui,
            tx,
            allowed,
            self.active && allowed,
            Icon::Whiteboard,
            tx.t(S::WbDraw),
        );
        let resp = if allowed {
            resp.on_hover_text(tx.t(S::WbDrawHint))
        } else {
            resp.on_disabled_hover_text(tx.t(S::WbNotAllowed))
        };
        if !resp.clicked() {
            return None;
        }
        self.set_active(!self.active);
        Some(self.active)
    }

    /// Switches drawing mode (a stroke in progress is finished).
    pub fn set_active(&mut self, on: bool) {
        if !on {
            self.editor.end(Instant::now());
        }
        self.active = on;
    }

    /// The tools bar shown under the session toolbar while drawing.
    pub fn tools_bar(
        &mut self,
        ui: &mut egui::Ui,
        tx: Tx,
        backend: &dyn Backend,
        session: SessionId,
    ) {
        let now = Instant::now();
        egui::Panel::top("whiteboard-tools")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().window_fill)
                    .inner_margin(egui::Margin::symmetric(14, 6)),
            )
            .show(ui, |ui| {
                theme::row(ui, tx.rtl(), |ui| {
                    for (tool, icon, label) in TOOLS {
                        let selected = self.style.tool == tool;
                        if icons::icon_toggle(ui, tx, selected, icon, tx.t(label)).clicked() {
                            self.style.tool = tool;
                        }
                    }
                    ui.separator();
                    for (color, name) in PALETTE {
                        let selected = self.style.color == color;
                        if swatch(ui, selected, to_color32(color), tx.t(name)).clicked() {
                            self.style.color = color;
                            if self.style.tool == Tool::Eraser {
                                self.style.tool = Tool::Pen;
                            }
                        }
                    }
                    ui.separator();
                    for (i, (width, name)) in WIDTHS.into_iter().enumerate() {
                        let selected = self.style.width == width;
                        let radius = 2.0 + 2.0 * i as f32;
                        if width_dot(ui, selected, radius, tx.t(name)).clicked() {
                            self.style.width = width;
                        }
                    }
                    ui.separator();
                    let fading = self.editor.fade().is_some();
                    let resp = icons::icon_toggle(ui, tx, fading, Icon::Clock, tx.t(S::WbFade))
                        .on_hover_text(tx.t(S::WbFadeHint));
                    if resp.clicked() {
                        self.editor
                            .set_fade(if fading { None } else { Some(FADE_AFTER) });
                    }
                    ui.separator();
                    let undo = ui.add_enabled_ui(self.editor.can_undo(), |ui| {
                        icons::icon_button(ui, tx, Icon::Undo, tx.t(S::WbUndo))
                    });
                    if undo.inner.clicked() {
                        self.editor.undo();
                    }
                    let redo = ui.add_enabled_ui(self.editor.can_redo(), |ui| {
                        icons::icon_button(ui, tx, Icon::Redo, tx.t(S::WbRedo))
                    });
                    if redo.inner.clicked() {
                        self.editor.redo();
                    }
                    let any = !self.editor.board().is_empty();
                    let clear = ui.add_enabled_ui(any, |ui| {
                        icons::icon_button(ui, tx, Icon::Trash, tx.t(S::WbClearAll))
                    });
                    if clear.inner.clicked() {
                        self.editor.clear_all();
                    }
                    if self.notice_until.is_some_and(|t| now < t) {
                        ui.separator();
                        ui.label(theme::caption(ui, tx.t(S::WbHostCleared)));
                        ui.ctx().request_repaint_after(Duration::from_millis(500));
                    }
                });
            });
        self.flush(backend, session);
    }

    /// Pointer handling while drawing mode is on: presses inside the visible image start a
    /// stroke, drags extend it, the release ends it. Nothing is sent as input.
    #[allow(clippy::too_many_arguments)]
    pub fn handle_pointer(
        &mut self,
        ui: &egui::Ui,
        image_rect: Rect,
        visible: Rect,
        frame_size: [usize; 2],
        backend: &dyn Backend,
        session: SessionId,
        now: Instant,
    ) {
        if frame_size[1] > 0 {
            self.editor
                .set_aspect(frame_size[0] as f64 / frame_size[1] as f64);
        }
        let hover = ui.input(|i| i.pointer.hover_pos());
        if hover.is_some_and(|p| visible.contains(p)) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        let events = ui.input(|i| i.events.clone());
        for ev in events {
            match ev {
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    ..
                } if visible.contains(pos) => {
                    let p = input_map::normalize_clamped(pos, image_rect);
                    self.editor.begin(p, self.style, now);
                }
                egui::Event::PointerMoved(pos) if self.editor.is_drawing() => {
                    let p = input_map::normalize_clamped(pos, image_rect);
                    self.editor.drag(p, now);
                }
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    ..
                } if self.editor.is_drawing() => {
                    let p = input_map::normalize_clamped(pos, image_rect);
                    self.editor.drag(p, now);
                    self.editor.end(now);
                }
                egui::Event::PointerGone | egui::Event::WindowFocused(false)
                    if self.editor.is_drawing() =>
                {
                    self.editor.end(now);
                }
                _ => {}
            }
        }
        self.flush(backend, session);
        if self.editor.is_drawing() {
            // Keep streaming while the pointer rests.
            ui.ctx().request_repaint_after(Duration::from_millis(16));
        }
    }

    /// Draws the strokes over the image (`image_rect`, clipped to `visible`), plus the
    /// gesture in progress.
    pub fn paint(
        &mut self,
        ui: &egui::Ui,
        image_rect: Rect,
        visible: Rect,
        frame_size: [usize; 2],
    ) {
        self.sync_texture(ui.ctx(), frame_size);
        let painter = ui.painter_at(visible);
        if let Some(tex) = &self.texture {
            if !self.editor.board().is_empty() {
                let uv = Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
                painter.image(tex.id(), image_rect, uv, Color32::WHITE);
            }
        }
        if let Some(preview) = self.editor.preview() {
            let [w, h] = frame_size;
            if w == 0 || h == 0 || image_rect.height() <= 0.0 {
                return;
            }
            let sx = image_rect.width() / w as f32;
            let sy = image_rect.height() / h as f32;
            let pts: Vec<egui::Pos2> = preview
                .points
                .iter()
                .map(|&p| {
                    let (x, y) = norm_to_px(p, w as u32, h as u32);
                    egui::pos2(image_rect.min.x + x * sx, image_rect.min.y + y * sy)
                })
                .collect();
            let width = (f32::from(preview.width) / 10_000.0 * image_rect.height()).max(1.0);
            let color = to_color32(preview.color);
            painter.add(egui::Shape::line(
                pts.clone(),
                egui::Stroke::new(width, color),
            ));
            if preview.color.a() == 0xFF {
                for p in [pts.first(), pts.last()].into_iter().flatten() {
                    painter.circle_filled(*p, width / 2.0, color);
                }
            }
        }
        if self.editor.fade().is_some() && !self.editor.board().is_empty() {
            // Fading needs ticks even without input.
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
    }

    /// Re-renders what changed into the texture (the decoded frame's size).
    fn sync_texture(&mut self, ctx: &egui::Context, frame_size: [usize; 2]) {
        let [w, h] = frame_size;
        let (Ok(w32), Ok(h32)) = (u32::try_from(w), u32::try_from(h)) else {
            return;
        };
        if w == 0 || h == 0 {
            return;
        }
        if self.texture.is_none() && self.editor.board().is_empty() {
            // Nothing drawn yet: no texture needed.
            self.editor.take_damage();
            return;
        }
        if self.texture.is_none() || self.tex_size != frame_size {
            self.editor.take_damage();
            let Some(full) = render_full(self.editor.board(), w32, h32, ChannelOrder::Rgba) else {
                return;
            };
            let image = ColorImage::from_rgba_premultiplied([w, h], full.data());
            match &mut self.texture {
                Some(tex) => tex.set(image, TextureOptions::LINEAR),
                None => {
                    self.texture =
                        Some(ctx.load_texture("whiteboard", image, TextureOptions::LINEAR))
                }
            }
            self.tex_size = frame_size;
            return;
        }
        let damage = self.editor.take_damage();
        if damage == Damage::None {
            return;
        }
        let Some(region) = damage_region(self.editor.board(), &damage, w32, h32) else {
            return;
        };
        let Some(pix) = render_region(self.editor.board(), w32, h32, region, ChannelOrder::Rgba)
        else {
            return;
        };
        let image = ColorImage::from_rgba_premultiplied(
            [region.width as usize, region.height as usize],
            pix.data(),
        );
        if let Some(tex) = &mut self.texture {
            tex.set_partial(
                [region.x as usize, region.y as usize],
                image,
                TextureOptions::LINEAR,
            );
        }
    }
}

fn to_color32(c: Rgba) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), c.a())
}

/// A round color swatch; a ring marks the selected one.
fn swatch(ui: &mut egui::Ui, selected: bool, color: Color32, name: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), Sense::click());
    let painter = ui.painter();
    let c = rect.center();
    if selected {
        painter.circle_stroke(
            c,
            10.0,
            egui::Stroke::new(2.0, ui.visuals().strong_text_color()),
        );
    } else if resp.hovered() {
        painter.circle_stroke(c, 10.0, ui.visuals().widgets.hovered.bg_stroke);
    }
    painter.circle_filled(c, 7.0, color);
    painter.circle_stroke(c, 7.0, egui::Stroke::new(1.0, Color32::from_gray(0x80)));
    resp.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, true, selected, name));
    resp.on_hover_text(name)
}

/// A line-width choice drawn as a dot of that size.
fn width_dot(ui: &mut egui::Ui, selected: bool, radius: f32, name: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), Sense::click());
    let painter = ui.painter();
    let c = rect.center();
    if selected {
        painter.circle_stroke(
            c,
            10.0,
            egui::Stroke::new(2.0, ui.visuals().strong_text_color()),
        );
    } else if resp.hovered() {
        painter.circle_stroke(c, 10.0, ui.visuals().widgets.hovered.bg_stroke);
    }
    painter.circle_filled(c, radius, ui.visuals().text_color());
    resp.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, true, selected, name));
    resp.on_hover_text(name)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use eframe::egui::{pos2, vec2, Pos2};
    use tlp_desk_proto::InputEvent;

    use super::*;
    use crate::backend::{
        AttemptId, ConnectTarget, DeviceId, FrameSlot, LocalInfo, MonitorDesc, PeerInfo, RequestId,
        VideoImage,
    };
    use crate::i18n::{DigitStyle, Lang};
    use crate::ui::session::ViewerSession;

    /// Records input and whiteboard ops the session sends.
    #[derive(Default)]
    struct Recorder {
        input: Mutex<Vec<InputEvent>>,
        whiteboard: Mutex<Vec<WhiteboardOp>>,
    }

    impl Backend for Recorder {
        fn local_info(&self) -> LocalInfo {
            LocalInfo {
                device_id: DeviceId(1),
                device_name: String::new(),
                addresses: vec![],
                one_time_code: String::new(),
            }
        }
        fn set_device_name(&self, _: String) {}
        fn refresh_one_time_code(&self) {}
        fn connect(&self, _: ConnectTarget, _: Option<String>) -> AttemptId {
            AttemptId(0)
        }
        fn cancel_connect(&self, _: AttemptId) {}
        fn forget_host_key(&self, _: AttemptId) -> bool {
            false
        }
        fn respond_to_request(&self, _: RequestId, _: bool) {}
        fn send_input(&self, _: SessionId, event: InputEvent) {
            self.input.lock().unwrap().push(event);
        }
        fn set_view_only(&self, _: SessionId, _: bool) {}
        fn disconnect(&self, _: SessionId) {}
        fn select_monitor(&self, _: SessionId, _: u32) {}
        fn send_whiteboard(&self, _: SessionId, ops: Vec<WhiteboardOp>) {
            self.whiteboard.lock().unwrap().extend(ops);
        }
    }

    struct Driver {
        ctx: egui::Context,
        backend: Recorder,
        session: ViewerSession,
        tx: Tx,
    }

    impl Driver {
        fn new(perms: Permissions, lang: Lang) -> Self {
            let ctx = egui::Context::default();
            icons::install(&ctx);
            let frames = FrameSlot::new();
            frames.publish(VideoImage {
                width: 160,
                height: 90,
                rgba: vec![255; 160 * 90 * 4],
                seq: 0,
            });
            let peer = PeerInfo {
                device_id: DeviceId(2),
                name: "مكتب المحاسبة".into(),
                address: "192.168.1.2".parse().unwrap(),
            };
            let mut d = Self {
                ctx,
                backend: Recorder::default(),
                session: ViewerSession::new(SessionId(1), peer, perms, frames),
                tx: Tx::new(lang, DigitStyle::Western),
            };
            d.run(vec![]);
            d.run(vec![]);
            d
        }

        fn run(&mut self, events: Vec<egui::Event>) {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 800.0))),
                events,
                ..Default::default()
            };
            let (session, backend, tx) = (&mut self.session, &self.backend, self.tx);
            let mut out = self
                .ctx
                .run_ui(input, |ui| session.ui(ui, tx, backend, false));
            out.textures_delta.clear();
        }

        fn press(&mut self, p: egui::Pos2, pressed: bool) {
            self.run(vec![
                egui::Event::PointerMoved(p),
                egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                },
            ]);
        }

        fn scribble(&mut self) {
            // The 16:9 frame is fitted into the middle of the window.
            self.press(pos2(400.0, 450.0), true);
            for i in 1..=20 {
                self.run(vec![egui::Event::PointerMoved(pos2(
                    400.0 + i as f32 * 20.0,
                    450.0 + (i % 3) as f32 * 10.0,
                ))]);
            }
            self.press(pos2(800.0, 460.0), false);
        }

        fn ops(&self) -> Vec<WhiteboardOp> {
            self.backend.whiteboard.lock().unwrap().clone()
        }
    }

    const WITH_WB: Permissions = Permissions(Permissions::ALL.0);

    #[test]
    fn drawing_mode_sends_strokes_and_no_input() {
        for lang in [Lang::En, Lang::Ar] {
            let mut d = Driver::new(WITH_WB, lang);
            d.session.whiteboard.set_active(true);
            d.scribble();
            assert!(d.backend.input.lock().unwrap().is_empty(), "{lang:?}");
            let ops = d.ops();
            assert!(ops.len() >= 2, "streamed in chunks: {ops:?}");
            let mut host = tlp_desk_input::whiteboard::Board::new();
            for op in &ops {
                host.apply(op);
            }
            assert_eq!(
                host.strokes(),
                d.session.whiteboard.editor().board().strokes()
            );
            let s = &host.strokes()[0];
            assert!(s.points.len() >= 2);
            assert_eq!(s.color, Style::default().color);
            // Points span the middle of the remote screen, left to right.
            assert!(s.points[0].0 < s.points[s.points.len() - 1].0);
        }
    }

    #[test]
    fn without_the_permission_the_pointer_controls_the_remote_pc() {
        let mut d = Driver::new(
            Permissions::ALL.difference(Permissions::WHITEBOARD),
            Lang::En,
        );
        d.session.whiteboard.set_active(true);
        d.run(vec![]);
        assert!(!d.session.whiteboard.is_active());
        d.scribble();
        assert!(d.ops().is_empty());
        assert!(!d.backend.input.lock().unwrap().is_empty());
    }

    #[test]
    fn live_revocation_drops_drawings_and_mode() {
        let mut d = Driver::new(WITH_WB, Lang::Ar);
        d.session.whiteboard.set_active(true);
        d.scribble();
        assert!(!d.session.whiteboard.editor().board().is_empty());
        let sent = d.ops().len();
        d.session.permissions = Permissions::ALL.difference(Permissions::WHITEBOARD);
        d.run(vec![]);
        assert!(!d.session.whiteboard.is_active());
        assert!(d.session.whiteboard.editor().board().is_empty());
        // Nothing more is sent (the host wiped its screen itself).
        assert_eq!(d.ops().len(), sent);
    }

    #[test]
    fn remote_clear_and_monitor_switch_wipe_the_board() {
        let mut d = Driver::new(WITH_WB, Lang::En);
        d.session.whiteboard.set_active(true);
        d.scribble();
        d.session
            .whiteboard
            .remote(&WhiteboardOp::Clear, Instant::now());
        assert!(d.session.whiteboard.editor().board().is_empty());
        d.run(vec![]);
        d.scribble();
        let mon = |index, x| MonitorDesc {
            index,
            name: format!("شاشة {index}"),
            width: 1920,
            height: 1080,
            primary: index == 0,
            x,
            y: 0,
        };
        d.session.set_monitors(vec![mon(0, 0), mon(1, 1920)], 0);
        d.session.set_monitors(vec![mon(0, 0), mon(1, 1920)], 1);
        d.run(vec![]);
        assert!(d.session.whiteboard.editor().board().is_empty());
        assert_eq!(d.ops().last(), Some(&WhiteboardOp::Clear));
    }

    #[test]
    fn tools_change_the_style_and_undo_resyncs() {
        let mut d = Driver::new(WITH_WB, Lang::En);
        d.session.whiteboard.set_active(true);
        d.scribble();
        d.scribble();
        assert_eq!(d.session.whiteboard.editor().board().strokes().len(), 2);
        // Undo through the editor the tools bar drives: Clear + the remaining stroke.
        d.session.whiteboard.editor.undo();
        d.run(vec![]);
        let ops = d.ops();
        let tail: Vec<_> = ops.iter().rev().take(2).collect();
        assert!(matches!(tail[1], WhiteboardOp::Clear), "{ops:?}");
        assert!(matches!(tail[0], WhiteboardOp::Stroke(_)));
        d.session.whiteboard.style.tool = Tool::Highlighter;
        d.scribble();
        let last = d
            .session
            .whiteboard
            .editor()
            .board()
            .strokes()
            .last()
            .cloned()
            .unwrap();
        assert_eq!(
            last.color.a(),
            tlp_desk_input::whiteboard::HIGHLIGHTER_ALPHA
        );
        // Rendering the texture and preview does not panic at odd sizes.
        let mut wb = WhiteboardUi::new();
        wb.editor = d.session.whiteboard.editor.clone();
        wb.editor.invalidate();
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(300.0, 1.0));
            wb.paint(ui, r, r, [3, 1]);
            wb.paint(ui, r, r, [0, 0]);
        });
        out.textures_delta.clear(); // headless: no renderer consumes the uploads
    }
}
