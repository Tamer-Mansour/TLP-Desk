//! A text editor that gets bidirectional text right while it is being edited.
//!
//! **Why:** egui's `TextEdit` keeps its caret, selection and arrow keys in *logical* order and
//! lays words out left to right, so typing Arabic shows the words in the wrong order and the
//! caret jumps around. `TextEdit::layouter` cannot fix that (the galley it returns must have the
//! same characters in the same order as the edited string). So fields whose text contains
//! right-to-left characters use this widget instead; plain Latin text keeps egui's own
//! `TextEdit` (IME, undo, accessibility).
//!
//! **How:** the text stays a logical-order `String`. [`model`] computes, per visual line, the
//! screen order (UAX #9 via `unicode-bidi`, [`crate::i18n::bidi_line`]), the caret slots and
//! all navigation; this file lays each line out with egui's shaper, reads glyph positions to
//! place the caret/selection, handles keyboard and pointer events and paints.
//!
//! The caret and anchor live in egui's `TextEditState` under the same widget id, so the field
//! switches between this widget and `TextEdit` (when the last Arabic letter is deleted or the
//! first one is typed) without losing the caret or the focus.
//!
//! Limitations: no IME pre-edit string and no accessibility text-edit tree while this widget
//! is active (typing Arabic with a normal keyboard layout never needs them); undo is a plain
//! snapshot stack.

mod editor;
pub mod model;

use std::sync::Arc;

use eframe::egui::{
    self, text::CCursor, text_selection::CCursorRange, Color32, EventFilter, FontId, Galley, Id,
    Key, Modifiers, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2,
};

use self::model::{split_segments, Layout, Pos};
use crate::i18n::{BaseDir, Tx};

/// What the Enter key does in a multi-line editor (a single-line editor gives up focus).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewlineKey {
    /// Enter inserts a line break.
    Enter,
    /// Shift+Enter inserts a line break; plain Enter is left to the caller (chat: send).
    ShiftEnter,
}

/// Padding between the frame and the text.
const PAD: Vec2 = Vec2::new(4.0, 3.0);
/// Undo steps kept.
const MAX_UNDO: usize = 100;

/// A bidi-aware text editor; see the module docs.
pub struct BidiEdit<'a> {
    text: &'a mut String,
    id: Id,
    width: f32,
    multiline: bool,
    rows: usize,
    base: BaseDir,
    default_rtl: bool,
    interactive: bool,
    newline: NewlineKey,
}

impl<'a> BidiEdit<'a> {
    /// A single-line editor for `text` with widget id `id`.
    pub fn new(text: &'a mut String, id: Id) -> Self {
        Self {
            text,
            id,
            width: 200.0,
            multiline: false,
            rows: 1,
            base: BaseDir::Auto,
            default_rtl: false,
            interactive: true,
            newline: NewlineKey::Enter,
        }
    }

    /// Total width of the field.
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    /// Multi-line editor with wrapping, at least `rows` lines tall.
    pub fn multiline(mut self, rows: usize, newline: NewlineKey) -> Self {
        self.multiline = true;
        self.rows = rows.max(1);
        self.newline = newline;
        self
    }

    /// Paragraph direction (default: from the first strong character of each line).
    pub fn base(mut self, base: BaseDir) -> Self {
        self.base = base;
        self
    }

    /// Alignment of lines without strong characters when the direction is automatic.
    pub fn default_rtl(mut self, rtl: bool) -> Self {
        self.default_rtl = rtl;
        self
    }

    /// `false` shows the text but ignores input.
    pub fn interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }

    /// Lays out, handles input and paints. `changed()` is set on the response when the text
    /// was edited.
    pub fn show(self, ui: &mut Ui) -> Response {
        show(self, ui)
    }
}

/// Convenience for the common case: `Tx` gives the UI direction.
pub fn default_rtl_of(tx: Tx) -> bool {
    tx.rtl()
}

#[derive(Clone)]
struct Snap {
    text: String,
    anchor: usize,
    caret: usize,
}

#[derive(Clone, Default)]
struct Mem {
    sticky_x: Option<f32>,
    scroll: f32,
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    typing: bool,
    last_interaction: f64,
    /// Where the caret is drawn (two places are possible at a bidi run boundary).
    pos: Option<Pos>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Edit {
    Typing,
    Other,
}

/// Everything derived from the current text: segments, their galleys and positions.
struct Frame {
    layout: Layout,
    galleys: Vec<Arc<Galley>>,
    /// Left/right x (relative to the segment origin) of every cluster, per segment.
    spans: Vec<Vec<(f32, f32)>>,
    line_h: f32,
    inner: Rect,
    align_right: Vec<bool>,
}

impl Frame {
    /// Left/right x (relative to the segment origin) of cluster `ci` of segment `si`.
    fn cluster_span(&self, si: usize, ci: usize) -> (f32, f32) {
        self.spans
            .get(si)
            .and_then(|v| v.get(ci))
            .copied()
            .unwrap_or((0.0, 0.0))
    }

    /// x of the segment's left edge (before scrolling).
    fn origin_x(&self, si: usize) -> f32 {
        let w = self.galleys.get(si).map_or(0.0, |g| g.size().x);
        if self.align_right.get(si).copied().unwrap_or(false) {
            self.inner.right() - w
        } else {
            self.inner.left()
        }
    }

    /// Screen x (before scrolling) of caret slot `slot` in segment `si`.
    fn slot_x(&self, si: usize, slot: usize) -> f32 {
        let Some(seg) = self.layout.segs.get(si) else {
            return self.inner.left();
        };
        let origin = self.origin_x(si);
        if seg.clusters.is_empty() {
            return origin;
        }
        let x = if slot == 0 {
            seg.visual
                .first()
                .map_or(0.0, |&ci| self.cluster_span(si, ci).0)
        } else {
            seg.visual
                .get(slot - 1)
                .map_or(0.0, |&ci| self.cluster_span(si, ci).1)
        };
        origin + x
    }

    fn pos_x(&self, pos: Pos) -> f32 {
        self.slot_x(pos.seg, pos.slot)
    }

    fn seg_top(&self, si: usize) -> f32 {
        self.inner.top() + si as f32 * self.line_h
    }
}

fn display_string(chars: &[char], seg: &model::Segment) -> String {
    match &seg.line {
        Some(l) => l.display.iter().collect(),
        None => chars
            .get(seg.start..seg.end)
            .unwrap_or_default()
            .iter()
            .collect(),
    }
}

/// Where each cluster of `seg` is drawn, from egui's glyph list.
///
/// epaint does not give one glyph per character for right-to-left runs (its glyph list is in
/// shaping order, followed by zero-width filler entries), so glyph indices cannot be matched to
/// characters. Instead the drawn glyphs (advance > 0) are sorted by x, which is screen order,
/// and matched against the clusters in screen order (`seg.visual`) by character. A cluster
/// with no glyph of its own (the tail of a lam-alef ligature) gets an empty span where the
/// previous one ended.
fn cluster_spans(chars: &[char], seg: &model::Segment, galley: &Galley) -> Vec<(f32, f32)> {
    let mut drawn: Vec<(f32, f32, char)> = galley
        .rows
        .iter()
        .flat_map(|r| r.row.glyphs.iter())
        .filter(|g| g.advance_width > 0.0)
        .map(|g| (g.pos.x, g.pos.x + g.advance_width, g.chr))
        .collect();
    drawn.sort_by(|a, b| a.0.total_cmp(&b.0));
    let shown = |li: usize| -> char {
        let local = li - seg.start;
        match &seg.line {
            Some(l) => l
                .display_of_logical()
                .get(local)
                .and_then(|&p| l.display.get(p))
                .copied()
                .unwrap_or(' '),
            None => chars.get(li).copied().unwrap_or(' '),
        }
    };
    let bases: Vec<char> = seg
        .visual
        .iter()
        .filter_map(|&ci| seg.clusters.get(ci))
        .map(|c| shown(c.start))
        .collect();
    let mut spans = vec![(0.0, 0.0); seg.clusters.len()];
    let mut k = 0;
    let mut edge = drawn.first().map_or(0.0, |g| g.0);
    for (n, &ci) in seg.visual.iter().enumerate() {
        let base = bases.get(n).copied().unwrap_or(' ');
        let take = drawn
            .get(k)
            .is_some_and(|g| g.2 == base || !bases.get(n + 1..).unwrap_or_default().contains(&g.2));
        if take {
            if let (Some(g), Some(slot)) = (drawn.get(k), spans.get_mut(ci)) {
                *slot = (g.0, g.1);
                edge = g.1;
            }
            k += 1;
        } else if let Some(slot) = spans.get_mut(ci) {
            *slot = (edge, edge);
        }
    }
    spans
}

fn build_frame(ui: &Ui, chars: &[char], p: &Params, inner: Rect, line_h_min: f32) -> Frame {
    let painter = ui.painter();
    let layout_text =
        |s: String| -> Arc<Galley> { painter.layout_no_wrap(s, p.font.clone(), p.color) };
    let max_w = p.multiline.then_some(inner.width());
    let segs = {
        let mut measure = |s: usize, e: usize| -> f32 {
            let seg = model::Segment::new(chars, s, e, p.base);
            layout_text(display_string(chars, &seg)).size().x
        };
        split_segments(chars, p.base, max_w, &mut measure)
    };
    let layout = Layout {
        chars: chars.to_vec(),
        segs,
    };
    let galleys: Vec<Arc<Galley>> = layout
        .segs
        .iter()
        .map(|s| layout_text(display_string(chars, s)))
        .collect();
    let spans = layout
        .segs
        .iter()
        .zip(&galleys)
        .map(|(seg, galley)| cluster_spans(chars, seg, galley))
        .collect();
    let line_h = galleys
        .iter()
        .map(|g| g.size().y)
        .fold(line_h_min, f32::max);
    let align_right = layout
        .segs
        .iter()
        .map(|s| match &s.line {
            Some(l) => l.base_rtl,
            None => match p.base {
                BaseDir::Rtl => true,
                BaseDir::Ltr => false,
                BaseDir::Auto => s.start == s.end && p.default_rtl,
            },
        })
        .collect();
    Frame {
        layout,
        galleys,
        spans,
        line_h,
        inner,
        align_right,
    }
}

/// Settings that do not change while the widget runs.
struct Params {
    font: FontId,
    color: Color32,
    base: BaseDir,
    default_rtl: bool,
    multiline: bool,
}

fn show(edit: BidiEdit<'_>, ui: &mut Ui) -> Response {
    let BidiEdit {
        text,
        id,
        width,
        multiline,
        rows,
        base,
        default_rtl,
        interactive,
        newline,
    } = edit;
    let ctx = ui.ctx().clone();
    let params = Params {
        font: egui::TextStyle::Body.resolve(ui.style()),
        color: ui.visuals().text_color(),
        base,
        default_rtl,
        multiline,
    };
    let width = width.min(ui.available_width()).max(40.0);

    // Size the frame from the wrapped text.
    let probe_inner = Rect::from_min_size(Pos2::ZERO, Vec2::new(width - 2.0 * PAD.x, 0.0));
    let min_line_h = ui.fonts_mut(|f| f.row_height(&params.font));
    let mut chars: Vec<char> = text.chars().collect();
    let probe = build_frame(ui, &chars, &params, probe_inner, min_line_h);
    let line_count = if multiline {
        probe.layout.segs.len().max(rows)
    } else {
        1
    };
    let height = line_count as f32 * probe.line_h + 2.0 * PAD.y;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let sense = if interactive {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let resp = ui.interact(rect, id, sense);
    let inner = rect.shrink2(PAD);
    let mut frame = build_frame(ui, &chars, &params, inner, min_line_h);

    let mut mem: Mem = ctx.data(|d| d.get_temp(id)).unwrap_or_default();
    let mut state = egui::text_edit::TextEditState::load(&ctx, id).unwrap_or_default();
    let (mut anchor, mut caret) = state
        .cursor
        .char_range()
        .map_or((chars.len(), chars.len()), |r| {
            (r.secondary.index.into(), r.primary.index.into())
        });
    anchor = frame.layout.snap(anchor);
    caret = frame.layout.snap(caret);
    let mut pos = match mem.pos {
        Some(p) if p.caret == caret => frame.layout.valid_pos(p),
        _ => frame.layout.pos(caret),
    };

    // Pointer.
    let mut changed = false;
    if interactive {
        let pressed = resp.contains_pointer() && ui.input(|i| i.pointer.primary_pressed());
        if pressed {
            ctx.memory_mut(|m| m.request_focus(id));
        }
        let shift = ui.input(|i| i.modifiers.shift);
        if (pressed || resp.dragged()) && resp.is_pointer_button_down_on() {
            if let Some(pointer) = resp.interact_pointer_pos() {
                let si = (((pointer.y - frame.inner.top()) / frame.line_h)
                    .floor()
                    .max(0.0) as usize)
                    .min(frame.layout.segs.len().saturating_sub(1));
                let x = pointer.x + mem.scroll;
                let c = frame
                    .layout
                    .pos_at_x(si, x, &|s, slot| frame.slot_x(s, slot))
                    .unwrap_or(pos);
                pos = c;
                caret = c.caret;
                if pressed && !shift {
                    anchor = c.caret;
                }
                mem.sticky_x = None;
                mem.typing = false;
                mem.last_interaction = ui.input(|i| i.time);
            }
        }
        if resp.double_clicked() {
            let w = frame.layout.word_at(caret);
            anchor = w.start;
            caret = w.end;
            pos = frame.layout.pos(caret);
        }
    }

    // Keyboard.
    let focused = ctx.memory(|m| m.has_focus(id));
    if interactive && focused {
        ctx.memory_mut(|m| {
            m.set_focus_lock_filter(
                id,
                EventFilter {
                    tab: false,
                    horizontal_arrows: true,
                    vertical_arrows: multiline,
                    escape: false,
                },
            )
        });
        let events = ui.input(|i| i.events.clone());
        let mut handled: Vec<(Modifiers, Key)> = Vec::new();
        let mut st = editor::Editor {
            ui,
            id,
            chars: &mut chars,
            anchor: &mut anchor,
            pos: &mut pos,
            mem: &mut mem,
            frame: &mut frame,
            params: &params,
            inner,
            min_line_h,
            changed: &mut changed,
            multiline,
            newline,
        };
        for ev in events {
            st.event(ev, &mut handled);
        }
        ui.input_mut(|i| {
            for (m, k) in handled {
                i.consume_key(m, k);
            }
        });
    }

    if changed {
        *text = chars.iter().collect();
    }
    // Rebuild once more for painting if the text changed.
    if changed {
        frame = build_frame(ui, &chars, &params, inner, min_line_h);
        anchor = frame.layout.snap(anchor);
    }
    caret = pos.caret;
    let pos = frame.layout.valid_pos(pos);
    mem.pos = Some(pos);
    state.cursor.set_char_range(Some(CCursorRange::two(
        CCursor::new(anchor),
        CCursor::new(caret),
    )));
    state.store(&ctx, id);

    // Keep the caret in view (single line).
    if !multiline {
        let content_w = frame.galleys.first().map_or(0.0, |g| g.size().x);
        if content_w <= inner.width() {
            mem.scroll = 0.0;
        } else if focused {
            let cx = frame.pos_x(pos);
            if cx - mem.scroll < inner.left() {
                mem.scroll = cx - inner.left();
            } else if cx - mem.scroll > inner.right() {
                mem.scroll = cx - inner.right();
            }
        }
    } else {
        mem.scroll = 0.0;
    }

    paint(
        ui,
        &resp,
        &frame,
        &mem,
        anchor,
        pos,
        focused && interactive,
        rect,
    );
    if interactive && resp.hovered() {
        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::Text);
    }
    let shown: String = chars.iter().collect();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, interactive, &shown));
    ctx.data_mut(|d| d.insert_temp(id, mem));
    let mut resp = resp;
    if changed {
        resp.mark_changed();
    }
    resp
}

#[allow(clippy::too_many_arguments)]
fn paint(
    ui: &Ui,
    resp: &Response,
    frame: &Frame,
    mem: &Mem,
    anchor: usize,
    pos: Pos,
    focused: bool,
    rect: Rect,
) {
    let visuals = ui.visuals();
    let widget = if focused {
        &visuals.widgets.active
    } else if resp.hovered() {
        &visuals.widgets.hovered
    } else {
        &visuals.widgets.inactive
    };
    let stroke = if focused {
        visuals.selection.stroke
    } else {
        widget.bg_stroke
    };
    ui.painter().rect(
        rect,
        widget.corner_radius,
        visuals.extreme_bg_color,
        stroke,
        StrokeKind::Inside,
    );
    let painter = ui.painter_at(frame.inner.expand2(Vec2::new(PAD.x - 1.0, 0.0)));
    let scroll = Vec2::new(-mem.scroll, 0.0);
    let caret = pos.caret;
    let (lo, hi) = (anchor.min(caret), anchor.max(caret));
    if lo != hi {
        for (si, ci) in frame.layout.selected_clusters(lo, hi) {
            let (l, r) = frame.cluster_span(si, ci);
            let o = frame.origin_x(si);
            let top = frame.seg_top(si);
            let r = Rect::from_min_max(
                Pos2::new(o + l - 0.5, top),
                Pos2::new(o + r + 0.5, top + frame.line_h),
            )
            .translate(scroll);
            painter.rect_filled(r, 0.0, visuals.selection.bg_fill);
        }
    }
    for (si, galley) in frame.galleys.iter().enumerate() {
        let pos = Pos2::new(frame.origin_x(si), frame.seg_top(si)) + scroll;
        painter.galley(pos, galley.clone(), visuals.text_color());
    }
    if focused {
        let on = caret_visible(ui, mem);
        if on {
            let x = frame.pos_x(pos) - mem.scroll;
            let top = frame.seg_top(pos.seg);
            painter.line_segment(
                [Pos2::new(x, top), Pos2::new(x, top + frame.line_h)],
                Stroke::new(
                    visuals.text_cursor.stroke.width,
                    visuals.text_cursor.stroke.color,
                ),
            );
        }
    }
}

/// Caret blink: solid while typing, then on/off with the style's durations.
fn caret_visible(ui: &Ui, mem: &Mem) -> bool {
    let style = &ui.visuals().text_cursor;
    if !style.blink {
        return true;
    }
    let (on, off) = (f64::from(style.on_duration), f64::from(style.off_duration));
    let t = ui.input(|i| i.time) - mem.last_interaction;
    let phase = t % (on + off);
    let visible = phase < on;
    let next = if visible {
        on - phase
    } else {
        on + off - phase
    };
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_secs_f64(next.max(0.05)));
    visible
}

#[cfg(test)]
mod tests;
