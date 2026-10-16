//! Keyboard and edit handling of the bidi editor (one frame's worth of events).

use eframe::egui::{Event, Id, Key, Modifiers, Rect, Ui};

use super::model::Pos;
use super::{build_frame, Edit, Frame, Mem, NewlineKey, Params, Snap, MAX_UNDO};

/// Mutable editing state for one frame of event handling.
pub(super) struct Editor<'a> {
    pub(super) ui: &'a Ui,
    pub(super) id: Id,
    pub(super) chars: &'a mut Vec<char>,
    pub(super) anchor: &'a mut usize,
    pub(super) pos: &'a mut Pos,
    pub(super) mem: &'a mut Mem,
    pub(super) frame: &'a mut Frame,
    pub(super) params: &'a Params,
    pub(super) inner: Rect,
    pub(super) min_line_h: f32,
    pub(super) changed: &'a mut bool,
    pub(super) multiline: bool,
    pub(super) newline: NewlineKey,
}

impl Editor<'_> {
    fn touch(&mut self) {
        self.mem.last_interaction = self.ui.input(|i| i.time);
    }

    fn relayout(&mut self) {
        *self.frame = build_frame(
            self.ui,
            self.chars,
            self.params,
            self.inner,
            self.min_line_h,
        );
        *self.pos = self
            .frame
            .layout
            .pos(self.frame.layout.snap(self.pos.caret));
        *self.anchor = self.frame.layout.snap(*self.anchor);
    }

    fn selection(&self) -> (usize, usize) {
        (
            (*self.anchor).min(self.pos.caret),
            (*self.anchor).max(self.pos.caret),
        )
    }

    fn snapshot(&self) -> Snap {
        Snap {
            text: self.chars.iter().collect(),
            anchor: *self.anchor,
            caret: self.pos.caret,
        }
    }

    /// Records an undo step before an edit (consecutive typing is one step).
    fn push_undo(&mut self, kind: Edit) {
        if kind == Edit::Typing && self.mem.typing {
            return;
        }
        let snap = self.snapshot();
        self.mem.undo.push(snap);
        if self.mem.undo.len() > MAX_UNDO {
            self.mem.undo.remove(0);
        }
        self.mem.redo.clear();
        self.mem.typing = kind == Edit::Typing;
    }

    /// Replaces `lo..hi` by `with` and puts the caret after it.
    fn replace(&mut self, lo: usize, hi: usize, with: &[char], kind: Edit) {
        if lo == hi && with.is_empty() {
            return;
        }
        self.push_undo(kind);
        let hi = hi.min(self.chars.len());
        let lo = lo.min(hi);
        self.chars.splice(lo..hi, with.iter().copied());
        self.pos.caret = lo + with.len();
        *self.anchor = self.pos.caret;
        *self.changed = true;
        self.mem.sticky_x = None;
        self.touch();
        self.relayout();
    }

    pub(super) fn insert(&mut self, text: &str, kind: Edit) {
        let clean: Vec<char> = text
            .chars()
            .filter_map(|c| match c {
                '\r' => None,
                '\n' if self.multiline => Some('\n'),
                '\n' | '\t' => Some(' '),
                c if c.is_control() => None,
                c => Some(c),
            })
            .collect();
        if clean.is_empty() {
            return;
        }
        let (lo, hi) = self.selection();
        self.replace(lo, hi, &clean, kind);
    }

    pub(super) fn copy(&mut self, cut: bool) {
        let (lo, hi) = self.selection();
        if lo == hi {
            return;
        }
        let s: String = self.chars.get(lo..hi).unwrap_or_default().iter().collect();
        self.ui.ctx().copy_text(s);
        if cut {
            self.replace(lo, hi, &[], Edit::Other);
        }
    }

    fn set_pos(&mut self, target: Pos, extend: bool) {
        *self.pos = target;
        if !extend {
            *self.anchor = target.caret;
        }
        self.mem.typing = false;
        self.touch();
    }

    fn undo(&mut self, redo: bool) {
        let snap = if redo {
            self.mem.redo.pop()
        } else {
            self.mem.undo.pop()
        };
        let Some(snap) = snap else { return };
        let now = self.snapshot();
        if redo {
            self.mem.undo.push(now);
        } else {
            self.mem.redo.push(now);
        }
        *self.chars = snap.text.chars().collect();
        *self.anchor = snap.anchor;
        self.pos.caret = snap.caret;
        *self.changed = true;
        self.mem.typing = false;
        self.touch();
        self.relayout();
    }

    /// Logical start of the word before `caret` (Ctrl+Backspace).
    fn word_back(&self, caret: usize) -> usize {
        let mut i = caret;
        while i > 0 && self.chars.get(i - 1).is_some_and(|c| c.is_whitespace()) {
            i -= 1;
        }
        while i > 0 && self.chars.get(i - 1).is_some_and(|c| !c.is_whitespace()) {
            i -= 1;
        }
        i
    }

    /// Logical end of the word after `caret` (Ctrl+Delete).
    fn word_fwd(&self, caret: usize) -> usize {
        let n = self.chars.len();
        let mut i = caret;
        while i < n && self.chars.get(i).is_some_and(|c| c.is_whitespace()) {
            i += 1;
        }
        while i < n && self.chars.get(i).is_some_and(|c| !c.is_whitespace()) {
            i += 1;
        }
        i
    }

    /// Applies one input event; keys that were used are added to `handled` (to be consumed).
    pub(super) fn event(&mut self, ev: Event, handled: &mut Vec<(Modifiers, Key)>) {
        match ev {
            Event::Text(t) | Event::Ime(eframe::egui::ImeEvent::Commit(t)) => {
                self.insert(&t, Edit::Typing)
            }
            Event::Paste(t) => self.insert(&t, Edit::Other),
            Event::Copy => self.copy(false),
            Event::Cut => self.copy(true),
            Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } if self.key(key, modifiers) => handled.push((modifiers, key)),
            _ => {}
        }
    }

    /// Handles a key press; returns `true` if the key was used (so it is consumed).
    fn key(&mut self, key: Key, m: Modifiers) -> bool {
        let extend = m.shift;
        let word = m.command;
        match key {
            Key::ArrowLeft | Key::ArrowRight => {
                let right = key == Key::ArrowRight;
                let (lo, hi) = self.selection();
                let layout = &self.frame.layout;
                let target = if lo != hi && !extend {
                    // Collapse to the selection end that lies in the arrow's direction.
                    let (pl, ph) = (layout.pos(lo), layout.pos(hi));
                    let (xl, xh) = (self.frame.pos_x(pl), self.frame.pos_x(ph));
                    if (xl <= xh) == right {
                        ph
                    } else {
                        pl
                    }
                } else if word {
                    layout.move_word(*self.pos, right)
                } else {
                    layout.move_visual(*self.pos, right)
                };
                self.mem.sticky_x = None;
                self.set_pos(target, extend);
                true
            }
            Key::ArrowUp | Key::ArrowDown if self.multiline => {
                let x = self
                    .mem
                    .sticky_x
                    .unwrap_or_else(|| self.frame.pos_x(*self.pos));
                let frame = &*self.frame;
                let target =
                    frame
                        .layout
                        .move_vertical(*self.pos, key == Key::ArrowUp, x, &|s, slot| {
                            frame.slot_x(s, slot)
                        });
                self.set_pos(target, extend);
                self.mem.sticky_x = Some(x);
                true
            }
            Key::Home | Key::End => {
                let end = key == Key::End;
                let layout = &self.frame.layout;
                let caret = if word {
                    if end {
                        self.chars.len()
                    } else {
                        0
                    }
                } else {
                    layout
                        .segs
                        .get(self.pos.seg)
                        .map_or(0, |s| if end { s.end } else { s.start })
                };
                let mut target = layout.pos(caret);
                // End of a wrapped line stays on that line (a soft-wrap caret belongs to the
                // next one).
                if let Some(seg) = layout.segs.get(self.pos.seg) {
                    if caret >= seg.start && caret <= seg.end {
                        target.seg = self.pos.seg;
                        target.slot = if end {
                            seg.default_slot_at_end()
                        } else {
                            seg.default_slot(caret)
                        };
                    }
                }
                self.mem.sticky_x = None;
                self.set_pos(target, extend);
                true
            }
            Key::Backspace | Key::Delete => {
                let (lo, hi) = self.selection();
                if lo != hi {
                    self.replace(lo, hi, &[], Edit::Other);
                    return true;
                }
                let range = if key == Key::Backspace {
                    if word {
                        let s = self.word_back(self.pos.caret);
                        (s < self.pos.caret).then_some(s..self.pos.caret)
                    } else {
                        self.frame.layout.back_range(self.pos.caret)
                    }
                } else if word {
                    let e = self.word_fwd(self.pos.caret);
                    (e > self.pos.caret).then_some(self.pos.caret..e)
                } else {
                    self.frame.layout.forward_range(self.pos.caret)
                };
                if let Some(r) = range {
                    self.replace(r.start, r.end, &[], Edit::Other);
                }
                true
            }
            Key::Enter => {
                if !self.multiline {
                    self.ui.ctx().memory_mut(|mem| mem.surrender_focus(self.id));
                    return false;
                }
                let wanted = match self.newline {
                    NewlineKey::Enter => !m.shift && !m.command && !m.alt,
                    NewlineKey::ShiftEnter => m.shift,
                };
                if wanted {
                    let (lo, hi) = self.selection();
                    self.replace(lo, hi, &['\n'], Edit::Other);
                    return true;
                }
                false
            }
            Key::A if m.command && !m.alt => {
                *self.anchor = 0;
                *self.pos = self.frame.layout.pos(self.chars.len());
                true
            }
            Key::Z if m.command && !m.alt => {
                self.undo(m.shift);
                true
            }
            Key::Y if m.command && !m.alt => {
                self.undo(true);
                true
            }
            _ => false,
        }
    }
}
