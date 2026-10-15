//! Viewer session: remote frame as a texture, toolbar, stats overlay, input forwarding.

use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{
    self, Color32, ColorImage, EventFilter, Pos2, Rect, RichText, Sense, TextureHandle,
    TextureOptions,
};
use tlp_desk_proto::{InputEvent, MouseButton, Permissions};

use super::chat::{self, ChatState};
use super::file_browser::FileBrowser;
use super::icons::{self, Icon};
use super::key_forward::KeyForwarder;
use super::transfers::{self, TransferPanel};
use super::whiteboard::WhiteboardUi;
use super::{theme, widgets};
use crate::backend::{Backend, FrameSlot, MonitorDesc, PeerInfo, SessionId, SessionStats};
use crate::i18n::{format, ltr_isolate, Tx, S};
use crate::input_map;
use crate::keygrab::KeyGrab;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Scale the remote screen to fit the window, preserving aspect ratio.
    Fit,
    /// One remote pixel per physical screen pixel, with scroll bars.
    Original,
}

pub struct ViewerSession {
    pub id: SessionId,
    pub peer: PeerInfo,
    pub permissions: Permissions,
    frames: Arc<FrameSlot>,
    texture: Option<TextureHandle>,
    frame_size: [usize; 2],
    pub mode: ViewMode,
    pub view_only: bool,
    pub show_stats: bool,
    pub stats: Option<SessionStats>,
    started: Instant,
    /// Session duration to show instead of the wall clock (deterministic README media).
    fixed_elapsed: Option<std::time::Duration>,
    pressed: Vec<MouseButton>,
    last_pos: Option<(u16, u16)>,
    focus_requested: bool,
    /// True while any input may have been sent since the last `ReleaseAll`.
    input_dirty: bool,
    /// OS keyboard grab (Win key, Alt+Tab...) for this session, if available.
    grab: Option<Box<dyn KeyGrab>>,
    /// Whether the grab is currently switched on.
    grab_on: bool,
    /// Keys the window path holds down on the host (fallback when the grab leaks).
    keys: KeyForwarder,
    /// The grab let a key through: it is not capturing (reported to it once per episode).
    grab_leak_reported: bool,
    /// The host user is using their own mouse/keyboard; our input is being ignored.
    pub host_input_priority: bool,
    /// Host monitors (picker shown when there are two or more) and the streamed one.
    monitors: Vec<MonitorDesc>,
    active_monitor: u32,
    /// Port forwarding window (TLP-tunnel).
    pub tunnels: super::tunnels::TunnelsPanel,
    /// Draw-on-screen state (TLP-whiteboard).
    pub whiteboard: WhiteboardUi,
    /// Session recording (TLP-recording), driven by the backend's recording events.
    pub recording: super::recordings::RecordState,
    /// Chat with the host user (side panel).
    pub chat: ChatState,
    /// "Sync clipboard" switch and toast (TLP-clipboard).
    pub clipboard: super::clipboard::ClipboardUi,
    /// Two-pane file browser (TLP-files), toggled from the toolbar.
    pub files: FileBrowser,
    /// File transfer queue (lane files-transfer).
    pub transfers: TransferPanel,
}

/// Fit an image of `size` into `avail`, centered, preserving aspect ratio.
pub fn fit_rect(avail: Rect, size: egui::Vec2) -> Rect {
    if size.x <= 0.0 || size.y <= 0.0 {
        return Rect::from_center_size(avail.center(), egui::Vec2::ZERO);
    }
    let scale = (avail.width() / size.x).min(avail.height() / size.y);
    Rect::from_center_size(avail.center(), size * scale)
}

impl ViewerSession {
    pub fn new(
        id: SessionId,
        peer: PeerInfo,
        permissions: Permissions,
        frames: Arc<FrameSlot>,
    ) -> Self {
        Self {
            id,
            peer,
            permissions,
            frames,
            texture: None,
            frame_size: [0, 0],
            mode: ViewMode::Fit,
            view_only: false,
            show_stats: true,
            stats: None,
            started: Instant::now(),
            fixed_elapsed: None,
            pressed: Vec::new(),
            last_pos: None,
            focus_requested: false,
            input_dirty: false,
            grab: None,
            grab_on: false,
            keys: KeyForwarder::default(),
            grab_leak_reported: false,
            host_input_priority: false,
            monitors: Vec::new(),
            active_monitor: 0,
            tunnels: Default::default(),
            whiteboard: WhiteboardUi::new(),
            recording: Default::default(),
            chat: ChatState::new(permissions.contains(Permissions::CHAT)),
            clipboard: Default::default(),
            files: FileBrowser::new(id),
            transfers: TransferPanel::new(),
        }
    }

    /// The host's monitor list changed or a switch was confirmed.
    pub fn set_monitors(&mut self, monitors: Vec<MonitorDesc>, active: u32) {
        if active != self.active_monitor {
            // New surface: re-send the pointer position on the next move.
            self.last_pos = None;
            self.whiteboard.monitor_changed();
        }
        self.monitors = monitors;
        self.active_monitor = active;
    }

    /// Show `elapsed` as the session duration instead of the wall clock (`None` = wall clock).
    /// Used by the README media generator so screenshots and the demo are reproducible.
    pub fn set_fixed_elapsed(&mut self, elapsed: Option<std::time::Duration>) {
        self.fixed_elapsed = elapsed;
    }

    /// Monitors and the active index, as last reported by the host.
    pub fn monitors(&self) -> (&[MonitorDesc], u32) {
        (&self.monitors, self.active_monitor)
    }

    /// Attach the keyboard grab; it is switched on only while the session view is focused,
    /// not view-only and the host grants `KEYBOARD`.
    pub fn set_key_grab(&mut self, grab: Box<dyn KeyGrab>) {
        self.grab = Some(grab);
        self.grab_on = false;
    }

    /// Whether system keys are currently captured for the remote PC.
    pub fn key_grab_active(&self) -> bool {
        self.grab_on
    }

    fn set_grab(&mut self, on: bool) {
        if self.grab_on == on {
            return;
        }
        if let Some(g) = self.grab.as_mut() {
            g.set_active(on);
            self.grab_on = on;
            self.grab_leak_reported = false;
        }
    }

    fn can_mouse(&self) -> bool {
        !self.view_only && self.permissions.contains(Permissions::CONTROL)
    }

    fn can_keyboard(&self) -> bool {
        self.can_mouse() && self.permissions.contains(Permissions::KEYBOARD)
    }

    fn send(&mut self, backend: &dyn Backend, ev: InputEvent) {
        self.input_dirty = true;
        backend.send_input(self.id, ev);
    }

    fn release_all(&mut self, backend: &dyn Backend) {
        if self.input_dirty || !self.pressed.is_empty() {
            backend.send_input(self.id, InputEvent::ReleaseAll);
        }
        self.pressed.clear();
        self.keys.clear();
        self.input_dirty = false;
    }

    /// Sends a key event from the window path (see [`KeyForwarder`]).
    fn send_key(&mut self, backend: &dyn Backend, ev: InputEvent) {
        let verdict = self.keys.on_event(self.grab_on, ev);
        if verdict.grab_leaked && !self.grab_leak_reported {
            // The OS grab did not swallow this key, so it is not capturing keys. Typing still
            // works through the window path; ask the grab to repair itself.
            self.grab_leak_reported = true;
            tracing::warn!("keyboard grab is on but keys reach the window; using window events");
            if let Some(g) = self.grab.as_mut() {
                g.report_leak();
            }
        }
        if let Some(ev) = verdict.event {
            self.send(backend, ev);
        }
    }

    /// Upload the newest decoded frame, reusing the texture.
    fn update_texture(&mut self, ctx: &egui::Context) {
        let Some(frame) = self.frames.take() else {
            return;
        };
        let (w, h) = (frame.width as usize, frame.height as usize);
        if w == 0 || h == 0 || frame.rgba.len() != w * h * 4 {
            tracing::warn!(
                "dropping malformed frame {}x{} ({} bytes)",
                w,
                h,
                frame.rgba.len()
            );
            return;
        }
        // Frames are opaque, so premultiplied == unmultiplied: no per-pixel math.
        let image = ColorImage::from_rgba_premultiplied([w, h], &frame.rgba);
        // The pixels were copied into the texture image: give the buffer back to the decoder.
        self.frames.recycle(frame.rgba);
        self.frame_size = [w, h];
        match &mut self.texture {
            Some(tex) => tex.set(image, TextureOptions::LINEAR),
            None => {
                self.texture =
                    Some(ctx.load_texture("remote-screen", image, TextureOptions::LINEAR))
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, tx: Tx, backend: &dyn Backend, modal_open: bool) {
        let ctx = ui.ctx().clone();
        self.update_texture(&ctx);
        self.whiteboard
            .update(self.permissions, backend, self.id, Instant::now());
        self.toolbar(ui, tx, backend);
        if self.whiteboard.is_active() {
            self.whiteboard.tools_bar(ui, tx, backend, self.id);
        }
        if self.host_input_priority {
            self.priority_banner(ui, tx);
        }
        // Clicks in the floating port forwarding window are not remote input. (Typing there is
        // not either: keys go to the remote PC only while the remote view has keyboard focus.)
        let window = self
            .tunnels
            .window(&ctx, tx, backend, self.id, self.permissions);
        let over_window = window.is_some_and(|r| {
            ctx.input(|i| i.pointer.hover_pos())
                .is_some_and(|p| r.expand(4.0).contains(p))
        });
        let modal_open = modal_open || over_window;
        self.files.ui(ui, tx, backend, self.permissions);
        let modal_open = modal_open || self.files.modal_open() || self.files.owns_keyboard();
        self.transfers_ui(ui, tx, backend);
        chat::side_panel(ui, tx, &mut self.chat, backend, self.id);

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::from_rgb(0x0B, 0x0D, 0x10)))
            .show(ui, |ui| {
                let Some(tex) = self.texture.clone() else {
                    ui.centered_and_justified(|ui| {
                        ui.label(RichText::new(tx.t(S::WaitingForVideo)).color(Color32::GRAY));
                    });
                    return;
                };
                let tex_size = egui::vec2(self.frame_size[0] as f32, self.frame_size[1] as f32);
                let uv = Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0));
                let (resp, image_rect, visible) = match self.mode {
                    ViewMode::Fit => {
                        let avail = ui.available_rect_before_wrap();
                        let resp = ui.allocate_rect(avail, Sense::click_and_drag());
                        let image_rect = fit_rect(avail, tex_size);
                        ui.painter().image(tex.id(), image_rect, uv, Color32::WHITE);
                        (resp, image_rect, image_rect)
                    }
                    ViewMode::Original => {
                        let size = tex_size / ui.ctx().pixels_per_point();
                        let source = if self.can_mouse() {
                            egui::scroll_area::ScrollSource::SCROLL_BAR
                        } else {
                            egui::scroll_area::ScrollSource::default()
                        };
                        egui::ScrollArea::both()
                            .scroll_source(source)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let (rect, resp) =
                                    ui.allocate_exact_size(size, Sense::click_and_drag());
                                ui.painter().image(tex.id(), rect, uv, Color32::WHITE);
                                let visible = ui.clip_rect().intersect(rect);
                                (resp, rect, visible)
                            })
                            .inner
                    }
                };
                self.whiteboard
                    .paint(ui, image_rect, visible, self.frame_size);
                if self.show_stats {
                    self.stats_overlay(ui, tx, ui.clip_rect());
                }
                if self.whiteboard.is_active() && !modal_open {
                    // The pointer draws; nothing is sent as input (keys neither).
                    self.set_grab(false);
                    self.whiteboard.handle_pointer(
                        ui,
                        image_rect,
                        visible,
                        self.frame_size,
                        backend,
                        self.id,
                        Instant::now(),
                    );
                } else {
                    self.handle_input(ui, backend, &resp, image_rect, visible, modal_open);
                }
            });
    }

    /// File transfer panel, drag and drop of files onto the window, and applying the panel's
    /// requests to the backend.
    fn transfers_ui(&mut self, ui: &mut egui::Ui, tx: Tx, backend: &dyn Backend) {
        let allowed = self.permissions.contains(Permissions::FILES);
        let ctx = ui.ctx().clone();
        TransferPanel::drop_overlay(&ctx, tx, &self.peer.name, allowed);
        let dropped = TransferPanel::take_dropped(&ctx);
        if !dropped.is_empty() {
            self.transfers.open = true;
            if allowed {
                backend.send_files(self.id, dropped);
            }
        }
        for action in self.transfers.ui(ui, tx, allowed) {
            match action {
                transfers::Action::Send(paths) => backend.send_files(self.id, paths),
                transfers::Action::Cancel(id) => backend.cancel_transfer(self.id, id),
                transfers::Action::Retry(id) => backend.retry_transfer(self.id, id),
                transfers::Action::OpenFolder(dir) => transfers::open_folder(&dir),
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, tx: Tx, backend: &dyn Backend) {
        let dark = ui.visuals().dark_mode;
        let mut disconnect = false;
        egui::Panel::top("session-toolbar")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().window_fill)
                    .inner_margin(egui::Margin::symmetric(14, 8)),
            )
            .show(ui, |ui| {
                theme::row(ui, tx.rtl(), |ui| {
                    theme::status_dot(ui, theme::success(dark));
                    ui.label(
                        RichText::new(tx.f(S::SessionWith, &[("name", &self.peer.name)])).strong(),
                    );
                    ui.separator();
                    for (mode, icon, key) in [
                        (ViewMode::Fit, Icon::FitToWindow, S::FitToWindow),
                        (ViewMode::Original, Icon::ActualSize, S::OriginalSize),
                    ] {
                        let selected = self.mode == mode;
                        if icons::icon_toggle(ui, tx, selected, icon, tx.t(key)).clicked() {
                            self.mode = mode;
                        }
                    }
                    ui.separator();
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
                    if icons::icon_toggle(ui, tx, self.show_stats, Icon::Stats, tx.t(S::Stats))
                        .clicked()
                    {
                        self.show_stats = !self.show_stats;
                    }
                    self.tunnels.toolbar_button(ui, tx);
                    let wb_allowed = self.permissions.contains(Permissions::WHITEBOARD);
                    if self.whiteboard.toggle_button(ui, tx, wb_allowed) == Some(true) {
                        // Drawing takes the pointer: nothing may stay pressed on the remote PC.
                        self.set_grab(false);
                        self.release_all(backend);
                    }
                    let (id, perms) = (self.id, self.permissions);
                    self.clipboard.toolbar(ui, tx, id, perms, backend);
                    self.files_buttons(ui, tx, backend);
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
                    if chat::chat_toggle(ui, tx, &self.chat) {
                        self.chat.set_open(!self.chat.is_open());
                    }
                    if let Some(index) = self.monitor_picker(ui, tx) {
                        // Nothing may stay pressed on the old screen.
                        self.release_all(backend);
                        backend.select_monitor(self.id, index);
                    }
                    if self.grab_on {
                        ui.separator();
                        // Accent icon = keys are captured. The caption is shown only when it fits
                        // next to Disconnect; otherwise it moves into the tooltip.
                        let accent = theme::accent(dark);
                        let caption = theme::caption(ui, tx.t(S::KeysCaptured));
                        let caption_w = egui::WidgetText::from(caption.clone())
                            .into_galley(ui, None, f32::INFINITY, egui::TextStyle::Small)
                            .size()
                            .x;
                        let fits = ui.available_width() > caption_w + 200.0;
                        let hint = if fits {
                            tx.t(S::KeysCapturedHint).to_owned()
                        } else {
                            format!("{}\n{}", tx.t(S::KeysCaptured), tx.t(S::KeysCapturedHint))
                        };
                        icons::icon(ui, tx, Icon::Keyboard, icons::BUTTON_ICON, accent)
                            .on_hover_text(&hint);
                        if fits {
                            ui.label(caption).on_hover_text(&hint);
                        }
                    }
                    theme::row_end(ui, tx.rtl(), |ui| {
                        let b = widgets::danger_icon_button(
                            ui,
                            tx,
                            Icon::Disconnect,
                            tx.t(S::Disconnect),
                        );
                        if ui.add(b).clicked() {
                            disconnect = true;
                        }
                        // recording (TLP-recording)
                        match super::recordings::record_toggle(ui, tx, self.recording) {
                            Some(true) => {
                                self.recording = super::recordings::RecordState::Starting;
                                backend.start_recording(self.id);
                            }
                            Some(false) => backend.stop_recording(self.id),
                            None => {}
                        }
                    });
                });
            });
        if disconnect {
            self.release_all(backend);
            backend.disconnect(self.id);
        }
    }

    /// Files toggle, plus a paste button while the remote PC has copied files.
    fn files_buttons(&mut self, ui: &mut egui::Ui, tx: Tx, backend: &dyn Backend) {
        if icons::icon_toggle(ui, tx, self.files.open, Icon::Folder, tx.t(S::FsFiles)).clicked() {
            self.files.open = !self.files.open;
        }
        if self.files.remote_clipboard.is_some()
            && self.permissions.contains(Permissions::FILES)
            && icons::icon_button(ui, tx, Icon::Clipboard, tx.t(S::FsPasteRemote)).clicked()
        {
            backend.fs_transfer(
                self.id,
                crate::backend::files::TransferRequest::PasteRemoteClipboard { local_dir: None },
            );
        }
    }

    /// Buttons "1", "2", ... for the host's monitors (hidden with a single monitor). Returns
    /// the index the user picked, if it differs from the active one. The active monitor is
    /// highlighted only once the host confirms the switch.
    fn monitor_picker(&self, ui: &mut egui::Ui, tx: Tx) -> Option<u32> {
        if self.monitors.len() < 2 {
            return None;
        }
        ui.separator();
        let weak = ui.visuals().weak_text_color();
        // Icon only (the label is its tooltip): the toolbar must fit 760 pt in Arabic too.
        icons::icon(ui, tx, Icon::Monitor, icons::BUTTON_ICON, weak)
            .on_hover_text(tx.t(S::Screens));
        let mut picked = None;
        // Number the screens left to right as they are arranged on the host; the toolbar row
        // itself is mirrored in Arabic by `theme::row`.
        let mut order: Vec<&MonitorDesc> = self.monitors.iter().collect();
        order.sort_by_key(|m| (m.x, m.y, m.index));
        for (n, m) in order.iter().enumerate() {
            let label = tx.n(&(n + 1).to_string());
            let res = tx.digits(&format!("{}×{}", m.width, m.height));
            let mut tip = tx.f(
                S::ScreenTooltip,
                &[("n", &tx.digits(&(n + 1).to_string())), ("res", &res)],
            );
            if m.primary {
                tip.push('\n');
                tip.push_str(tx.t(S::ScreenPrimary));
            }
            let active = m.index == self.active_monitor;
            let resp = ui
                .add(
                    egui::Button::selectable(active, RichText::new(label).strong())
                        .min_size(egui::vec2(26.0, 0.0)),
                )
                .on_hover_text(tip);
            if resp.clicked() && !active {
                picked = Some(m.index);
            }
        }
        picked
    }

    /// Notice shown while the host user has input priority.
    fn priority_banner(&self, ui: &mut egui::Ui, tx: Tx) {
        let dark = ui.visuals().dark_mode;
        let (fill, text) = if dark {
            (
                Color32::from_rgb(0x4A, 0x37, 0x0B),
                Color32::from_rgb(0xFF, 0xE3, 0xA3),
            )
        } else {
            (
                Color32::from_rgb(0xFF, 0xF1, 0xC7),
                Color32::from_rgb(0x5C, 0x40, 0x00),
            )
        };
        egui::Panel::top("host-priority-banner")
            .frame(
                egui::Frame::new()
                    .fill(fill)
                    .inner_margin(egui::Margin::symmetric(14, 6)),
            )
            .show(ui, |ui| {
                theme::row(ui, tx.rtl(), |ui| {
                    icons::icon(ui, tx, Icon::MousePointer, icons::BUTTON_ICON, text);
                    ui.label(RichText::new(tx.t(S::HostUsingInput)).color(text));
                });
            });
        // Keep the notice current even when no frames arrive.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
    }

    fn stats_overlay(&self, ui: &egui::Ui, tx: Tx, area: Rect) {
        let font = egui::FontId::proportional(13.5);
        let galleys: Vec<_> = self
            .stat_lines(tx)
            .into_iter()
            .map(|text| {
                ui.painter()
                    .layout_no_wrap(text, font.clone(), Color32::from_gray(235))
            })
            .collect();
        let pad = 10.0;
        let w = galleys.iter().map(|g| g.size().x).fold(0.0, f32::max) + pad * 2.0;
        let h = galleys.iter().map(|g| g.size().y + 2.0).sum::<f32>() + pad * 2.0;
        let margin = 12.0;
        let min = if tx.rtl() {
            egui::pos2(area.min.x + margin, area.min.y + margin)
        } else {
            egui::pos2(area.max.x - margin - w, area.min.y + margin)
        };
        let rect = Rect::from_min_size(min, egui::vec2(w, h));
        let painter = ui.painter();
        painter.rect_filled(rect, 10.0, Color32::from_black_alpha(185));
        let mut y = rect.min.y + pad;
        for g in galleys {
            let x = if tx.rtl() {
                rect.max.x - pad - g.size().x
            } else {
                rect.min.x + pad
            };
            let gh = g.size().y;
            painter.galley(egui::pos2(x, y), g, Color32::WHITE);
            y += gh + 2.0;
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(500));
    }

    /// The overlay's lines, display-ready ("label: value" in the UI direction).
    fn stat_lines(&self, tx: Tx) -> Vec<String> {
        let d = tx.digits;
        let mut lines: Vec<(S, String)> = Vec::new();
        if let Some(s) = self.stats {
            lines.push((
                S::StatFps,
                format!(
                    "{} {}",
                    format::number(f64::from(s.fps), 0, d),
                    tx.raw(S::UnitFps)
                ),
            ));
            lines.push((
                S::StatLatency,
                format!(
                    "{} {}",
                    format::number(f64::from(s.latency_ms), 0, d),
                    tx.raw(S::UnitMs)
                ),
            ));
            lines.push((S::StatBitrate, format::bitrate(s.bitrate_kbps, tx.lang, d)));
        }
        lines.push((
            S::StatResolution,
            tx.digits(&format!("{}×{}", self.frame_size[0], self.frame_size[1])),
        ));
        lines.push((
            S::StatDuration,
            ltr_isolate(&format::duration(
                self.fixed_elapsed.unwrap_or_else(|| self.started.elapsed()),
                d,
            )),
        ));
        lines
            .iter()
            .map(|(k, v)| tx.p(&format!("{}: {}", tx.raw(*k), v)))
            .collect()
    }

    fn handle_input(
        &mut self,
        ui: &egui::Ui,
        backend: &dyn Backend,
        resp: &egui::Response,
        image_rect: Rect,
        visible: Rect,
        modal_open: bool,
    ) {
        if modal_open {
            self.set_grab(false);
            self.release_all(backend);
            return;
        }
        let events = ui.input(|i| i.events.clone());
        let window_focused = ui.input(|i| i.viewport().focused).unwrap_or(true)
            && !events
                .iter()
                .any(|e| matches!(e, egui::Event::WindowFocused(false)));
        // The session view owns the keyboard unless a text field (chat, a dialog) has it.
        // Toolbar buttons take focus when clicked and the window may have been alt-tabbed
        // away and back: both must hand the keyboard back, or typing silently stops.
        let reclaim = window_focused && !resp.has_focus() && !ui.ctx().text_edit_focused();
        if !self.focus_requested
            || resp.clicked()
            || resp.drag_started()
            || reclaim
            || events
                .iter()
                .any(|e| matches!(e, egui::Event::WindowFocused(true)))
        {
            resp.request_focus();
            self.focus_requested = true;
        }
        // Keep Tab / arrows / Escape on the remote view instead of egui focus navigation.
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                resp.id,
                EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            )
        });
        if resp.lost_focus() {
            self.release_all(backend);
        }

        let mouse_ok = self.can_mouse();
        let hover = ui.input(|i| i.pointer.hover_pos());
        // OS keyboard grab: only while this view has keyboard focus inside a focused window.
        self.set_grab(self.can_keyboard() && resp.has_focus() && window_focused);
        // While grabbed, the hook swallows the keys it forwards, so any key that still reaches
        // the window was not captured and goes through `send_key` (no duplicates possible).
        let keys_ok = self.can_keyboard() && resp.has_focus();
        for ev in events {
            match ev {
                egui::Event::WindowFocused(false) => self.release_all(backend),
                egui::Event::PointerMoved(pos) if mouse_ok => {
                    if visible.contains(pos) || !self.pressed.is_empty() {
                        let p = input_map::normalize_clamped(pos, image_rect);
                        if self.last_pos != Some(p) {
                            self.last_pos = Some(p);
                            self.send(backend, InputEvent::MouseMove { x: p.0, y: p.1 });
                        }
                    }
                }
                egui::Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    ..
                } if mouse_ok => {
                    let b = input_map::mouse_button(button);
                    if pressed {
                        if visible.contains(pos) && !self.pressed.contains(&b) {
                            let p = input_map::normalize_clamped(pos, image_rect);
                            if self.last_pos != Some(p) {
                                self.last_pos = Some(p);
                                self.send(backend, InputEvent::MouseMove { x: p.0, y: p.1 });
                            }
                            self.pressed.push(b);
                            self.send(
                                backend,
                                InputEvent::MouseButton {
                                    button: b,
                                    down: true,
                                },
                            );
                        }
                    } else if let Some(i) = self.pressed.iter().position(|x| *x == b) {
                        self.pressed.swap_remove(i);
                        self.send(
                            backend,
                            InputEvent::MouseButton {
                                button: b,
                                down: false,
                            },
                        );
                    }
                }
                egui::Event::MouseWheel { unit, delta, .. } if mouse_ok => {
                    if hover.is_some_and(|p| visible.contains(p)) {
                        if let Some(w) = input_map::wheel_event(unit, delta) {
                            self.send(backend, w);
                        }
                    }
                }
                egui::Event::Key {
                    key,
                    physical_key,
                    pressed,
                    ..
                } if keys_ok => {
                    if let Some(k) = input_map::key_event(key, physical_key, pressed) {
                        self.send_key(backend, k);
                    }
                }
                // egui turns Ctrl+C/X/V presses into clipboard events; forward them as the
                // key press (the release still arrives as a normal key event).
                egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_) if keys_ok => {
                    let key = match ev {
                        egui::Event::Copy => egui::Key::C,
                        egui::Event::Cut => egui::Key::X,
                        _ => egui::Key::V,
                    };
                    if let Some(k) = input_map::key_event(key, None, true) {
                        self.send_key(backend, k);
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{AttemptId, ConnectTarget, DeviceId, LocalInfo, RequestId, VideoImage};
    use crate::i18n::{DigitStyle, Lang};
    use eframe::egui::{pos2, vec2};
    use std::sync::Mutex;

    /// Backend that records the input the session sends.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<InputEvent>>, Mutex<Vec<u32>>);

    /// Keyboard grab that records how it was switched.
    struct FakeGrab(Arc<Mutex<Vec<bool>>>);

    impl KeyGrab for FakeGrab {
        fn set_active(&mut self, active: bool) {
            self.0.lock().unwrap().push(active);
        }
        fn report_leak(&mut self) {
            LEAKS.with(|l| l.set(l.get() + 1));
        }
    }

    thread_local! {
        static LEAKS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
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
            self.0.lock().unwrap().push(event);
        }
        fn set_view_only(&self, _: SessionId, _: bool) {}
        fn disconnect(&self, _: SessionId) {}
        fn select_monitor(&self, _: SessionId, index: u32) {
            self.1.lock().unwrap().push(index);
        }
    }

    /// Drive a real egui context headlessly through a session and return what it sent.
    fn drive(view_only: bool) -> Vec<InputEvent> {
        drive_with(view_only, None)
    }

    fn drive_with(view_only: bool, grab: Option<Box<dyn KeyGrab>>) -> Vec<InputEvent> {
        let ctx = egui::Context::default();
        icons::install(&ctx);
        let backend = Recorder::default();
        let frames = FrameSlot::new();
        frames.publish(VideoImage {
            width: 100,
            height: 50,
            rgba: vec![255; 100 * 50 * 4],
            seq: 0,
        });
        let peer = PeerInfo {
            device_id: DeviceId(2),
            name: "peer".into(),
            address: "192.168.1.2".parse().unwrap(),
        };
        let mut session = ViewerSession::new(SessionId(1), peer, Permissions::ALL, frames);
        session.view_only = view_only;
        if let Some(g) = grab {
            session.set_key_grab(g);
        }
        let tx = Tx::new(Lang::En, DigitStyle::Western);
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 700.0));
        let mut run = |events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| session.ui(ui, tx, &backend, false));
            out.textures_delta.clear(); // headless: no renderer consumes the uploads
        };
        run(vec![]);
        run(vec![]);
        let p = pos2(500.0, 400.0); // inside the fitted image
        run(vec![
            egui::Event::PointerMoved(p),
            egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
            egui::Event::Key {
                key: egui::Key::A,
                physical_key: Some(egui::Key::A),
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            },
        ]);
        run(vec![egui::Event::WindowFocused(false)]);
        backend.0.into_inner().unwrap()
    }

    #[test]
    fn session_forwards_pointer_keys_and_releases_on_focus_loss() {
        let sent = drive(false);
        assert!(
            matches!(sent.first(), Some(InputEvent::MouseMove { x, y })
                if x.abs_diff(32768) < 700 && *y > 0 && *y < 65535),
            "{sent:?}"
        );
        assert!(sent.contains(&InputEvent::MouseButton {
            button: MouseButton::Left,
            down: true
        }));
        assert!(sent.contains(&InputEvent::Key {
            scancode: 0x1E,
            extended: false,
            down: true
        }));
        assert_eq!(sent.last(), Some(&InputEvent::ReleaseAll), "{sent:?}");
    }

    #[test]
    fn view_only_sends_nothing() {
        assert_eq!(drive(true), vec![]);
    }

    /// Regression: with the grab on, egui keys used to be dropped even when the hook had not
    /// captured them (hook removed by Windows, wrong target window), so nothing could be
    /// typed. A key that reaches the window proves the hook did not swallow it: forward it.
    #[test]
    fn keyboard_grab_follows_focus_and_forwards_keys_it_did_not_capture() {
        LEAKS.with(|l| l.set(0));
        let log = Arc::new(Mutex::new(Vec::new()));
        let sent = drive_with(false, Some(Box::new(FakeGrab(log.clone()))));
        // On while focused, off when the window lost focus; switched only on changes.
        assert_eq!(*log.lock().unwrap(), vec![true, false]);
        assert!(sent.contains(&InputEvent::MouseButton {
            button: MouseButton::Left,
            down: true
        }));
        assert!(sent.contains(&key_ev(0x1E, true)), "{sent:?}");
        assert_eq!(LEAKS.with(|l| l.get()), 1, "the grab is told it leaks");
    }

    fn key_ev(scancode: u16, down: bool) -> InputEvent {
        InputEvent::Key {
            scancode,
            extended: false,
            down,
        }
    }

    fn egui_key(key: egui::Key, pressed: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers: Default::default(),
        }
    }

    /// A headless session in a live egui context.
    struct Rig {
        ctx: egui::Context,
        backend: Recorder,
        session: ViewerSession,
        /// A text field shown next to the session (stands in for a chat box).
        edit: Option<String>,
    }

    impl Rig {
        fn new(grab: Option<Box<dyn KeyGrab>>) -> Self {
            let ctx = egui::Context::default();
            icons::install(&ctx);
            let frames = FrameSlot::new();
            frames.publish(VideoImage {
                width: 100,
                height: 50,
                rgba: vec![255; 100 * 50 * 4],
                seq: 0,
            });
            let peer = PeerInfo {
                device_id: DeviceId(2),
                name: "peer".into(),
                address: "192.168.1.2".parse().unwrap(),
            };
            let mut session = ViewerSession::new(SessionId(1), peer, Permissions::ALL, frames);
            if let Some(g) = grab {
                session.set_key_grab(g);
            }
            let mut rig = Self {
                ctx,
                backend: Recorder::default(),
                session,
                edit: None,
            };
            rig.frame(vec![]);
            rig.frame(vec![]);
            rig
        }

        fn frame(&mut self, events: Vec<egui::Event>) {
            let tx = Tx::new(Lang::En, DigitStyle::Western);
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 700.0))),
                events,
                ..Default::default()
            };
            let (session, backend, edit) = (&mut self.session, &self.backend, &mut self.edit);
            let mut out = self.ctx.run_ui(input, |ui| {
                if let Some(text) = edit {
                    egui::Panel::bottom("chat").show(ui, |ui| {
                        ui.add(egui::TextEdit::singleline(text).id(egui::Id::new("chat-input")));
                    });
                }
                session.ui(ui, tx, backend, false)
            });
            out.textures_delta.clear();
        }

        fn take(&self) -> Vec<InputEvent> {
            std::mem::take(&mut *self.backend.0.lock().unwrap())
        }
    }

    /// Grab capturing: the hook swallows keys, egui sees none, nothing is sent twice. A key-up
    /// the grab let through (key held before the grab) must not become a stray event.
    #[test]
    fn capturing_grab_causes_no_duplicates_or_stray_key_ups() {
        LEAKS.with(|l| l.set(0));
        let mut rig = Rig::new(Some(Box::new(FakeGrab(Default::default()))));
        rig.frame(vec![]);
        assert!(rig.session.key_grab_active());
        rig.take();
        rig.frame(vec![egui_key(egui::Key::AltLeft, false)]);
        assert!(!rig
            .take()
            .iter()
            .any(|e| matches!(e, InputEvent::Key { .. })));
        assert_eq!(LEAKS.with(|l| l.get()), 0);
    }

    /// Regression: clicking a toolbar button moves egui focus to it; typing must then still
    /// reach the host (the view takes the keyboard back), with and without the grab.
    #[test]
    fn keys_flow_after_a_toolbar_widget_took_focus() {
        for grab in [false, true] {
            let g: Option<Box<dyn KeyGrab>> =
                grab.then(|| Box::new(FakeGrab(Default::default())) as Box<dyn KeyGrab>);
            let mut rig = Rig::new(g);
            rig.frame(vec![]);
            rig.take();
            // What a click on a toolbar button does.
            rig.ctx
                .memory_mut(|m| m.request_focus(egui::Id::new("toolbar-button")));
            rig.frame(vec![]);
            rig.frame(vec![
                egui_key(egui::Key::H, true),
                egui_key(egui::Key::H, false),
            ]);
            let sent = rig.take();
            assert!(sent.contains(&key_ev(0x23, true)), "grab={grab}: {sent:?}");
            assert!(sent.contains(&key_ev(0x23, false)), "grab={grab}: {sent:?}");
        }
    }

    /// A focused text field (e.g. a chat box) keeps the keyboard.
    #[test]
    fn text_field_focus_is_not_stolen() {
        let mut rig = Rig::new(None);
        rig.edit = Some(String::new());
        rig.frame(vec![]);
        rig.ctx
            .memory_mut(|m| m.request_focus(egui::Id::new("chat-input")));
        rig.frame(vec![]);
        rig.frame(vec![]);
        rig.take();
        rig.frame(vec![egui_key(egui::Key::H, true)]);
        let sent = rig.take();
        assert!(sent.is_empty(), "{sent:?}");
    }

    /// Typing in the real chat panel (Arabic included) must not leak a single input event to
    /// the remote PC.
    #[test]
    fn typing_in_the_chat_box_sends_no_input_to_the_remote() {
        let mut rig = Rig::new(None);
        rig.session.chat.set_open(true);
        rig.frame(vec![]);
        rig.ctx
            .memory_mut(|m| m.request_focus(egui::Id::new(("chat-input", SessionId(1).0))));
        rig.frame(vec![]);
        rig.frame(vec![]);
        rig.take();
        rig.frame(vec![
            egui_key(egui::Key::H, true),
            egui::Event::Text("مرحبا hi".into()),
            egui_key(egui::Key::H, false),
            egui_key(egui::Key::Backspace, true),
            egui_key(egui::Key::Backspace, false),
        ]);
        let sent = rig.take();
        assert!(sent.is_empty(), "{sent:?}");
    }

    #[test]
    fn keyboard_grab_stays_off_in_view_only() {
        let log = Arc::new(Mutex::new(Vec::new()));
        drive_with(true, Some(Box::new(FakeGrab(log.clone()))));
        assert!(log.lock().unwrap().is_empty());
    }

    fn two_monitors() -> Vec<MonitorDesc> {
        vec![
            MonitorDesc {
                index: 0,
                name: "A".into(),
                width: 3840,
                height: 2400,
                primary: true,
                x: 0,
                y: 0,
            },
            MonitorDesc {
                index: 1,
                name: "B".into(),
                width: 1920,
                height: 1080,
                primary: false,
                x: 3840,
                y: 0,
            },
        ]
    }

    /// The picker appears with 2+ monitors, a click on "2" asks the backend to switch, the
    /// active monitor follows the host's confirmation, and a resized frame is shown at its new
    /// size.
    #[test]
    fn monitor_picker_selects_and_follows_host() {
        for lang in [Lang::En, Lang::Ar] {
            let ctx = egui::Context::default();
            icons::install(&ctx);
            let backend = Recorder::default();
            let frames = FrameSlot::new();
            frames.publish(VideoImage {
                width: 100,
                height: 50,
                rgba: vec![255; 100 * 50 * 4],
                seq: 0,
            });
            let peer = PeerInfo {
                device_id: DeviceId(2),
                name: "peer".into(),
                address: "192.168.1.2".parse().unwrap(),
            };
            let mut session =
                ViewerSession::new(SessionId(1), peer, Permissions::ALL, frames.clone());
            let tx = Tx::new(lang, DigitStyle::Western);
            let screen = Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 700.0));
            let run = |session: &mut ViewerSession, events: Vec<egui::Event>| {
                let input = egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                };
                let mut out = ctx.run_ui(input, |ui| session.ui(ui, tx, &backend, false));
                out.textures_delta.clear();
                out
            };
            run(&mut session, vec![]);
            session.set_monitors(two_monitors(), 0);
            // Find the "2" button by scanning the toolbar row for a clickable spot: click
            // every candidate position until the backend records a selection.
            let mut clicked = false;
            'outer: for xi in 0..120 {
                let x = xi as f32 * 10.0 + 5.0;
                let p = pos2(x, 18.0);
                run(&mut session, vec![egui::Event::PointerMoved(p)]);
                run(
                    &mut session,
                    vec![
                        egui::Event::PointerButton {
                            pos: p,
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            modifiers: Default::default(),
                        },
                        egui::Event::PointerButton {
                            pos: p,
                            button: egui::PointerButton::Primary,
                            pressed: false,
                            modifiers: Default::default(),
                        },
                    ],
                );
                if !backend.1.lock().unwrap().is_empty() {
                    clicked = true;
                    break 'outer;
                }
            }
            assert!(clicked, "no monitor button found ({lang:?})");
            assert_eq!(*backend.1.lock().unwrap(), vec![1]);
            // Not highlighted until the host confirms.
            assert_eq!(session.monitors().1, 0);
            session.set_monitors(two_monitors(), 1);
            assert_eq!(session.monitors().1, 1);
            frames.publish(VideoImage {
                width: 64,
                height: 36,
                rgba: vec![128; 64 * 36 * 4],
                seq: 1,
            });
            run(&mut session, vec![]);
            assert_eq!(session.frame_size, [64, 36]);
        }
    }

    #[test]
    fn picker_hidden_with_one_monitor_and_banner_renders() {
        let ctx = egui::Context::default();
        icons::install(&ctx);
        let backend = Recorder::default();
        let peer = PeerInfo {
            device_id: DeviceId(2),
            name: "peer".into(),
            address: "192.168.1.2".parse().unwrap(),
        };
        let mut session =
            ViewerSession::new(SessionId(1), peer, Permissions::ALL, FrameSlot::new());
        let mut one = two_monitors();
        one.truncate(1);
        session.set_monitors(one, 0);
        session.host_input_priority = true;
        for lang in [Lang::En, Lang::Ar] {
            let tx = Tx::new(lang, DigitStyle::Western);
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 600.0))),
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| session.ui(ui, tx, &backend, false));
            out.textures_delta.clear(); // headless: no renderer consumes the uploads
        }
        assert!(backend.1.lock().unwrap().is_empty());
    }

    /// The resolution value keeps its W×H order inside the Arabic overlay line: it is displayed
    /// exactly like the standalone number `tx.n("1024×768")` (Arabic-Indic digit runs are
    /// pre-reversed for egui, which draws Arabic-font runs right to left).
    #[test]
    fn arabic_overlay_keeps_resolution_order() {
        let frames = FrameSlot::new();
        let peer = PeerInfo {
            device_id: DeviceId(2),
            name: "حاسوب-المكتب".into(),
            address: "192.168.1.2".parse().unwrap(),
        };
        let mut session = ViewerSession::new(SessionId(1), peer, Permissions::ALL, frames);
        session.frame_size = [1024, 768];
        for digits in [DigitStyle::Western, DigitStyle::ArabicIndic] {
            for lang in [Lang::Ar, Lang::En] {
                let tx = Tx::new(lang, digits);
                let want = tx.n("1024×768");
                let lines = session.stat_lines(tx);
                assert!(
                    lines[0].contains(&want),
                    "{lang:?} {digits:?}: {:?}",
                    lines[0]
                );
            }
        }
        // And the standalone form is W first for Western digits.
        assert_eq!(
            Tx::new(Lang::Ar, DigitStyle::Western).n("1024×768"),
            "1024×768"
        );
    }

    #[test]
    fn fit_rect_preserves_aspect_and_centers() {
        let avail = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 1000.0));
        let r = fit_rect(avail, vec2(1920.0, 1080.0));
        assert!((r.width() - 1000.0).abs() < 0.01);
        assert!((r.height() - 562.5).abs() < 0.01);
        assert!((r.center().y - 500.0).abs() < 0.01);
        // Pointer in the letterbox area maps to nothing.
        assert_eq!(input_map::normalize_pointer(pos2(500.0, 100.0), r), None);
        let (x, y) = input_map::normalize_pointer(r.center(), r).unwrap();
        assert!(x.abs_diff(32768) <= 1 && y.abs_diff(32768) <= 1, "{x},{y}");
    }
}
