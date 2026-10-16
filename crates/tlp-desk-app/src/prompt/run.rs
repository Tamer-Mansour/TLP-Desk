//! The popup process: `TLP-Desk.exe --accept-prompt` reads one [`PromptRequest`] line from
//! stdin, shows the Accept card in a borderless, always-on-top window at the bottom-right of
//! the primary screen's work area, and prints one [`PromptDecision`] line on stdout.

use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use eframe::egui;

use super::platform;
use super::protocol::{
    decode, encode, read_line, Outcome, PromptDecision, PromptRequest, PromptTheme, CANCEL_LINE,
};
use crate::i18n::{tr_raw, Tx, S};
use crate::ui::accept_window::AcceptCard;
use crate::ui::theme;

/// The command-line switch that turns `TLP-Desk.exe` into the popup.
pub const ACCEPT_PROMPT_ARG: &str = "--accept-prompt";
/// Window width, points. The height follows the content.
const WIDTH: f32 = 404.0;
/// First guess at the height before the content was measured.
const START_HEIGHT: f32 = 470.0;
/// Distance from the work area's edges.
const MARGIN: f32 = 16.0;

/// Called first thing in `main`: runs the popup and returns its exit code when the first
/// argument is [`ACCEPT_PROMPT_ARG`], `None` otherwise.
pub fn dispatch() -> Option<i32> {
    if std::env::args().nth(1).as_deref() != Some(ACCEPT_PROMPT_ARG) {
        return None;
    }
    Some(run_stdio())
}

/// Exit codes: 0 answered (any outcome), 2 no/invalid request, 3 the window failed.
fn run_stdio() -> i32 {
    let request = {
        let mut input = std::io::stdin().lock();
        match read_line(&mut input).and_then(|l| match l {
            Some(line) => decode::<PromptRequest>(&line),
            None => Err(super::protocol::PromptError::Missing),
        }) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("accept prompt: {e}");
                return 2;
            }
        }
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let ctx_slot: Arc<OnceLock<egui::Context>> = Arc::new(OnceLock::new());
    {
        let cancel = Arc::clone(&cancel);
        let ctx_slot = Arc::clone(&ctx_slot);
        // Later stdin lines: `cancel`, or end of input when the parent went away.
        std::thread::spawn(move || {
            let mut input = std::io::stdin().lock();
            loop {
                match read_line(&mut input) {
                    Ok(Some(line)) if line.trim() != CANCEL_LINE => continue,
                    _ => break,
                }
            }
            cancel.store(true, Ordering::SeqCst);
            if let Some(ctx) = ctx_slot.get() {
                ctx.request_repaint();
            }
        });
    }
    let sink = Box::new(|d: &PromptDecision| {
        if let Ok(line) = encode(d) {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    });
    match run_window(request, cancel, ctx_slot, sink) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("accept prompt window failed: {e}");
            3
        }
    }
}

/// Receives the single decision (stdout line, or a pipe command in service mode).
pub type DecisionSink = Box<dyn FnMut(&PromptDecision) + Send>;

/// Show the popup until the user answers, it times out, or `cancel` is set. `sink` gets
/// exactly one decision (closing the window any other way counts as Decline).
pub fn run_window(
    request: PromptRequest,
    cancel: Arc<AtomicBool>,
    ctx_slot: Arc<OnceLock<egui::Context>>,
    sink: DecisionSink,
) -> Result<(), eframe::Error> {
    if let Err(e) = tlp_desk_input::ensure_dpi_aware() {
        eprintln!("could not enable per-monitor DPI awareness: {e}");
    }
    let card = AcceptCard::new(request);
    let lang = card.request.lang;
    let title = format!(
        "{} - {}",
        tr_raw(lang, S::PopupTitle),
        tr_raw(lang, S::AppTitle)
    );
    let area = platform::work_area_points();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(title)
        .with_inner_size([WIDTH, START_HEIGHT])
        .with_decorations(false)
        .with_resizable(false)
        .with_always_on_top()
        .with_active(true)
        .with_icon(window_icon());
    if let Some(area) = area {
        viewport = viewport.with_position(corner_position(area, START_HEIGHT));
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Wgpu,
        centered: area.is_none(),
        ..Default::default()
    };
    let sink = Arc::new(Mutex::new(Some(sink)));
    let app_sink = Arc::clone(&sink);
    let result = eframe::run_native(
        "TLP-Desk accept prompt",
        options,
        Box::new(move |cc| {
            let _ = ctx_slot.set(cc.egui_ctx.clone());
            theme::install(&cc.egui_ctx);
            theme::apply_lang(&cc.egui_ctx, lang);
            cc.egui_ctx.set_theme(match card.request.theme {
                PromptTheme::System => egui::ThemePreference::System,
                PromptTheme::Dark => egui::ThemePreference::Dark,
                PromptTheme::Light => egui::ThemePreference::Light,
            });
            Ok(Box::new(PromptApp {
                tx: Tx::new(lang, card.request.digits),
                card,
                cancel,
                sink: app_sink,
                hwnd: crate::keygrab::window_handle(cc),
                frames: 0,
                height: START_HEIGHT,
                area,
            }))
        }),
    );
    // Closed without an answer (Alt+F4, or the window failed): that is a Decline.
    finish(
        &sink,
        &PromptDecision {
            outcome: Outcome::Decline,
            permissions: Default::default(),
            trust: false,
        },
    );
    result
}

/// Hand `decision` to the sink unless one was already given.
fn finish(sink: &Mutex<Option<DecisionSink>>, decision: &PromptDecision) {
    let taken = sink.lock().map(|mut s| s.take()).unwrap_or(None);
    if let Some(mut f) = taken {
        f(decision);
    }
}

/// Top-left corner that puts a `height`-tall window at the bottom-right of `area`.
fn corner_position(area: [f32; 4], height: f32) -> [f32; 2] {
    let [left, top, right, bottom] = area;
    [
        (right - WIDTH - MARGIN).max(left),
        (bottom - height - MARGIN).max(top),
    ]
}

fn window_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../../assets/icon/TLP-Desk-256.png"))
        .unwrap_or_else(|_| theme::app_icon())
}

struct PromptApp {
    tx: Tx,
    card: AcceptCard,
    cancel: Arc<AtomicBool>,
    sink: Arc<Mutex<Option<DecisionSink>>>,
    hwnd: Option<isize>,
    frames: u32,
    height: f32,
    area: Option<[f32; 4]>,
}

impl PromptApp {
    fn close(&self, ctx: &egui::Context, outcome: Outcome) {
        finish(&self.sink, &self.card.decision(outcome));
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl eframe::App for PromptApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.frames == 0 {
            if let Some(h) = self.hwnd {
                platform::round_corners(h);
            }
            if self.card.request.sound {
                platform::notification_sound();
            }
        }
        // Once the window is really on screen, ask for the foreground (again a bit later in
        // case the first attempt raced the window's creation).
        if matches!(self.frames, 1 | 12) {
            if let Some(h) = self.hwnd {
                platform::bring_to_front(h);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        self.frames = self.frames.saturating_add(1);
        if self.cancel.load(Ordering::SeqCst) {
            self.close(&ctx, Outcome::Cancelled);
            return;
        }
        let dark = ui.visuals().dark_mode;
        let border = if dark {
            egui::Color32::from_gray(0x3A)
        } else {
            egui::Color32::from_gray(0xD6)
        };
        let mut answer = None;
        let used = egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().window_fill)
                    .stroke(egui::Stroke::new(1.0, border)),
            )
            .show(ui, |ui| {
                answer = self.card.show(ui, self.tx, 0);
                ui.min_rect().height()
            })
            .inner;
        if self.card.header_drag {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        // Fit the window to the card and keep it in the corner.
        let want = used.ceil() + 2.0;
        if want > 100.0 && (want - self.height).abs() > 1.0 && self.frames > 2 {
            self.height = want;
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(WIDTH, want)));
            if let Some(area) = self.area {
                let [x, y] = corner_position(area, want);
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
            }
        }
        if let Some(outcome) = answer {
            self.close(&ctx, outcome);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corner_position_hugs_the_bottom_right_and_stays_inside() {
        let p = corner_position([0.0, 0.0, 1920.0, 1040.0], 470.0);
        assert_eq!(p, [1920.0 - WIDTH - MARGIN, 1040.0 - 470.0 - MARGIN]);
        // A tiny screen never pushes the window above or left of the work area.
        let p = corner_position([100.0, 50.0, 300.0, 200.0], 470.0);
        assert_eq!(p, [100.0, 50.0]);
    }

    #[test]
    fn finish_delivers_exactly_once() {
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = Arc::clone(&got);
        let sink: Mutex<Option<DecisionSink>> = Mutex::new(Some(Box::new(move |d| {
            g.lock().unwrap().push(d.outcome);
        })));
        let d = |outcome| PromptDecision {
            outcome,
            permissions: Default::default(),
            trust: false,
        };
        finish(&sink, &d(Outcome::Accept));
        finish(&sink, &d(Outcome::Decline));
        assert_eq!(*got.lock().unwrap(), vec![Outcome::Accept]);
    }
}
