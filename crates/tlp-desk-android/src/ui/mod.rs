//! The egui screens: connect form and the touch session view.

pub mod theme;

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Color32, ColorImage, Layout, Rect, RichText, Sense, TextureHandle, TextureOptions,
};
use tlp_desk_input::ViewRect;
use tlp_desk_proto::{ClipboardData, Message, Permissions};
use tlp_desk_viewer::ViewerSession;

use crate::gesture::mapper::InputMapper;
use crate::gesture::{GestureConfig, Phase, Recognizer, Touch};
use crate::i18n::{Lang, S};
use crate::keyboard::{self, SpecialKey, TextStep};
use crate::session::{ConnState, Controller, CONNECT_REPAINT};
use crate::view::Point;

/// The Android viewer application state.
pub struct ViewerApp {
    lang: Lang,
    controller: Arc<Controller>,
    address: String,
    secret: String,
    mapper: InputMapper,
    recognizer: Recognizer,
    texture: Option<TextureHandle>,
    keyboard_open: bool,
    keyboard_buffer: String,
    started: Instant,
    fingers_down: usize,
}

fn point(p: egui::Pos2) -> Point {
    Point::new(p.x, p.y)
}

impl ViewerApp {
    /// Creates the app. `data_dir` is where the identity and known hosts live.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        data_dir: std::path::PathBuf,
    ) -> Result<Self, crate::session::ControllerError> {
        theme::install(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
        let repaint: tlp_desk_viewer::RepaintFn = Arc::new(move || ctx.request_repaint());
        let controller = Arc::new(Controller::new(data_dir, repaint)?);
        Ok(Self {
            lang: Lang::detect(),
            controller,
            address: String::new(),
            secret: String::new(),
            mapper: InputMapper::new(ViewRect::new(0.0, 0.0, 1.0, 1.0)),
            recognizer: Recognizer::new(GestureConfig::default()),
            texture: None,
            keyboard_open: false,
            keyboard_buffer: String::new(),
            started: Instant::now(),
            fingers_down: 0,
        })
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

impl eframe::App for ViewerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let layout = if self.lang.rtl() {
            Layout::top_down(Align::Max)
        } else {
            Layout::top_down(Align::Min)
        };
        let state = self.controller.state();
        match state {
            ConnState::Connected => self.session_ui(ui),
            other => {
                ui.with_layout(layout, |ui| self.connect_ui(ui, other));
            }
        }
    }
}

impl ViewerApp {
    fn connect_ui(&mut self, ui: &mut egui::Ui, state: ConnState) {
        let lang = self.lang;
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(24.0);
            ui.horizontal(|ui| {
                ui.heading(lang.t(S::ConnectTitle));
                if ui.button(lang.other().t(S::Language)).clicked() {
                    self.lang = lang.other();
                }
            });
            ui.label(RichText::new(lang.t(S::LanOnly)).weak());
            ui.add_space(12.0);

            ui.label(lang.t(S::AddressLabel));
            ui.add(
                egui::TextEdit::singleline(&mut self.address)
                    .hint_text(lang.t(S::AddressHint))
                    .desired_width(f32::INFINITY),
            );
            ui.label(lang.t(S::CodeLabel));
            ui.add(
                egui::TextEdit::singleline(&mut self.secret)
                    .hint_text(lang.t(S::CodeHint))
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(8.0);

            let busy = matches!(state, ConnState::Connecting { .. });
            ui.add_enabled_ui(!busy && !self.address.trim().is_empty(), |ui| {
                if ui
                    .add_sized(
                        [ui.available_width(), 52.0],
                        egui::Button::new(lang.t(S::Connect)),
                    )
                    .clicked()
                {
                    self.controller.connect(&self.address, &self.secret);
                }
            });

            match state {
                ConnState::Connecting { awaiting_accept } => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        let text = if awaiting_accept {
                            S::WaitingAccept
                        } else {
                            S::Connecting
                        };
                        ui.label(lang.t(text));
                    });
                    ui.ctx().request_repaint_after(CONNECT_REPAINT);
                }
                ConnState::Failed {
                    message,
                    changed_key,
                } => {
                    let shown = lang.f(S::ConnectFailed, &message);
                    ui.colored_label(ui.visuals().error_fg_color, shown);
                    if let Some(key) = changed_key {
                        if ui.button("Trust the new key").clicked() {
                            self.controller.forget_host_key(&key);
                        }
                    }
                    if ui.button("OK").clicked() {
                        self.controller.acknowledge();
                    }
                }
                _ => {}
            }
        });
    }

    fn session_ui(&mut self, ui: &mut egui::Ui) {
        let Some(session) = self.controller.session() else {
            return;
        };
        let lang = self.lang;
        let can_control = session.permissions().contains(Permissions::CONTROL);
        egui::Panel::top("session-bar").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button(lang.t(S::Disconnect)).clicked() {
                    self.controller.disconnect();
                }
                if can_control && ui.button(lang.t(S::Keyboard)).clicked() {
                    self.keyboard_open = !self.keyboard_open;
                }
                if ui.button(lang.t(S::ResetZoom)).clicked() {
                    self.mapper.view = Default::default();
                }
            });
            if !can_control {
                ui.label(RichText::new(lang.t(S::ViewOnly)).weak());
            }
            if self.keyboard_open {
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.keyboard_buffer)
                        .desired_width(f32::INFINITY),
                );
                edit.request_focus();
                ui.label(RichText::new(lang.t(S::KeyboardNote)).weak().small());
            }
        });

        self.update_texture(ui.ctx(), &session);

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::BLACK))
            .show(ui, |ui| {
                let rect = ui.available_rect_before_wrap();
                let (_, _) = ui.allocate_exact_size(rect.size(), Sense::hover());
                let image = self
                    .texture
                    .as_ref()
                    .map(|t| (t.size()[0] as u32, t.size()[1] as u32))
                    .unwrap_or((0, 0));
                self.mapper.set_geometry(
                    ViewRect::new(rect.min.x, rect.min.y, rect.width(), rect.height()),
                    image,
                );
                match &self.texture {
                    Some(tex) => {
                        let r = self.mapper.view.image_rect(self.mapper.viewport, image);
                        let target = Rect::from_min_size(
                            egui::pos2(r.x, r.y),
                            egui::vec2(r.width, r.height),
                        );
                        ui.painter().with_clip_rect(rect).image(
                            tex.id(),
                            target,
                            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    None => {
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            lang.t(S::WaitingFrame),
                            egui::FontId::proportional(18.0),
                            Color32::GRAY,
                        );
                    }
                }
                self.handle_input(ui, rect, &session, can_control);
            });
    }

    fn update_texture(&mut self, ctx: &egui::Context, session: &ViewerSession) {
        let Some(frame) = session.take_frame() else {
            return;
        };
        let (w, h) = (frame.width as usize, frame.height as usize);
        if w == 0 || h == 0 || frame.pixels.len() != w * h * 4 {
            return;
        }
        let image = ColorImage::from_rgba_premultiplied([w, h], &frame.pixels);
        session.frames().recycle(frame.pixels);
        match &mut self.texture {
            Some(t) => t.set(image, TextureOptions::LINEAR),
            None => {
                self.texture = Some(ctx.load_texture("tlp-remote", image, TextureOptions::LINEAR));
            }
        }
    }

    fn handle_input(
        &mut self,
        ui: &mut egui::Ui,
        area: Rect,
        session: &ViewerSession,
        can_control: bool,
    ) {
        let events = ui.input(|i| i.events.clone());
        let mut actions = Vec::new();
        let mut typed = String::new();
        let mut keys = Vec::new();
        for ev in &events {
            match ev {
                egui::Event::Touch { id, phase, pos, .. } => {
                    // Fingers on the toolbar or keyboard field are not for the remote screen.
                    if !area.contains(*pos) && *phase == egui::TouchPhase::Start {
                        continue;
                    }
                    let phase = match phase {
                        egui::TouchPhase::Start => {
                            self.fingers_down += 1;
                            Phase::Start
                        }
                        egui::TouchPhase::Move => Phase::Move,
                        egui::TouchPhase::End => {
                            self.fingers_down = self.fingers_down.saturating_sub(1);
                            Phase::End
                        }
                        egui::TouchPhase::Cancel => {
                            self.fingers_down = self.fingers_down.saturating_sub(1);
                            Phase::Cancel
                        }
                    };
                    self.recognizer.touch(
                        Touch {
                            id: id.0,
                            phase,
                            pos: point(*pos),
                            t_ms: self.now_ms(),
                        },
                        &mut actions,
                    );
                }
                egui::Event::Text(s) if self.keyboard_open => typed.push_str(s),
                egui::Event::Ime(egui::ImeEvent::Commit(s)) if self.keyboard_open => {
                    if !typed.ends_with(s.as_str()) {
                        typed.push_str(s);
                    }
                }
                egui::Event::Key {
                    key, pressed: true, ..
                } if self.keyboard_open => {
                    let special = match key {
                        egui::Key::Backspace => Some(SpecialKey::Backspace),
                        egui::Key::Enter => Some(SpecialKey::Enter),
                        egui::Key::Tab => Some(SpecialKey::Tab),
                        egui::Key::Escape => Some(SpecialKey::Escape),
                        egui::Key::Delete => Some(SpecialKey::Delete),
                        egui::Key::ArrowLeft => Some(SpecialKey::Left),
                        egui::Key::ArrowRight => Some(SpecialKey::Right),
                        egui::Key::ArrowUp => Some(SpecialKey::Up),
                        egui::Key::ArrowDown => Some(SpecialKey::Down),
                        egui::Key::Home => Some(SpecialKey::Home),
                        egui::Key::End => Some(SpecialKey::End),
                        _ => None,
                    };
                    if let Some(k) = special {
                        keys.push(k);
                    }
                }
                _ => {}
            }
        }
        let now = self.now_ms();
        self.recognizer.tick(now, &mut actions);
        if self.fingers_down == 0 && !self.recognizer.is_active() {
            self.mapper.gesture_finished();
        } else {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }

        for action in actions {
            let input = self.mapper.apply(action);
            if can_control {
                for e in input {
                    session.send_input(e);
                }
            }
        }

        if can_control {
            for k in keys {
                for e in keyboard::special_key(k) {
                    session.send_input(e);
                }
            }
            send_text(session, &typed);
        }
        self.keyboard_buffer.clear();
    }
}

/// Types `text` on the host: US-layout keys where possible, clipboard + Ctrl+V for the rest.
fn send_text(session: &ViewerSession, text: &str) {
    for step in keyboard::plan_text(text) {
        match step {
            TextStep::Keys(events) => {
                for e in events {
                    session.send_input(e);
                }
            }
            TextStep::Paste(s) => {
                session.send_message(Message::ClipboardData(ClipboardData::Text(s)));
                for e in keyboard::paste_keys() {
                    session.send_input(e);
                }
            }
        }
    }
}
