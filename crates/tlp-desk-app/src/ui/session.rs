//! Viewer session: remote frame as a texture, toolbar, stats overlay, input forwarding.

use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{
    self, Color32, ColorImage, EventFilter, Pos2, Rect, RichText, Sense, TextureHandle,
};
use tlp_desk_proto::{InputEvent, MouseButton, Permissions};

mod toolbar;

use super::chat::{self, ChatState};
use super::file_browser::FileBrowser;
use super::icons::{self, Icon};
use super::key_forward::KeyForwarder;
use super::perf;
use super::privacy::PrivacyUi;
use super::transfers::{self, TransferPanel};
use super::whiteboard::WhiteboardUi;
use super::{theme, view_scale, widgets};
use crate::backend::{Backend, FrameSlot, MonitorDesc, PeerInfo, SessionId, SessionStats};
use crate::i18n::{format, ltr_isolate, Tx, S};
use crate::input_map;
use crate::keygrab::KeyGrab;
use crate::settings::QualityChoice;
use crate::text_route::TextRouter;

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
    /// Sharp display of the frame: filter choice and still-screen pre-scaling.
    view: super::view_scale::RemoteView,
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
    /// Privacy mode of the host (toolbar toggle, confirmation, banner).
    pub privacy: PrivacyUi,
    /// System menu, confirmations and restart progress (TLP-unattended).
    pub unattended: super::unattended::SessionUnattended,
    /// TLP-service: the host's secure-desktop / lock state, once reported.
    pub remote_desktop: Option<crate::backend::service::RemoteDesktop>,
    /// Quality preset requested from the host (toolbar menu; starts as the Settings default).
    pub quality: QualityChoice,
    /// Remote device-info window (toolbar button).
    pub device_info: super::device_info::DeviceInfoPanel,
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
    /// The remote computer's sound (toolbar toggle and volume).
    pub audio: super::audio::AudioControls,
    /// Scancode / Unicode-text routing of typed characters (`text_route`).
    pub typing: TextRouter,
    /// High-resolution wheel remainder (TLP-multiviewer).
    wheel: input_map::WheelAccumulator,
    /// Where the toolbar put its items in the last frame (responsive layout, tests).
    pub toolbar_layout: toolbar::ToolbarLayout,
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
            view: Default::default(),
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
            privacy: PrivacyUi::default(),
            unattended: Default::default(),
            remote_desktop: None,
            quality: QualityChoice::default(),
            device_info: Default::default(),
            tunnels: Default::default(),
            whiteboard: WhiteboardUi::new(),
            recording: Default::default(),
            chat: ChatState::new(permissions.contains(Permissions::CHAT)),
            clipboard: Default::default(),
            files: FileBrowser::new(id),
            transfers: TransferPanel::new(),
            audio: Default::default(),
            typing: TextRouter::default(),
            wheel: Default::default(),
            toolbar_layout: Default::default(),
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
        self.typing.clear();
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
        self.view
            .set_frame(ctx, &mut self.texture, image, Instant::now());
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, tx: Tx, backend: &dyn Backend, modal_open: bool) {
        let ctx = ui.ctx().clone();
        self.update_texture(&ctx);
        self.whiteboard
            .update(self.permissions, backend, self.id, Instant::now());
        self.toolbar(ui, tx, backend);
        self.clipboard.show_toast(&ctx, tx);
        self.device_info.window(&ctx, tx, backend, self.id);
        if self.whiteboard.is_active() {
            self.whiteboard.tools_bar(ui, tx, backend, self.id);
        }
        if self.host_input_priority {
            self.priority_banner(ui, tx);
        }
        self.privacy.banner(ui, tx);
        if let Some(on) = self.privacy.dialog(&ctx, tx, &self.peer.name) {
            backend.set_privacy_mode(self.id, on);
        }
        let modal_open = modal_open || self.privacy.confirming();
        if let Some(state) = self.remote_desktop {
            super::service::desktop_banner(ui, tx, state);
        }
        // Restart / lock confirmations and the reconnect overlay block input (TLP-unattended).
        let confirm_open = self
            .unattended
            .dialogs(&ctx, tx, backend, self.id, &self.peer);
        let modal_open = modal_open || confirm_open || self.unattended.blocks_input();
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
                let area = ui.max_rect();
                let Some(mut tex) = self.texture.clone() else {
                    ui.centered_and_justified(|ui| {
                        ui.label(RichText::new(tx.t(S::WaitingForVideo)).color(Color32::GRAY));
                    });
                    if self.unattended.overlay(ui, tx, area) {
                        backend.cancel_reconnect(self.id);
                    }
                    return;
                };
                let ppp = ui.ctx().pixels_per_point();
                let now = Instant::now();
                let uv = Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0));
                let (resp, image_rect, visible) = match self.mode {
                    ViewMode::Fit => {
                        let avail = ui.available_rect_before_wrap();
                        let resp = ui.allocate_rect(avail, Sense::click_and_drag());
                        let image_rect = view_scale::fit_rect_snapped(avail, self.frame_size, ppp);
                        let id = self.view.texture_for(ui.ctx(), &mut tex, image_rect, now);
                        ui.painter().image(id, image_rect, uv, Color32::WHITE);
                        (resp, image_rect, image_rect)
                    }
                    ViewMode::Original => {
                        let size =
                            egui::vec2(self.frame_size[0] as f32, self.frame_size[1] as f32) / ppp;
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
                                // One texel per physical pixel, at a physical-pixel origin.
                                let rect =
                                    view_scale::original_rect(rect.min, self.frame_size, ppp);
                                let id = self.view.texture_for(ui.ctx(), &mut tex, rect, now);
                                ui.painter().image(id, rect, uv, Color32::WHITE);
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
                if self.unattended.overlay(ui, tx, area) {
                    backend.cancel_reconnect(self.id);
                }
            });
    }

    /// File transfer panel, drag and drop of files onto the window, and applying the panel's
    /// requests to the backend.
    fn transfers_ui(&mut self, ui: &mut egui::Ui, tx: Tx, backend: &dyn Backend) {
        let allowed = self.permissions.contains(Permissions::FILES);
        let ctx = ui.ctx().clone();
        // Over the file browser's panes the browser shows where the files would go.
        if !self.files.drop_hover_handled() {
            TransferPanel::drop_overlay(&ctx, tx, &self.peer.name, allowed);
        }
        let dropped = TransferPanel::take_dropped(&ctx);
        if !dropped.is_empty() {
            self.transfers.open = true;
            if allowed {
                backend.send_files(self.id, dropped);
            }
        }
        let peer = (self.peer.name.clone(), self.peer.device_id.0);
        for action in self
            .transfers
            .ui(ui, tx, allowed, backend, (&peer.0, peer.1))
        {
            match action {
                transfers::Action::Send(paths) => backend.send_files(self.id, paths),
                transfers::Action::Queue(a) => apply_queue_action(backend, self.id, a),
            }
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

    /// Buttons "1", "2", ... for the host's monitors plus "All" (hidden with a single
    /// monitor). Returns what the user picked if it differs from the active view: `Some(index)`
    /// for one monitor, `None` inside for all of them. The active one is highlighted only once
    /// the host confirms the switch.
    fn monitor_picker(&self, ui: &mut egui::Ui, tx: Tx) -> Option<Option<u32>> {
        if self.monitors.len() < 2 {
            return None;
        }
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
                picked = Some(Some(m.index));
            }
        }
        if perf::all_monitors_button(ui, tx, self.active_monitor == perf::ALL_MONITORS) {
            picked = Some(None);
        }
        picked
    }

    /// Notice shown while the host user has input priority.
    fn priority_banner(&self, ui: &mut egui::Ui, tx: Tx) {
        let dark = ui.visuals().dark_mode;
        let (text, fill) = theme::Tone::Warning.colors(dark);

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
        // Compact translucent card: soft shadow, hairline edge, rounded like toasts.
        let radius = egui::CornerRadius::same(theme::radius::LG);
        painter.add(theme::float_shadow(true).as_shape(rect, radius));
        painter.rect_filled(
            rect,
            radius,
            Color32::from_rgba_unmultiplied(14, 17, 21, 200),
        );
        painter.rect_stroke(
            rect,
            radius,
            egui::Stroke::new(1.0, Color32::from_white_alpha(28)),
            egui::StrokeKind::Inside,
        );
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
            // "~" marks the coarse estimate used until the clocks are synchronized.
            let approx = if s.latency_synced { "" } else { "~" };
            lines.push((
                S::StatLatency,
                format!(
                    "{approx}{} {}",
                    format::number(f64::from(s.latency_ms), 0, d),
                    tx.raw(S::UnitMs)
                ),
            ));
            lines.push((S::StatBitrate, format::bitrate(s.bitrate_kbps, tx.lang, d)));
        }
        // "1280×800 · Q≈24": the size the host currently streams (the adaptive controller
        // lowers it before it lets the picture get blurry) and how finely it is quantised.
        let mut resolution = format!("{}×{}", self.frame_size[0], self.frame_size[1]);
        if let Some(qp) = self.stats.map(|s| s.qp).filter(|&q| q > 0) {
            resolution.push_str(&format!(" · Q≈{qp}"));
        }
        lines.push((S::StatResolution, tx.digits(&resolution)));
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
        self.typing.begin_frame();
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
                        if let Some(w) = self.wheel.event(unit, delta) {
                            self.send(backend, w);
                        }
                    }
                }
                egui::Event::Key {
                    key,
                    physical_key,
                    pressed,
                    modifiers,
                    ..
                } if keys_ok => {
                    if let Some(k) = self.typing.key(key, physical_key, modifiers, pressed) {
                        self.send_key(backend, k);
                    }
                }
                // Characters without a key of their own (IME, emoji) or, in "my layout" mode,
                // every typed character: Unicode text (see `text_route`).
                egui::Event::Text(ref text) if keys_ok => {
                    for t in self.typing.text(text) {
                        self.send(backend, t);
                    }
                }
                egui::Event::Ime(egui::ImeEvent::Commit(ref text)) if keys_ok => {
                    for t in self.typing.commit(text) {
                        self.send(backend, t);
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

/// Applies a request from a transfer queue (viewer panel or host window) to the backend.
pub fn apply_queue_action(
    backend: &dyn Backend,
    session: crate::backend::SessionId,
    action: super::transfer_queue::QueueAction,
) {
    use super::transfer_queue::{self as q, QueueAction as A};
    match action {
        A::Cancel(id) => backend.cancel_transfer(session, id),
        A::Pause(id) => backend.pause_transfer(session, id),
        A::Resume(id) => backend.retry_transfer(session, id),
        A::OpenFolder(dir) => transfers::open_folder(&dir),
        A::OpenFile(file) => q::open_file(&file),
        A::ShowInFolder(file) => q::show_in_folder(&file),
        A::ClearHistory => backend.clear_transfer_history(),
        A::Repeat(r) => {
            if r.sent {
                backend.send_files(session, r.local_paths.iter().map(Into::into).collect());
            } else if let Some(dest) = r.local_dest {
                backend.download_files(session, r.remote_paths, dest.into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{AttemptId, ConnectTarget, DeviceId, LocalInfo, RequestId, VideoImage};
    use crate::i18n::{DigitStyle, Lang};
    use crate::text_route::TextRoute;
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
                identity_conflict: None,
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

    fn key_downs(sent: &[InputEvent]) -> Vec<(u16, bool)> {
        sent.iter()
            .filter_map(|e| match e {
                InputEvent::Key {
                    scancode,
                    extended,
                    down: true,
                } => Some((*scancode, *extended)),
                _ => None,
            })
            .collect()
    }

    /// Alt+Shift and Win+Space (Arabic <-> English layout switching on the host) are plain
    /// scancodes through the window path, so they reach the host's keyboard-layout hotkeys.
    #[test]
    fn layout_switch_hotkeys_reach_the_host_as_scancodes() {
        let mut rig = Rig::new(None);
        rig.take();
        rig.frame(vec![
            egui_key(egui::Key::AltLeft, true),
            egui_key(egui::Key::ShiftLeft, true),
            egui_key(egui::Key::ShiftLeft, false),
            egui_key(egui::Key::AltLeft, false),
            egui_key(egui::Key::SuperLeft, true),
            egui_key(egui::Key::Space, true),
            egui_key(egui::Key::Space, false),
            egui_key(egui::Key::SuperLeft, false),
        ]);
        let sent = rig.take();
        assert_eq!(
            key_downs(&sent),
            vec![(0x38, false), (0x2A, false), (0x5B, true), (0x39, false)]
        );
        // The same holds in "my keyboard layout" mode: they are not turned into text.
        rig.session.typing.set_route(TextRoute::Layout);
        rig.frame(vec![
            egui_key(egui::Key::SuperLeft, true),
            egui_key(egui::Key::Space, true),
            egui_key(egui::Key::Space, false),
            egui_key(egui::Key::SuperLeft, false),
        ]);
        assert_eq!(key_downs(&rig.take()), vec![(0x5B, true), (0x39, false)]);
    }

    /// A typed key goes out once: as a scancode by default, as text in "my layout" mode.
    #[test]
    fn typed_character_is_never_sent_twice() {
        let mut rig = Rig::new(None);
        rig.take();
        rig.frame(vec![
            egui_key(egui::Key::A, true),
            egui::Event::Text("ش".into()),
            egui_key(egui::Key::A, false),
        ]);
        let sent = rig.take();
        assert!(
            sent.iter().all(|e| matches!(e, InputEvent::Key { .. })),
            "{sent:?}"
        );
        assert_eq!(key_downs(&sent), vec![(0x1E, false)]);

        rig.session.typing.set_route(TextRoute::Layout);
        rig.frame(vec![
            egui_key(egui::Key::A, true),
            egui::Event::Text("ش".into()),
            egui_key(egui::Key::A, false),
        ]);
        assert_eq!(rig.take(), vec![InputEvent::Text { text: "ش".into() }]);
    }

    /// IME commits and emoji have no key: they travel as text in both modes.
    #[test]
    fn ime_commit_and_emoji_are_sent_as_text() {
        let mut rig = Rig::new(None);
        rig.take();
        rig.frame(vec![
            egui::Event::Ime(egui::ImeEvent::Commit("مرحبا".into())),
            egui::Event::Text("😀".into()),
        ]);
        assert_eq!(
            rig.take(),
            vec![
                InputEvent::Text {
                    text: "مرحبا".into()
                },
                InputEvent::Text {
                    text: "😀".into()
                }
            ]
        );
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
                // The scan may hit the privacy toggle: dismiss its confirmation dialog.
                if session.privacy.confirming() {
                    session.privacy.confirm(false, Instant::now());
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

    /// The toolbar adapts to the window width: Disconnect and the More button are always fully
    /// inside the bar, nothing is clipped or overlapping, and wide windows show everything.
    #[test]
    fn toolbar_fits_every_width_in_both_languages() {
        use toolbar::Item;
        let essentials = [
            Item::Mode,
            Item::Fullscreen,
            Item::ViewOnly,
            Item::Stats,
            Item::Files,
            Item::Chat,
            Item::Monitors,
            Item::Quality,
        ];
        for width in [800.0_f32, 1120.0, 1920.0] {
            for (lang, name) in [
                (Lang::En, "Reception desk computer with a very long name"),
                (Lang::Ar, "حاسوب-المكتب-الرئيسي-في-الطابق-الثاني"),
            ] {
                for digits in [DigitStyle::Western, DigitStyle::ArabicIndic] {
                    let ctx = egui::Context::default();
                    icons::install(&ctx);
                    let backend = Recorder::default();
                    let peer = PeerInfo {
                        device_id: DeviceId(2),
                        name: name.into(),
                        address: "192.168.1.2".parse().unwrap(),
                    };
                    let mut session =
                        ViewerSession::new(SessionId(1), peer, Permissions::ALL, FrameSlot::new());
                    session.set_monitors(two_monitors(), 0);
                    session.grab_on = true;
                    session.audio.enabled = true;
                    session.recording = crate::ui::recordings::RecordState::On {
                        since: Instant::now(),
                    };
                    let tx = Tx::new(lang, digits);
                    let screen = Rect::from_min_size(Pos2::ZERO, vec2(width, 700.0));
                    for _ in 0..3 {
                        let input = egui::RawInput {
                            screen_rect: Some(screen),
                            ..Default::default()
                        };
                        let mut out = ctx.run_ui(input, |ui| session.ui(ui, tx, &backend, false));
                        out.textures_delta.clear();
                    }
                    let l = session.toolbar_layout.clone();
                    let tag = format!("{width} {lang:?} {digits:?}");
                    let bar = l.content.expand(0.5);
                    assert!(bar.contains_rect(l.disconnect), "{tag}: disconnect {l:?}");
                    assert!(l.disconnect.width() > 20.0, "{tag}");
                    let mut rects: Vec<Rect> = vec![l.disconnect];
                    for (item, r) in &l.inline {
                        assert!(
                            bar.contains_rect(*r),
                            "{tag}: {item:?} clipped {r:?} in {bar:?}"
                        );
                        rects.push(*r);
                    }
                    if let Some(more) = l.more {
                        assert!(bar.contains_rect(more), "{tag}: more clipped {more:?}");
                        assert!(!l.overflow.is_empty(), "{tag}");
                        rects.push(more);
                    } else {
                        assert!(l.overflow.is_empty(), "{tag}");
                    }
                    for (i, a) in rects.iter().enumerate() {
                        for b in &rects[i + 1..] {
                            assert!(!a.shrink(0.5).intersects(*b), "{tag}: overlap {a:?} {b:?}");
                        }
                    }
                    for item in essentials {
                        assert!(
                            l.inline.iter().any(|(i, _)| *i == item),
                            "{tag}: essential {item:?} missing"
                        );
                    }
                    // Disconnect is at the reading end: right in English, left in Arabic.
                    let mid = l.content.center().x;
                    assert_eq!(l.disconnect.center().x > mid, lang == Lang::En, "{tag}");
                    if width >= 1920.0 {
                        assert!(l.overflow.is_empty(), "{tag}: {:?}", l.overflow);
                    }
                    if width <= 800.0 {
                        assert!(!l.overflow.is_empty(), "{tag}");
                    }
                }
            }
        }
    }

    /// The More button opens and closes the menu holding the overflowed items, in both directions.
    #[test]
    fn more_menu_opens_and_closes() {
        for lang in [Lang::En, Lang::Ar] {
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
            let tx = Tx::new(lang, DigitStyle::Western);
            let screen = Rect::from_min_size(Pos2::ZERO, vec2(640.0, 600.0));
            let run = |session: &mut ViewerSession, events: Vec<egui::Event>| {
                let input = egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                };
                let mut out = ctx.run_ui(input, |ui| session.ui(ui, tx, &backend, false));
                out.textures_delta.clear();
            };
            let click = |session: &mut ViewerSession, p: Pos2| {
                run(session, vec![egui::Event::PointerMoved(p)]);
                for pressed in [true, false] {
                    run(
                        session,
                        vec![egui::Event::PointerButton {
                            pos: p,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: Default::default(),
                        }],
                    );
                }
            };
            run(&mut session, vec![]);
            run(&mut session, vec![]);
            let more = session.toolbar_layout.more.expect("overflow at 640 px");
            assert!(session
                .toolbar_layout
                .overflow
                .contains(&toolbar::Item::Clipboard));
            assert!(!egui::Popup::is_any_open(&ctx));
            click(&mut session, more.center());
            run(&mut session, vec![]);
            assert!(
                egui::Popup::is_any_open(&ctx),
                "{lang:?}: menu did not open"
            );
            // It stays open across frames, and a click on the button closes it again.
            run(&mut session, vec![]);
            assert!(
                egui::Popup::is_any_open(&ctx),
                "{lang:?}: menu closed by itself"
            );
            click(&mut session, more.center());
            run(&mut session, vec![]);
            assert!(
                !egui::Popup::is_any_open(&ctx),
                "{lang:?}: menu did not close"
            );
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

    /// The privacy banner, notice and confirmation dialog render in both languages, and no
    /// remote input is sent while the confirmation dialog is open.
    #[test]
    fn privacy_dialog_pauses_input_and_banners_render() {
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
                name: "حاسوب-المكتبة".into(),
                address: "192.168.1.2".parse().unwrap(),
            };
            let mut session = ViewerSession::new(SessionId(1), peer, Permissions::ALL, frames);
            let tx = Tx::new(lang, DigitStyle::Western);
            let screen = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 700.0));
            let run = |session: &mut ViewerSession, events: Vec<egui::Event>| {
                let input = egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                };
                let mut out = ctx.run_ui(input, |ui| session.ui(ui, tx, &backend, false));
                out.textures_delta.clear();
            };
            run(&mut session, vec![]);
            // Open the confirmation dialog: remote input must pause.
            assert_eq!(session.privacy.toggle(Instant::now()), None);
            assert!(session.privacy.confirming());
            let p = pos2(500.0, 400.0);
            run(
                &mut session,
                vec![
                    egui::Event::PointerMoved(p),
                    egui::Event::Key {
                        key: egui::Key::A,
                        physical_key: Some(egui::Key::A),
                        pressed: true,
                        repeat: false,
                        modifiers: Default::default(),
                    },
                ],
            );
            assert!(
                !backend
                    .0
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|e| !matches!(e, InputEvent::ReleaseAll)),
                "{lang:?}: input leaked while the dialog was open"
            );
            assert_eq!(session.privacy.confirm(true, Instant::now()), Some(true));
            // "On" banner, then the notice after the host ended it.
            session.privacy.on_state(true, Instant::now());
            run(&mut session, vec![]);
            session.privacy.on_state(false, Instant::now());
            assert!(session.privacy.notice_visible(Instant::now()));
            run(&mut session, vec![]);
        }
    }

    /// Characters of `display` sorted left to right as the real egui pipeline draws them.
    fn screen_order(display: &str) -> String {
        let mut fonts =
            egui::epaint::text::Fonts::new(Default::default(), theme::font_definitions());
        let galley = fonts.with_pixels_per_point(1.0).layout_no_wrap(
            display.to_owned(),
            egui::FontId::proportional(13.5),
            Color32::WHITE,
        );
        let mut glyphs: Vec<(f32, char)> = galley
            .rows
            .iter()
            .flat_map(|r| r.row.glyphs.iter())
            .filter(|g| g.advance_width > 0.0)
            .map(|g| (g.pos.x, g.chr))
            .collect();
        glyphs.sort_by(|a, b| a.0.total_cmp(&b.0));
        glyphs.into_iter().map(|(_, c)| c).collect()
    }

    /// On screen (glyph x positions, not the string) the resolution reads W then H in every
    /// language and digit style.
    #[test]
    fn overlay_resolution_is_left_to_right_on_screen() {
        let frames = FrameSlot::new();
        let peer = PeerInfo {
            device_id: DeviceId(2),
            name: "pc".into(),
            address: "192.168.1.2".parse().unwrap(),
        };
        let mut session = ViewerSession::new(SessionId(1), peer, Permissions::ALL, frames);
        session.frame_size = [1280, 720];
        session.set_fixed_elapsed(Some(std::time::Duration::from_secs(267)));
        for digits in [DigitStyle::Western, DigitStyle::ArabicIndic] {
            for lang in [Lang::Ar, Lang::En] {
                let tx = Tx::new(lang, digits);
                let lines = session.stat_lines(tx);
                let shown = screen_order(&lines[lines.len() - 2]);
                let want = if digits == DigitStyle::Western {
                    "1280\u{d7}720"
                } else {
                    "\u{661}\u{662}\u{668}\u{660}\u{d7}\u{667}\u{662}\u{660}"
                };
                assert!(shown.contains(want), "{lang:?} {digits:?}: {shown:?}");
                // The clock keeps "m:ss" and the monitor tooltip keeps "W×H" left to right too.
                let clock = screen_order(&lines[lines.len() - 1]);
                let want_clock = if digits == DigitStyle::Western {
                    "4:27"
                } else {
                    "\u{664}:\u{662}\u{667}"
                };
                assert!(clock.contains(want_clock), "{lang:?} {digits:?}: {clock:?}");
                let res = tx.digits("1920\u{d7}1080");
                let tip = screen_order(&tx.f(S::ScreenTooltip, &[("n", "1"), ("res", &res)]));
                let want_res = if digits == DigitStyle::Western {
                    "1920\u{d7}1080"
                } else {
                    "\u{661}\u{669}\u{662}\u{660}\u{d7}\u{661}\u{660}\u{668}\u{660}"
                };
                assert!(tip.contains(want_res), "{lang:?} {digits:?}: {tip:?}");
            }
        }
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
