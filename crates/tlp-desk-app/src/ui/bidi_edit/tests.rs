//! Headless egui tests of the bidi editor.
use super::*;
use crate::i18n::Lang;
use eframe::egui::{pos2, vec2, Event};

/// Runs the widget headlessly with scripted events, like the app does.
struct Rig {
    ctx: egui::Context,
    text: String,
    multiline: bool,
    id: Id,
    pub width: f32,
}

impl Rig {
    fn new(text: &str, multiline: bool) -> Self {
        let ctx = egui::Context::default();
        crate::ui::icons::install(&ctx);
        crate::ui::theme::install(&ctx);
        crate::ui::theme::apply_lang(&ctx, Lang::Ar);
        let id = Id::new("bidi-test");
        let mut rig = Self {
            ctx,
            text: text.to_owned(),
            multiline,
            id,
            width: 300.0,
        };
        rig.frame(vec![]);
        rig.ctx.memory_mut(|m| m.request_focus(id));
        rig.frame(vec![]);
        rig
    }

    fn frame(&mut self, events: Vec<Event>) -> bool {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
            events,
            ..Default::default()
        };
        let (text, multiline, id, width) = (&mut self.text, self.multiline, self.id, self.width);
        let mut changed = false;
        let mut out = self.ctx.run_ui(input, |ui| {
            let mut e = BidiEdit::new(text, id).width(width);
            if multiline {
                e = e.multiline(3, NewlineKey::Enter);
            }
            changed = e.show(ui).changed();
        });
        out.textures_delta.clear();
        changed
    }

    fn cursor(&self) -> (usize, usize) {
        let st = egui::text_edit::TextEditState::load(&self.ctx, self.id).unwrap();
        let r = st.cursor.char_range().unwrap();
        (r.secondary.index.into(), r.primary.index.into())
    }

    fn key(&mut self, key: Key, modifiers: Modifiers) {
        self.frame(vec![Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        }]);
    }

    fn type_text(&mut self, s: &str) {
        for c in s.chars() {
            self.frame(vec![Event::Text(c.to_string())]);
        }
    }
}

#[test]
fn typing_arabic_appends_in_logical_order_and_keeps_the_caret_at_the_end() {
    let mut r = Rig::new("مر", false);
    r.type_text("حبا");
    assert_eq!(r.text, "مرحبا");
    assert_eq!(r.cursor(), (5, 5));
}

#[test]
fn typing_mixed_text_inserts_at_the_caret_not_at_the_visual_end() {
    let mut r = Rig::new("مرحبا 123", false);
    // Caret at the logical end; type a Latin word, then move left and type Arabic.
    r.type_text(" ok");
    assert_eq!(r.text, "مرحبا 123 ok");
    r.key(Key::Home, Modifiers::NONE);
    assert_eq!(r.cursor(), (0, 0));
    r.type_text("ب");
    assert_eq!(r.text, "بمرحبا 123 ok");
}

#[test]
fn arrow_keys_move_the_caret_in_screen_direction() {
    let mut r = Rig::new("مرحبا", false);
    assert_eq!(r.cursor(), (5, 5));
    // The logical end of Arabic text is its screen-left end: Left cannot go further...
    r.key(Key::ArrowLeft, Modifiers::NONE);
    assert_eq!(r.cursor(), (5, 5));
    // ...and Right walks back toward the logical start.
    r.key(Key::ArrowRight, Modifiers::NONE);
    assert_eq!(r.cursor(), (4, 4));
    r.key(Key::ArrowRight, Modifiers::NONE);
    r.key(Key::ArrowRight, Modifiers::NONE);
    assert_eq!(r.cursor(), (2, 2));
    r.key(Key::ArrowLeft, Modifiers::NONE);
    assert_eq!(r.cursor(), (3, 3));
}

#[test]
fn backspace_deletes_the_previous_logical_character_with_its_marks() {
    let mut r = Rig::new("بَت", false);
    r.key(Key::Backspace, Modifiers::NONE);
    assert_eq!(r.text, "بَ");
    r.key(Key::Backspace, Modifiers::NONE);
    assert_eq!(r.text, "");
}

#[test]
fn shift_arrows_select_and_typing_replaces_the_selection() {
    let mut r = Rig::new("مرحبا بك", false);
    r.key(Key::ArrowRight, Modifiers::SHIFT);
    r.key(Key::ArrowRight, Modifiers::SHIFT);
    let (a, c) = r.cursor();
    assert_eq!((a, c), (8, 6));
    r.type_text("ي");
    assert_eq!(r.text, "مرحبا ي");
}

#[test]
fn select_all_copy_and_undo() {
    let mut r = Rig::new("سلام abc", false);
    r.key(Key::A, Modifiers::COMMAND);
    assert_eq!(r.cursor(), (0, 6 + 2));
    r.key(Key::Delete, Modifiers::NONE);
    assert_eq!(r.text, "");
    r.key(Key::Z, Modifiers::COMMAND);
    assert_eq!(r.text, "سلام abc");
}

#[test]
fn paste_flattens_newlines_in_a_single_line_field() {
    let mut r = Rig::new("ب", false);
    r.frame(vec![Event::Paste("س\nص".into())]);
    assert_eq!(r.text, "بس ص");
}

#[test]
fn multiline_enter_inserts_a_newline_and_vertical_moves_work() {
    let mut r = Rig::new("مرحبا", true);
    r.key(Key::Enter, Modifiers::NONE);
    r.type_text("abc");
    assert_eq!(r.text, "مرحبا\nabc");
    r.key(Key::ArrowUp, Modifiers::NONE);
    let (_, c) = r.cursor();
    assert!(c <= 5, "caret moved to the first line: {c}");
    r.key(Key::ArrowDown, Modifiers::NONE);
    let (_, c) = r.cursor();
    assert!(c >= 6, "caret moved to the second line: {c}");
}

#[test]
fn long_arabic_text_wraps_into_several_lines_without_losing_text() {
    let long = "مرحبا بالعالم الجميل ".repeat(8);
    let mut r = Rig::new(&long, true);
    r.width = 160.0;
    r.frame(vec![]);
    assert_eq!(r.text, long);
    r.key(Key::A, Modifiers::COMMAND);
    r.key(Key::Backspace, Modifiers::NONE);
    assert_eq!(r.text, "");
}

#[test]
fn non_interactive_field_ignores_typing() {
    let ctx = egui::Context::default();
    let mut text = String::from("مرحبا");
    let id = Id::new("ro");
    for events in [vec![], vec![Event::Text("x".into())]] {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 300.0))),
            events,
            ..Default::default()
        };
        ctx.memory_mut(|m| m.request_focus(id));
        let mut out = ctx.run_ui(input, |ui| {
            BidiEdit::new(&mut text, id).interactive(false).show(ui);
        });
        out.textures_delta.clear();
    }
    assert_eq!(text, "مرحبا");
}

#[test]
fn caret_x_follows_the_logical_caret_through_mixed_text() {
    // Unit check of the geometry helpers against real glyph positions.
    let ctx = egui::Context::default();
    crate::ui::theme::install(&ctx);
    crate::ui::theme::apply_lang(&ctx, Lang::Ar);
    let mut text = String::from("abc مرحبا");
    let id = Id::new("geo");
    let mut xs: Vec<f32> = Vec::new();
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 300.0))),
        ..Default::default()
    };
    // Fonts installed by `theme::install` take effect on the second frame.
    ctx.run_ui(input.clone(), |_| {}).textures_delta.clear();
    let mut out = ctx.run_ui(input, |ui| {
        BidiEdit::new(&mut text, id).width(300.0).show(ui);
        let chars: Vec<char> = text.chars().collect();
        let params = Params {
            font: egui::TextStyle::Body.resolve(ui.style()),
            color: Color32::WHITE,
            base: BaseDir::Auto,
            default_rtl: false,
            multiline: false,
        };
        let inner = Rect::from_min_size(pos2(4.0, 3.0), vec2(292.0, 20.0));
        let frame = build_frame(ui, &chars, &params, inner, 16.0);
        xs = (0..=chars.len())
            .map(|c| frame.pos_x(frame.layout.pos(c)))
            .collect();
    });
    out.textures_delta.clear();
    // "abc " is Latin: carets 0..=3 go left to right.
    assert!(xs[0] < xs[1] && xs[1] < xs[2] && xs[2] < xs[3], "{xs:?}");
    // Each Arabic letter's trailing edge is its left edge, so with the caret kept beside the
    // character before it, later letters sit further left; the first letter is rightmost.
    assert!(
        xs[5] > xs[6] && xs[6] > xs[7] && xs[7] > xs[8] && xs[8] > xs[9],
        "{xs:?}"
    );
    // The caret after the space and the one at the end both touch the word's left end.
    assert!((xs[4] - xs[9]).abs() < 1.0, "{xs:?}");
    assert!(xs[5] > xs[3], "{xs:?}");
}

/// The app's `text_field` hands over from egui's `TextEdit` to `BidiEdit` when the first Arabic
/// letter is typed, keeping focus and caret, and back when the last one is deleted.
#[test]
fn text_field_switches_editors_without_losing_focus_or_caret() {
    use crate::i18n::{DigitStyle, Tx};
    let ctx = egui::Context::default();
    crate::ui::theme::install(&ctx);
    crate::ui::theme::apply_lang(&ctx, Lang::Ar);
    let tx = Tx::new(Lang::Ar, DigitStyle::Western);
    let mut value = String::from("abc");
    let id_cell = std::cell::Cell::new(None);
    let mut frame = |events: Vec<Event>, value: &mut String| {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 300.0))),
            events,
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            id_cell.set(Some(ui.make_persistent_id("field")));
            crate::ui::widgets::text_field(ui, tx, "field", value, "hint", 300.0);
        });
        out.textures_delta.clear();
    };
    frame(vec![], &mut value);
    frame(vec![], &mut value);
    let id = id_cell.get().unwrap();
    ctx.memory_mut(|m| m.request_focus(id));
    frame(vec![], &mut value);
    // Latin text is edited by egui; the first Arabic letter makes the field a BidiEdit.
    frame(vec![Event::Text("م".into())], &mut value);
    frame(vec![Event::Text("ر".into())], &mut value);
    frame(vec![Event::Text("ح".into())], &mut value);
    assert!(value.contains("مرح"), "{value:?}");
    assert!(ctx.memory(|m| m.has_focus(id)), "focus kept");
    // Delete everything back to Latin: egui's editor takes over again.
    frame(
        vec![Event::Key {
            key: Key::A,
            physical_key: Some(Key::A),
            pressed: true,
            repeat: false,
            modifiers: Modifiers::COMMAND,
        }],
        &mut value,
    );
    frame(
        vec![Event::Key {
            key: Key::Backspace,
            physical_key: Some(Key::Backspace),
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
        &mut value,
    );
    frame(vec![Event::Text("x".into())], &mut value);
    assert_eq!(value, "x");
    assert!(ctx.memory(|m| m.has_focus(id)), "focus kept");
}

/// Cluster spans come out in screen order without overlaps, including a lam-alef ligature.
#[test]
fn cluster_spans_follow_screen_order() {
    let ctx = egui::Context::default();
    crate::ui::theme::install(&ctx);
    crate::ui::theme::apply_lang(&ctx, Lang::Ar);
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 300.0))),
        ..Default::default()
    };
    ctx.run_ui(input.clone(), |_| {}).textures_delta.clear();
    for text in ["مرحبا بالعالم", "لا إله إلا الله", "abc مرحبا 123", "كَتَبَ"]
    {
        let mut out = ctx.run_ui(input.clone(), |ui| {
            let chars: Vec<char> = text.chars().collect();
            let params = Params {
                font: egui::TextStyle::Body.resolve(ui.style()),
                color: Color32::WHITE,
                base: BaseDir::Auto,
                default_rtl: false,
                multiline: false,
            };
            let frame = build_frame(
                ui,
                &chars,
                &params,
                Rect::from_min_size(pos2(0.0, 0.0), vec2(500.0, 20.0)),
                16.0,
            );
            let seg = &frame.layout.segs[0];
            let mut last_right = f32::MIN;
            for &ci in &seg.visual {
                let (l, r) = frame.cluster_span(0, ci);
                assert!(l <= r + 0.01, "{text}: span {l}..{r}");
                assert!(l >= last_right - 1.5, "{text}: overlap at cluster {ci}");
                last_right = r;
            }
        });
        out.textures_delta.clear();
    }
}
