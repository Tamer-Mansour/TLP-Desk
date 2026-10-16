//! The Accept card: one incoming request, drawn the same way in the stand-alone popup window
//! (`TLP-Desk.exe --accept-prompt`, see [`crate::prompt`]) and in the main window's fallback
//! dialog.
//!
//! Layout, top to bottom: an accent header strip, the device (avatar with initials inside a
//! countdown ring, name, ID, address, how it authenticated), permission chips the user can
//! switch off (or on) before accepting, "Trust this device", and the Decline / Accept buttons.
//!
//! Safety details: the buttons only arm [`ARM_DELAY`] after the card appears, so a click or a
//! key press meant for the window underneath cannot accept by accident, and Enter is never
//! bound to Accept (Escape declines).

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Layout, Margin, RichText, Sense, Stroke,
    Ui, Vec2,
};
use tlp_desk_proto::Permissions;

use crate::i18n::{Tx, S};
use crate::prompt::protocol::{Outcome, PromptAuth, PromptDecision, PromptRequest};
use crate::settings::host::PermissionSet;
use crate::ui::icons::Icon;
use crate::ui::settings::incoming::PERMISSION_LABELS;
use crate::ui::theme;

/// Seconds after the card appears before Accept / Decline react to clicks.
pub const ARM_DELAY: f64 = 0.6;
/// Length of the entrance animation, seconds.
const ENTRANCE: f32 = 0.28;
/// Permissions always offered as chips (others appear when the defaults include them).
const CORE_CHIPS: Permissions = Permissions::VIEW
    .union(Permissions::CONTROL)
    .union(Permissions::KEYBOARD)
    .union(Permissions::CLIPBOARD)
    .union(Permissions::FILES)
    .union(Permissions::AUDIO)
    .union(Permissions::CHAT);
/// Avatar colours (picked by device ID), readable with white initials in both themes.
const AVATAR_COLORS: [Color32; 8] = [
    Color32::from_rgb(0x2E, 0x7D, 0x9A),
    Color32::from_rgb(0x6A, 0x4C, 0x93),
    Color32::from_rgb(0xB2, 0x5E, 0x2E),
    Color32::from_rgb(0x2F, 0x8F, 0x5B),
    Color32::from_rgb(0xA8, 0x3F, 0x6B),
    Color32::from_rgb(0x3F, 0x5E, 0xB0),
    Color32::from_rgb(0x8A, 0x6D, 0x1F),
    Color32::from_rgb(0x1F, 0x86, 0x84),
];

/// State of one Accept card.
#[derive(Debug, Clone)]
pub struct AcceptCard {
    /// The (sanitized) request being shown.
    pub request: PromptRequest,
    /// Permissions currently switched on.
    pub permissions: Permissions,
    /// "Trust this device" checkbox.
    pub trust: bool,
    /// The header strip was dragged this frame (the popup moves its window).
    pub header_drag: bool,
    /// egui time of the first frame, for the countdown and arming.
    shown_at: Option<f64>,
}

impl AcceptCard {
    /// A card for `request` (sanitized here), with the request's permissions switched on.
    pub fn new(request: PromptRequest) -> Self {
        let request = request.sanitized();
        Self {
            permissions: request.permissions.0,
            request,
            trust: false,
            header_drag: false,
            shown_at: None,
        }
    }

    /// Pretend the card appeared `secs` ago (screenshots and tests).
    pub fn set_elapsed(&mut self, now: f64, secs: f64) {
        self.shown_at = Some(now - secs);
    }

    fn elapsed(&self, now: f64) -> f64 {
        self.shown_at.map_or(0.0, |t| (now - t).max(0.0))
    }

    /// Seconds left before the request declines itself.
    pub fn remaining(&self, now: f64) -> f64 {
        (f64::from(self.request.timeout_secs) - self.elapsed(now)).max(0.0)
    }

    /// The answer to report for `outcome` with the current choices.
    pub fn decision(&self, outcome: Outcome) -> PromptDecision {
        let permissions = if self.request.permissions_locked {
            self.request.permissions.0
        } else {
            self.permissions
        };
        PromptDecision {
            outcome,
            permissions: PermissionSet(permissions.union(Permissions::VIEW)),
            trust: self.trust && outcome == Outcome::Accept,
        }
    }

    /// Draw the card filling `ui`'s width. `corner` rounds the header strip's top corners to
    /// match the surrounding frame. Returns the user's answer (or `Timeout`) once.
    pub fn show(&mut self, ui: &mut Ui, tx: Tx, corner: u8) -> Option<Outcome> {
        let now = ui.input(|i| i.time);
        if self.shown_at.is_none() {
            self.shown_at = Some(now);
        }
        let remaining = self.remaining(now);
        if remaining <= 0.0 {
            return Some(Outcome::Timeout);
        }
        // Smooth countdown ring and the entrance animation.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(50));
        let t = (self.elapsed(now) as f32 / ENTRANCE).clamp(0.0, 1.0);
        let ease = 1.0 - (1.0 - t).powi(3);
        ui.multiply_opacity(ease);
        ui.add_space((1.0 - ease) * 14.0);

        let dark = ui.visuals().dark_mode;
        let armed = self.elapsed(now) >= ARM_DELAY;
        let mut answer = None;
        self.header_drag = false;

        // Header strip.
        let header = egui::Frame::new()
            .fill(theme::accent(dark))
            .corner_radius(CornerRadius {
                nw: corner,
                ne: corner,
                sw: 0,
                se: 0,
            })
            .inner_margin(Margin::symmetric(16, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                theme::row(ui, tx.rtl(), |ui| {
                    ui.add(Icon::ShieldCheck.image(18.0, Color32::WHITE));
                    ui.label(
                        RichText::new(tx.t(S::PopupTitle))
                            .color(Color32::WHITE)
                            .strong(),
                    );
                    theme::row_end(ui, tx.rtl(), |ui| {
                        ui.label(
                            RichText::new(tx.t(S::AppTitle))
                                .small()
                                .color(Color32::from_white_alpha(200)),
                        );
                    });
                });
            });
        let drag = ui.interact(
            header.response.rect,
            ui.id().with("accept-card-header"),
            Sense::drag(),
        );
        self.header_drag = drag.drag_started();

        egui::Frame::new()
            .inner_margin(Margin {
                left: 20,
                right: 20,
                top: 16,
                bottom: 18,
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                theme::column(ui, tx.rtl(), |ui| {
                    self.identity(ui, tx, remaining);
                    ui.add_space(14.0);
                    ui.label(RichText::new(tx.t(S::PopupAllow)).strong());
                    ui.add_space(2.0);
                    self.chips(ui, tx);
                    ui.add_space(8.0);
                    let trust = theme::row(ui, tx.rtl(), |ui| {
                        let mut r = ui.checkbox(&mut self.trust, "");
                        let label =
                            ui.add(egui::Label::new(tx.t(S::PopupTrust)).sense(Sense::click()));
                        if label.clicked() {
                            self.trust = !self.trust;
                            r.mark_changed();
                        }
                        r.union(label)
                    });
                    trust.on_hover_text(tx.t(S::PopupTrustHint));
                    ui.add_space(4.0);
                    let secs = format!("{}", remaining.ceil() as u64);
                    ui.label(theme::caption(
                        ui,
                        &tx.f(S::PopupCountdown, &[("n", &tx.digits(&secs))]),
                    ));
                    ui.add_space(12.0);
                    answer = self.buttons(ui, tx, armed);
                });
            });
        if armed && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            answer = Some(Outcome::Decline);
        }
        answer
    }

    /// Avatar with countdown ring, name, ID, address and the auth chip.
    fn identity(&self, ui: &mut Ui, tx: Tx, remaining: f64) {
        let dark = ui.visuals().dark_mode;
        let frac = (remaining / f64::from(self.request.timeout_secs.max(1))) as f32;
        let layout = if tx.rtl() {
            Layout::right_to_left(Align::Min)
        } else {
            Layout::left_to_right(Align::Min)
        };
        ui.allocate_ui_with_layout(Vec2::new(ui.available_width(), 84.0), layout, |ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(84.0), Sense::hover());
            let c = rect.center();
            let painter = ui.painter();
            // Countdown ring: a faint full circle, the remaining part in the accent colour
            // (red in the last five seconds).
            let ring_color = if remaining <= 5.0 {
                theme::danger(dark)
            } else {
                theme::accent(dark)
            };
            painter.circle_stroke(c, 39.0, Stroke::new(4.0, ring_track(dark)));
            painter.add(ring_arc(c, 39.0, frac, Stroke::new(4.0, ring_color)));
            let color = AVATAR_COLORS
                .get((self.request.device_id % 8) as usize)
                .copied()
                .unwrap_or(AVATAR_COLORS[0]);
            painter.circle_filled(c, 32.0, color);
            painter.text(
                c,
                Align2::CENTER_CENTER,
                tx.d(&initials(&self.request.peer_name)),
                FontId::proportional(25.0),
                Color32::WHITE,
            );
            ui.add_space(4.0);
            theme::column(ui, tx.rtl(), |ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                let name = if self.request.peer_name.is_empty() {
                    tx.n(&id_text(self.request.device_id))
                } else {
                    tx.d(&self.request.peer_name)
                };
                ui.add(egui::Label::new(RichText::new(name).size(20.0).strong()).truncate());
                ui.label(theme::caption(ui, tx.t(S::PopupWants)));
                theme::row(ui, tx.rtl(), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let weak = ui.visuals().weak_text_color();
                    ui.add(Icon::Hash.image(13.0, weak));
                    ui.label(
                        RichText::new(tx.n(&id_text(self.request.device_id)))
                            .monospace()
                            .strong(),
                    );
                    ui.add_space(6.0);
                    ui.add(Icon::Network.image(13.0, weak));
                    ui.label(
                        RichText::new(tx.n(&self.request.address))
                            .monospace()
                            .color(weak),
                    );
                });
            });
        });
        ui.add_space(6.0);
        let (icon, key) = match self.request.auth {
            PromptAuth::AcceptPrompt => (Icon::Bell, S::PopupAuthPrompt),
            PromptAuth::OneTimeCode => (Icon::Key, S::PopupAuthCode),
            PromptAuth::Password => (Icon::Lock, S::PopupAuthPassword),
        };
        theme::row(ui, tx.rtl(), |ui| {
            let accent = theme::accent(dark);
            egui::Frame::new()
                .fill(accent.gamma_multiply(0.14))
                .corner_radius(CornerRadius::same(255))
                .inner_margin(Margin::symmetric(10, 3))
                .show(ui, |ui| {
                    ui.spacing_mut().interact_size.y = 18.0;
                    theme::row(ui, tx.rtl(), |ui| {
                        ui.spacing_mut().item_spacing.x = 5.0;
                        ui.add(icon.image(13.0, accent));
                        ui.label(RichText::new(tx.t(key)).small().color(accent).strong());
                    });
                });
        });
    }

    /// Toggleable permission chips, wrapping in reading order.
    fn chips(&mut self, ui: &mut Ui, tx: Tx) {
        let dark = ui.visuals().dark_mode;
        let accent = theme::accent(dark);
        let fixed = self.request.permissions_locked;
        // Fixed permissions: show only what will be granted.
        let offered = if fixed {
            self.request.permissions.0
        } else {
            CORE_CHIPS.union(self.request.permissions.0)
        };
        let layout = if tx.rtl() {
            Layout::right_to_left(Align::Center).with_main_wrap(true)
        } else {
            Layout::left_to_right(Align::Center).with_main_wrap(true)
        };
        ui.allocate_ui_with_layout(Vec2::new(ui.available_width(), 0.0), layout, |ui| {
            ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
            for (bit, key) in PERMISSION_LABELS {
                if !offered.contains(bit) {
                    continue;
                }
                let on = self.permissions.contains(bit);
                let locked = bit == Permissions::VIEW || fixed;
                let (fg, fill, stroke) = if on {
                    (
                        accent,
                        accent.gamma_multiply(0.16),
                        Stroke::new(1.0, accent),
                    )
                } else {
                    let weak = ui.visuals().weak_text_color();
                    (
                        weak,
                        Color32::TRANSPARENT,
                        Stroke::new(1.0, weak.gamma_multiply(0.5)),
                    )
                };
                // Viewing is what a session is: always on, shown with a lock.
                let icon = match (bit == Permissions::VIEW, on) {
                    (true, _) => Icon::Lock,
                    (false, true) => Icon::Check,
                    (false, false) => Icon::Plus,
                };
                let text = RichText::new(tx.t(key)).size(13.0).color(fg);
                let image = icon.image(13.0, fg);
                let button = if tx.rtl() {
                    egui::Button::new((text, image))
                } else {
                    egui::Button::new((image, text))
                }
                .fill(fill)
                .stroke(stroke)
                .corner_radius(CornerRadius::same(255))
                .min_size(Vec2::new(0.0, 26.0));
                let resp = ui.add(button);
                if resp.clicked() && !locked {
                    self.permissions = if on {
                        self.permissions.difference(bit)
                    } else {
                        self.permissions.union(bit)
                    };
                }
            }
        });
    }

    /// Decline (secondary) and Accept (primary), sharing the row.
    fn buttons(&self, ui: &mut Ui, tx: Tx, armed: bool) -> Option<Outcome> {
        let dark = ui.visuals().dark_mode;
        let mut out = None;
        let gap = 10.0;
        let w = ((ui.available_width() - gap) / 2.0).max(60.0);
        let size = Vec2::new(w, 40.0);
        theme::row(ui, tx.rtl(), |ui| {
            ui.spacing_mut().item_spacing.x = gap;
            let weak = ui.visuals().widgets.inactive.fg_stroke.color;
            let decline = crate::ui::icons::icon_text_colored(
                tx,
                Icon::Reject,
                RichText::new(tx.t(S::Decline)).strong().color(weak),
                weak,
            )
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::new(1.0, weak.gamma_multiply(0.45)))
            .corner_radius(CornerRadius::same(10))
            .min_size(size);
            if ui.add(decline).clicked() && armed {
                out = Some(Outcome::Decline);
            }
            let accept = crate::ui::icons::icon_text_colored(
                tx,
                Icon::Accept,
                RichText::new(tx.t(S::Accept))
                    .strong()
                    .color(Color32::WHITE),
                Color32::WHITE,
            )
            .fill(theme::accent(dark))
            .corner_radius(CornerRadius::same(10))
            .min_size(size);
            if ui.add(accept).clicked() && armed {
                out = Some(Outcome::Accept);
            }
        });
        out
    }
}

fn ring_track(dark: bool) -> Color32 {
    if dark {
        Color32::from_white_alpha(28)
    } else {
        Color32::from_black_alpha(22)
    }
}

/// Arc from 12 o'clock, clockwise, covering `frac` (0..=1) of the circle.
fn ring_arc(center: egui::Pos2, radius: f32, frac: f32, stroke: Stroke) -> egui::Shape {
    let frac = frac.clamp(0.0, 1.0);
    let steps = ((64.0 * frac).ceil() as usize).max(2);
    let start = -std::f32::consts::FRAC_PI_2;
    let sweep = std::f32::consts::TAU * frac;
    let points: Vec<egui::Pos2> = (0..=steps)
        .map(|i| {
            let a = start + sweep * (i as f32 / steps as f32);
            center + Vec2::new(a.cos(), a.sin()) * radius
        })
        .collect();
    egui::Shape::line(points, stroke)
}

/// `123 456 789`.
fn id_text(id: u32) -> String {
    crate::backend::DeviceId(id).to_string()
}

/// Up to two initials (one for Arabic and other scripts without letter case, where two
/// isolated letters read worse than one). Falls back to a bullet for an empty name.
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name
        .split(|c: char| c.is_whitespace() || matches!(c, '-' | '_' | '.'))
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .collect();
    let first = |w: &str| w.chars().find(|c| c.is_alphanumeric());
    let Some(a) = words.first().and_then(|w| first(w)) else {
        return "•".to_owned();
    };
    let cased = a.is_uppercase() || a.is_lowercase();
    let mut out: String = a.to_uppercase().collect();
    if cased {
        if let Some(b) = words.get(1).and_then(|w| first(w)) {
            out.extend(b.to_uppercase());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{DigitStyle, Lang};
    use egui_kittest::kittest::Queryable as _;
    use egui_kittest::Harness;

    #[test]
    fn initials_cover_latin_arabic_and_empty_names() {
        assert_eq!(initials("Lab-Laptop"), "LL");
        assert_eq!(initials("tamer pc"), "TP");
        assert_eq!(initials("حاسوب-المختبر"), "ح");
        assert_eq!(initials("  "), "•");
        assert_eq!(initials("42-box"), "4");
    }

    #[test]
    fn decision_always_keeps_view_and_trust_only_on_accept() {
        let mut card = AcceptCard::new(PromptRequest::default());
        card.permissions = Permissions::FILES;
        card.trust = true;
        let d = card.decision(Outcome::Accept);
        assert!(d.permissions.0.contains(Permissions::VIEW));
        assert!(d.permissions.0.contains(Permissions::FILES));
        assert!(d.trust);
        assert!(!card.decision(Outcome::Decline).trust);
    }

    #[test]
    fn locked_permissions_ignore_changes() {
        let mut card = AcceptCard::new(PromptRequest {
            permissions_locked: true,
            ..PromptRequest::default()
        });
        let defaults = card.request.permissions.0;
        card.permissions = Permissions::VIEW;
        assert_eq!(card.decision(Outcome::Accept).permissions.0, defaults);
    }

    fn request(lang: Lang, name: &str) -> PromptRequest {
        PromptRequest {
            peer_name: name.into(),
            device_id: 950_442_118,
            address: "192.168.1.81".into(),
            lang,
            timeout_secs: 30,
            ..PromptRequest::default()
        }
    }

    fn harness(lang: Lang, dark: bool, name: &str, width: f32) -> Harness<'static, AcceptCard> {
        let tx = Tx::new(lang, DigitStyle::Western);
        let mut h = Harness::builder()
            .with_size(egui::vec2(width, 520.0))
            .with_theme(if dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            })
            .build_ui_state(
                move |ui, card: &mut AcceptCard| {
                    card.show(ui, tx, 0);
                },
                AcceptCard::new(request(lang, name)),
            );
        theme::install(&h.ctx);
        theme::apply_lang(&h.ctx, lang);
        h.run_steps(3);
        h
    }

    #[test]
    fn renders_in_both_languages_and_themes() {
        for lang in [Lang::En, Lang::Ar] {
            for dark in [false, true] {
                let h = harness(lang, dark, "حاسوب-المختبر", 420.0);
                let tx = Tx::new(lang, DigitStyle::Western);
                let _ = h.get_by_label(tx.t(S::Accept));
                let _ = h.get_by_label(tx.t(S::Decline));
            }
        }
    }

    #[test]
    fn long_arabic_name_on_a_narrow_card_does_not_panic() {
        let long = "جهاز الحاسوب المحمول الخاص بقسم المحاسبة في الطابق الثالث من المبنى الرئيسي";
        let h = harness(Lang::Ar, true, long, 300.0);
        assert!(h.state().request.peer_name.chars().count() <= 64);
    }

    #[test]
    fn countdown_runs_out_as_timeout() {
        let tx = Tx::new(Lang::En, DigitStyle::Western);
        let mut h = Harness::builder().build_ui_state(
            move |ui, s: &mut (AcceptCard, Option<Outcome>)| {
                let now = ui.input(|i| i.time);
                s.0.set_elapsed(now, 31.0);
                if let Some(o) = s.0.show(ui, tx, 0) {
                    s.1 = Some(o);
                }
            },
            (AcceptCard::new(request(Lang::En, "pc")), None),
        );
        h.run_steps(1);
        assert_eq!(h.state().1, Some(Outcome::Timeout));
        assert_eq!(h.state().0.remaining(h.ctx.input(|i| i.time)), 0.0);
    }

    #[test]
    fn buttons_ignore_clicks_until_armed_then_answer() {
        let tx = Tx::new(Lang::En, DigitStyle::Western);
        let mut h = Harness::builder()
            .with_size(egui::vec2(420.0, 520.0))
            .with_step_dt(0.02)
            .build_ui_state(
                move |ui, s: &mut (AcceptCard, Option<Outcome>)| {
                    if let Some(o) = s.0.show(ui, tx, 0) {
                        s.1 = Some(o);
                    }
                },
                (AcceptCard::new(request(Lang::En, "Lab-Laptop")), None),
            );
        theme::install(&h.ctx);
        h.run_steps(2);
        h.get_by_label(tx.t(S::Accept)).click();
        h.run_steps(2);
        assert_eq!(
            h.state().1,
            None,
            "a click right after appearing must not accept"
        );
        // Let the arm delay pass, then switch Files off and accept.
        let now = h.ctx.input(|i| i.time);
        h.state_mut().0.set_elapsed(now, 2.0);
        h.get_by_label(tx.t(S::PermFiles)).click();
        h.run_steps(2);
        h.get_by_label(tx.t(S::Accept)).click();
        h.run_steps(2);
        assert_eq!(h.state().1, Some(Outcome::Accept));
        let d = h.state().0.decision(Outcome::Accept);
        assert!(!d.permissions.0.contains(Permissions::FILES));
    }
}
