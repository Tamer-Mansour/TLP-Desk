//! Playback window for a session recording (TLP-recording): its own OS window (an egui
//! immediate viewport; embedded as an in-app window where viewports are not available) with
//! the video, play/pause, a timeline with keyframe marks, speed 0.5x-4x and fit/actual size.
//!
//! Direction: the control row follows the reading direction (mirrored in Arabic), but the
//! **timeline and the time readout always run left to right**, as media timelines
//! conventionally do in RTL interfaces too: they picture the direction of time in the video,
//! not of text. Keys (native window only): Space plays/pauses, Left/Right jump 5 seconds.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{
    self, Color32, ColorImage, Pos2, Rect, RichText, Sense, TextureHandle, TextureOptions,
    ViewportBuilder, ViewportClass, ViewportId,
};
use tlp_desk_viewer::recording::{Player, PlayerStatus, MAX_SPEED, MIN_SPEED};
use tlp_desk_viewer::{PixelFormat, ViewerError};

use super::icons::{self, Icon};
use super::session::{fit_rect, ViewMode};
use super::theme;
use crate::i18n::{format, DigitStyle, Tx, S};

/// Speeds offered in the speed menu.
const SPEEDS: [f32; 6] = [0.5, 1.0, 1.25, 1.5, 2.0, 4.0];
/// Arrow-key jump.
const STEP: Duration = Duration::from_secs(5);

/// One open recording.
pub struct PlaybackWindow {
    id: ViewportId,
    path: PathBuf,
    /// File name as stored (logical order; shaped when shown).
    name: String,
    player: Player,
    texture: Option<TextureHandle>,
    size: [usize; 2],
    mode: ViewMode,
    open: bool,
}

/// `h:mm:ss` / `m:ss` with Western digits (localized by the caller).
fn clock(d: Duration) -> String {
    format::duration(d, DigitStyle::Western)
}

impl PlaybackWindow {
    /// Opens `path` for playback (paused on the first frame).
    pub fn open(ctx: &egui::Context, path: &Path) -> Result<Self, ViewerError> {
        // Immediate viewports are drawn during the main window's frame, so waking the main
        // window is what shows a new frame (embedded or native).
        let wake = ctx.clone();
        let repaint: tlp_desk_viewer::RepaintFn =
            Arc::new(move || wake.request_repaint_of(ViewportId::ROOT));
        let player = Player::open(path, PixelFormat::Rgba, Some(repaint))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(Self {
            id: ViewportId::from_hash_of(("tlp-playback", path)),
            path: path.to_owned(),
            name,
            player,
            texture: None,
            size: [0, 0],
            mode: ViewMode::Fit,
            open: true,
        })
    }

    /// The file being played.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Draws the window. Returns `false` once the user closed it.
    pub fn show(&mut self, ctx: &egui::Context, tx: Tx) -> bool {
        if !self.open {
            return false;
        }
        let title = tx.raw(S::PlayTitle).replace("{name}", &self.name);
        let builder = ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([960.0, 620.0])
            .with_min_inner_size([480.0, 320.0]);
        ctx.show_viewport_immediate(self.id, builder, |ui, class| {
            let embedded = class == ViewportClass::EmbeddedWindow;
            if embedded {
                ui.set_min_size(egui::vec2(640.0, 420.0));
            }
            if ui.input(|i| i.viewport().close_requested()) {
                self.open = false;
            }
            self.contents(ui, tx, !embedded);
        });
        self.open
    }

    fn update_texture(&mut self, ctx: &egui::Context) {
        let Some(frame) = self.player.take_frame() else {
            return;
        };
        let (w, h) = (frame.width as usize, frame.height as usize);
        if w == 0 || h == 0 || frame.pixels.len() != w * h * 4 {
            return;
        }
        // Decoded frames are opaque: premultiplied == unmultiplied.
        let image = ColorImage::from_rgba_premultiplied([w, h], &frame.pixels);
        self.player.frames().recycle(frame.pixels);
        self.size = [w, h];
        match &mut self.texture {
            Some(t) => t.set(image, TextureOptions::LINEAR),
            None => {
                self.texture =
                    Some(ctx.load_texture("tlp-playback-frame", image, TextureOptions::LINEAR))
            }
        }
    }

    fn contents(&mut self, ui: &mut egui::Ui, tx: Tx, keys: bool) {
        self.update_texture(ui.ctx());
        let status = self.player.status();
        if keys {
            self.keyboard(ui, &status);
        }
        egui::Panel::bottom(egui::Id::new(("tlp-playback-controls", self.id)))
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().window_fill)
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ui, |ui| self.controls(ui, tx, &status));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::from_rgb(0x0B, 0x0D, 0x10)))
            .show(ui, |ui| self.video(ui));
        if status.playing {
            // Keep the timeline moving between frames of a static recording.
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
    }

    fn keyboard(&self, ui: &egui::Ui, status: &PlayerStatus) {
        let (space, left, right) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
            )
        });
        if space {
            self.toggle(status);
        }
        if left {
            self.player.seek(status.position.saturating_sub(STEP));
        }
        if right {
            self.player
                .seek((status.position + STEP).min(status.duration));
        }
    }

    fn toggle(&self, status: &PlayerStatus) {
        if status.playing {
            self.player.pause();
        } else {
            self.player.play();
        }
    }

    fn video(&mut self, ui: &mut egui::Ui) {
        let Some(tex) = self.texture.clone() else {
            ui.centered_and_justified(|ui| ui.spinner());
            return;
        };
        let size = egui::vec2(self.size[0] as f32, self.size[1] as f32);
        let uv = Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0));
        match self.mode {
            ViewMode::Fit => {
                let avail = ui.available_rect_before_wrap();
                ui.allocate_rect(avail, Sense::hover());
                ui.painter()
                    .image(tex.id(), fit_rect(avail, size), uv, Color32::WHITE);
            }
            ViewMode::Original => {
                let size = size / ui.ctx().pixels_per_point();
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
                        ui.painter().image(tex.id(), rect, uv, Color32::WHITE);
                    });
            }
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui, tx: Tx, status: &PlayerStatus) {
        if let Some(t) = timeline(ui, status, self.player.keyframes()) {
            self.player.seek(t);
        }
        ui.add_space(4.0);
        theme::row(ui, tx.rtl(), |ui| {
            let (icon, label) = if status.playing {
                (Icon::Pause, S::RecPause)
            } else {
                (Icon::Play, S::RecPlay)
            };
            if icons::icon_button(ui, tx, icon, tx.t(label)).clicked() {
                self.toggle(status);
            }
            // Time readout: always "position / length", left to right.
            let time = format!("{} / {}", clock(status.position), clock(status.duration));
            ui.label(RichText::new(tx.n(&time)).monospace());
            if let Some(e) = &status.error {
                let dark = ui.visuals().dark_mode;
                icons::icon(
                    ui,
                    tx,
                    Icon::Warning,
                    icons::BUTTON_ICON,
                    theme::danger(dark),
                )
                .on_hover_text(tx.d(e));
            }
            theme::row_end(ui, tx.rtl(), |ui| {
                if ui
                    .add(icons::icon_text_button(tx, Icon::Close, tx.t(S::PlayClose)))
                    .clicked()
                {
                    self.open = false;
                }
                ui.separator();
                for (mode, icon, key) in [
                    (ViewMode::Original, Icon::ActualSize, S::OriginalSize),
                    (ViewMode::Fit, Icon::FitToWindow, S::FitToWindow),
                ] {
                    if icons::icon_toggle(ui, tx, self.mode == mode, icon, tx.t(key)).clicked() {
                        self.mode = mode;
                    }
                }
                ui.separator();
                let mut speed = status.speed;
                let label = |s: f32| tx.n(&format!("{s}×"));
                egui::ComboBox::from_id_salt(("tlp-playback-speed", self.id))
                    .selected_text(label(speed))
                    .width(64.0)
                    .show_ui(ui, |ui| {
                        for s in SPEEDS {
                            ui.selectable_value(&mut speed, s, label(s));
                        }
                    })
                    .response
                    .on_hover_text(tx.t(S::PlaySpeed));
                if (speed - status.speed).abs() > f32::EPSILON {
                    self.player.set_speed(speed.clamp(MIN_SPEED, MAX_SPEED));
                }
            });
        });
    }
}

/// Horizontal timeline, always left to right: progress, keyframe ticks and a knob. Returns the
/// time to seek to while it is clicked or dragged.
fn timeline(ui: &mut egui::Ui, status: &PlayerStatus, keyframes: &[Duration]) -> Option<Duration> {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 22.0),
        Sense::click_and_drag(),
    );
    let track = rect.shrink2(egui::vec2(8.0, 8.0));
    let total = status.duration.as_secs_f32().max(1e-3);
    let x_of =
        |d: Duration| track.min.x + track.width() * (d.as_secs_f32() / total).clamp(0.0, 1.0);
    let dark = ui.visuals().dark_mode;
    let painter = ui.painter_at(rect);
    painter.rect_filled(track, 3.0, ui.visuals().extreme_bg_color);
    let x = x_of(status.position);
    painter.rect_filled(
        Rect::from_min_max(track.min, egui::pos2(x, track.max.y)),
        3.0,
        theme::accent(dark),
    );
    let tick = ui.visuals().weak_text_color();
    for k in keyframes {
        let kx = x_of(*k);
        painter.line_segment(
            [
                egui::pos2(kx, track.max.y + 1.0),
                egui::pos2(kx, track.max.y + 4.0),
            ],
            egui::Stroke::new(1.0, tick),
        );
    }
    let knob = if resp.hovered() || resp.dragged() {
        7.0
    } else {
        5.5
    };
    painter.circle_filled(egui::pos2(x, track.center().y), knob, theme::accent(dark));

    if resp.clicked() || resp.dragged() {
        let p = resp.interact_pointer_pos()?;
        let frac = ((p.x - track.min.x) / track.width().max(1.0)).clamp(0.0, 1.0);
        return Some(status.duration.mul_f32(frac));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records `n` frames of real H.264 into `dir` through the public recording API.
    fn make_recording(dir: &Path, n: u64) -> PathBuf {
        use tlp_desk_viewer::recording::Recording;
        use tlp_desk_viewer::RecordingConfig;
        let cfg = RecordingConfig::new(dir.to_owned(), "مكتب");
        let (rec, tap) = Recording::start_with(cfg, Arc::new(|| {})).unwrap();
        let mut enc =
            tlp_desk_codec::new_encoder(tlp_desk_codec::EncoderConfig::for_screen(160, 120, 30))
                .unwrap();
        for seq in 0..n {
            let mut f = tlp_desk_capture::BgraFrame::new_tight(160, 120);
            f.data.iter_mut().for_each(|b| *b = (seq * 9) as u8);
            f.timestamp_us = seq * 33_400;
            let ef = enc.encode(&f).unwrap().unwrap();
            tap(&tlp_desk_proto::VideoFrame {
                seq,
                timestamp_us: seq * 33_400,
                width: 160,
                height: 120,
                codec: tlp_desk_proto::Codec::H264,
                keyframe: ef.keyframe,
                data: ef.data,
            });
        }
        let summary = rec.stop();
        summary.files[0].clone()
    }

    #[test]
    fn window_plays_a_recording_headless() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_recording(dir.path(), 20);
        for lang in [crate::i18n::Lang::En, crate::i18n::Lang::Ar] {
            let ctx = egui::Context::default();
            super::super::icons::install(&ctx);
            let tx = Tx::new(lang, DigitStyle::ArabicIndic);
            let mut w = PlaybackWindow::open(&ctx, &path).unwrap();
            assert_eq!(w.path(), path);
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            loop {
                let input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1000.0, 700.0))),
                    ..Default::default()
                };
                let mut out = ctx.run_ui(input, |ui| {
                    assert!(w.show(ui.ctx(), tx), "still open");
                });
                out.textures_delta.clear();
                if w.size == [160, 120] {
                    break;
                }
                assert!(std::time::Instant::now() < deadline, "no frame shown");
                std::thread::sleep(Duration::from_millis(10));
            }
            w.player.set_speed(4.0);
            w.player.play();
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            while !w.player.status().ended {
                assert!(std::time::Instant::now() < deadline, "playback never ended");
                std::thread::sleep(Duration::from_millis(10));
            }
            w.open = false;
            assert!(!w.show(&ctx, tx));
        }
    }

    #[test]
    fn clock_formats_like_the_session_timer() {
        assert_eq!(clock(Duration::from_secs(75)), "1:15");
        assert_eq!(clock(Duration::from_secs(3725)), "1:02:05");
    }

    #[test]
    fn timeline_is_left_to_right_and_seeks_where_clicked() {
        for lang in [crate::i18n::Lang::En, crate::i18n::Lang::Ar] {
            let ctx = egui::Context::default();
            crate::ui::theme::apply_lang(&ctx, lang);
            let status = PlayerStatus {
                duration: Duration::from_secs(100),
                position: Duration::from_secs(10),
                ..PlayerStatus::default()
            };
            let screen = Rect::from_min_size(Pos2::ZERO, egui::vec2(416.0, 100.0));
            let mut sought = None;
            for events in [
                vec![],
                vec![
                    egui::Event::PointerMoved(egui::pos2(108.0, 19.0)),
                    egui::Event::PointerButton {
                        pos: egui::pos2(108.0, 19.0),
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    },
                    egui::Event::PointerButton {
                        pos: egui::pos2(108.0, 19.0),
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: Default::default(),
                    },
                ],
            ] {
                let input = egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                };
                let mut out = ctx.run_ui(input, |ui| {
                    if let Some(t) = timeline(ui, &status, &[Duration::ZERO]) {
                        sought = Some(t);
                    }
                });
                out.textures_delta.clear(); // headless: no renderer consumes the uploads
            }
            // The track starts 8 pt in from the left edge of the panel's content area: a click
            // a quarter of the way along seeks to about 25 s, in both languages.
            let t = sought.expect("click seeks").as_secs_f32();
            assert!((20.0..30.0).contains(&t), "{lang:?}: {t}");
        }
    }
}
