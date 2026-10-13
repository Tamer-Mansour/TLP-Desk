//! Chat (TLP-chat): the side panel of a viewer session, the host user's chat window, message
//! bubbles and the message box.
//!
//! Arabic is first-class here:
//! * every message gets its own paragraph direction from its first strong character
//!   ([`message_is_rtl`]), so an Arabic message is right-aligned in its bubble even in the
//!   English UI, and an English one left-aligned in the Arabic UI;
//! * which side a bubble sits on follows the UI direction (mine at the reading end);
//! * long messages are wrapped in *logical* order ([`wrap_ranges`]) and every line is shaped
//!   on its own, because egui would wrap pre-shaped RTL text in visual order (see
//!   `i18n::shaping`);
//! * the message box uses the address-box approach (raw text while editing, shaped text and a
//!   preview otherwise).

use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;

use eframe::egui::{
    self, text::LayoutJob, Align, Color32, Galley, Id, Key, KeyboardShortcut, Modifiers, Pos2,
    Rect, Response, RichText, Sense, TextBuffer, Ui,
};
use unicode_bidi::Direction;

use super::icons::{self, Icon};
use super::theme;
use crate::backend::chat::{max_chat_chars, ChatLine, ChatSendError};
use crate::backend::{Backend, SessionId};
use crate::i18n::{has_rtl, shape_for_display, shape_for_edit, BaseDir, Tx, S};

/// Messages kept per conversation in the UI (oldest dropped first).
pub const MAX_LINES: usize = 500;
/// Inner padding of a bubble, in points.
const PAD: f32 = 10.0;
/// Space between bubbles.
const GAP: f32 = 6.0;
/// A bubble is at most this share of the conversation width.
const BUBBLE_SHARE: f32 = 0.8;

/// One message plus what the UI derived from it.
#[derive(Debug, Clone)]
struct ChatItem {
    line: ChatLine,
    /// Control characters removed (tabs become spaces); the text is otherwise untouched.
    text: String,
    /// Local wall-clock time (hour, minute) when it was sent or received.
    hm: (u8, u8),
    /// Logical-order line ranges for the last text width (wrapping is expensive for long
    /// messages, so it is done once per width).
    wrap: Option<(u32, Vec<Range<usize>>)>,
}

/// The conversation of one session and its message box.
#[derive(Debug, Clone)]
pub struct ChatState {
    items: VecDeque<ChatItem>,
    /// The message being written (logical order).
    pub draft: String,
    /// Panel (viewer) or window (host) shown.
    open: bool,
    unread: usize,
    /// Chat currently allowed by the host.
    allowed: bool,
    error: Option<ChatSendError>,
}

impl ChatState {
    /// An empty, closed conversation.
    pub fn new(allowed: bool) -> Self {
        Self {
            items: VecDeque::new(),
            draft: String::new(),
            open: false,
            unread: 0,
            allowed,
            error: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Shows or hides the conversation; showing it marks everything read.
    pub fn set_open(&mut self, open: bool) {
        self.open = open;
        if open {
            self.unread = 0;
        }
    }

    /// Messages from the peer that arrived while the conversation was hidden.
    pub fn unread(&self) -> usize {
        self.unread
    }

    pub fn allowed(&self) -> bool {
        self.allowed
    }

    pub fn set_allowed(&mut self, allowed: bool) {
        self.allowed = allowed;
        if allowed && self.error == Some(ChatSendError::NotAllowed) {
            self.error = None;
        }
    }

    /// The messages, oldest first.
    pub fn lines(&self) -> impl Iterator<Item = &ChatLine> {
        self.items.iter().map(|i| &i.line)
    }

    /// Adds a sent or received message.
    pub fn push(&mut self, line: ChatLine) {
        if !line.mine && !self.open {
            self.unread = self.unread.saturating_add(1);
        }
        self.items.push_back(ChatItem {
            text: display_text(&line.text),
            hm: local_hm(line.at_unix_ms),
            line,
            wrap: None,
        });
        while self.items.len() > MAX_LINES {
            self.items.pop_front();
        }
    }

    /// Sends the draft; on success the message is added and the box cleared, otherwise the
    /// draft stays and the reason is shown.
    pub fn send_draft(&mut self, backend: &dyn Backend, session: SessionId) {
        if self.draft.trim().is_empty() {
            return;
        }
        match backend.send_chat(session, &self.draft) {
            Ok(line) => {
                self.draft.clear();
                self.error = None;
                self.push(line);
            }
            Err(e) => self.error = Some(e),
        }
    }
}

/// Removes control characters a peer could use to garble the display (tabs become spaces,
/// `\n` stays). Bidi marks and joiners are not control characters and stay.
pub fn display_text(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\n' => Some('\n'),
            '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect()
}

/// Whether a message reads right to left: from its first strong character (Unicode bidi rules
/// P2/P3); text without letters (digits, emoji) follows the UI direction.
pub fn message_is_rtl(text: &str, ui_rtl: bool) -> bool {
    match unicode_bidi::get_base_direction_full(text) {
        Direction::Rtl => true,
        Direction::Ltr => false,
        Direction::Mixed => ui_rtl,
    }
}

fn base_dir(rtl: bool) -> BaseDir {
    if rtl {
        BaseDir::Rtl
    } else {
        BaseDir::Ltr
    }
}

/// Local (hour, minute) of a Unix time in milliseconds.
fn local_hm(unix_ms: u64) -> (u8, u8) {
    let offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    let secs = i64::try_from(unix_ms / 1000).unwrap_or(0);
    match time::OffsetDateTime::from_unix_timestamp(secs) {
        Ok(t) => {
            let t = t.to_offset(offset);
            (t.hour(), t.minute())
        }
        Err(_) => (0, 0),
    }
}

/// "3:07 PM" / "٣:٠٧ م" (12-hour clock, the user's digit style), display-ready.
pub fn clock_text(tx: Tx, (hour, minute): (u8, u8)) -> String {
    let half = if hour < 12 { S::TimeAm } else { S::TimePm };
    let h12 = match hour % 12 {
        0 => 12,
        h => h,
    };
    let digits = tx.digits(&format!("{h12}:{minute:02}"));
    tx.p(&format!("{digits} {}", tx.raw(half)))
}

/// Splits `text` into lines no wider than `max_width` (as measured by `width`), in logical
/// order: at `\n`, else at the last space that fits, else (a word longer than a line) between
/// characters. Returns byte ranges into `text`; the breaking space or `\n` is not part of any
/// line.
pub fn wrap_ranges(
    text: &str,
    max_width: f32,
    width: &mut dyn FnMut(&str) -> f32,
) -> Vec<Range<usize>> {
    let max_width = if max_width.is_nan() {
        0.0
    } else {
        max_width.max(0.0)
    };
    let mut out = Vec::new();
    let mut base = 0;
    for para in text.split('\n') {
        wrap_paragraph(para, base, max_width, width, &mut out);
        base += para.len() + 1;
    }
    out
}

fn wrap_paragraph(
    p: &str,
    base: usize,
    max: f32,
    width: &mut dyn FnMut(&str) -> f32,
    out: &mut Vec<Range<usize>>,
) {
    if p.is_empty() || width(p) <= max {
        out.push(base..base + p.len());
        return;
    }
    let mut start = 0;
    let mut idx = 0;
    let mut fit: Option<usize> = None;
    while start < p.len() {
        let next_space = p
            .get(idx..)
            .and_then(|rest| rest.find(' '))
            .map(|i| idx + i);
        let end = next_space.unwrap_or(p.len());
        let candidate = p.get(start..end).unwrap_or("");
        if width(candidate) <= max {
            match next_space {
                Some(space) => {
                    fit = Some(end);
                    idx = space + 1;
                }
                None => {
                    out.push(base + start..base + end);
                    return;
                }
            }
        } else if let Some(f) = fit.take() {
            out.push(base + start..base + f);
            // Skip the space we broke at.
            start = f + 1;
            idx = start;
        } else {
            let cut = char_fit(candidate, max, width);
            if cut == 0 {
                // Only an empty candidate (a space at `start`) gets here: skip that space.
                start = end + 1;
                idx = start;
                continue;
            }
            out.push(base + start..base + start + cut);
            start += cut;
            idx = start;
        }
    }
}

/// Byte length of the longest prefix of `s` that fits (at least one character).
fn char_fit(s: &str, max: f32, width: &mut dyn FnMut(&str) -> f32) -> usize {
    let bounds: Vec<usize> = s.char_indices().map(|(i, c)| i + c.len_utf8()).collect();
    let (mut lo, mut hi) = (0usize, bounds.len());
    // Largest k with prefix of k chars fitting; binary search keeps long words cheap.
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let end = bounds.get(mid.saturating_sub(1)).copied().unwrap_or(0);
        if width(s.get(..end).unwrap_or("")) <= max {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    bounds.get(lo.max(1) - 1).copied().unwrap_or(s.len())
}

/// Where a bubble and its text go, relative to the top-left corner of its row.
#[derive(Clone)]
pub struct Bubble {
    pub rect: Rect,
    /// The message reads right to left.
    pub rtl: bool,
    /// One galley per wrapped line, with its top-left position.
    pub lines: Vec<(Pos2, Arc<Galley>)>,
    pub time: (Pos2, Arc<Galley>),
    /// Height of the whole row.
    pub height: f32,
}

fn bubble_colors(ui: &Ui, mine: bool) -> (Color32, Color32, Color32) {
    let dark = ui.visuals().dark_mode;
    if mine {
        (
            theme::accent(dark),
            Color32::WHITE,
            Color32::from_white_alpha(210),
        )
    } else {
        let fill = if dark {
            Color32::from_rgb(0x2A, 0x2F, 0x37)
        } else {
            Color32::from_rgb(0xE6, 0xEA, 0xEF)
        };
        (
            fill,
            ui.visuals().text_color(),
            ui.visuals().weak_text_color(),
        )
    }
}

/// Lays out one message for a row `row_width` wide.
fn layout_bubble(ui: &Ui, tx: Tx, item: &mut ChatItem, row_width: f32) -> Bubble {
    let (_, text_color, time_color) = bubble_colors(ui, item.line.mine);
    let body = egui::TextStyle::Body.resolve(ui.style());
    let small = egui::TextStyle::Small.resolve(ui.style());
    let rtl = message_is_rtl(&item.text, tx.rtl());
    let max_text = (row_width * BUBBLE_SHARE - 2.0 * PAD).max(40.0);
    let key = max_text.round() as u32;
    if item.wrap.as_ref().is_none_or(|(w, _)| *w != key) {
        let painter = ui.painter();
        let mut measure = |s: &str| {
            painter
                .layout_no_wrap(s.to_owned(), body.clone(), text_color)
                .size()
                .x
        };
        item.wrap = Some((key, wrap_ranges(&item.text, max_text, &mut measure)));
    }
    let ranges = item.wrap.as_ref().map(|(_, r)| r.as_slice()).unwrap_or(&[]);
    let galleys: Vec<Arc<Galley>> = ranges
        .iter()
        .map(|r| {
            let line = item.text.get(r.clone()).unwrap_or("");
            let shown = shape_for_display(line, base_dir(rtl)).into_owned();
            ui.painter().layout_no_wrap(shown, body.clone(), text_color)
        })
        .collect();
    let time = ui
        .painter()
        .layout_no_wrap(clock_text(tx, item.hm), small, time_color);

    let text_w = galleys.iter().map(|g| g.size().x).fold(0.0, f32::max);
    let content_w = text_w.max(time.size().x);
    let text_h: f32 = galleys.iter().map(|g| g.size().y).sum();
    let size = egui::vec2(
        content_w + 2.0 * PAD,
        text_h + 2.0 + time.size().y + 2.0 * PAD,
    );
    // Mine at the reading end of the UI (right in English, left in Arabic).
    let at_right = item.line.mine != tx.rtl();
    let x = if at_right {
        (row_width - size.x).max(0.0)
    } else {
        0.0
    };
    let rect = Rect::from_min_size(egui::pos2(x, 0.0), size);

    let mut y = rect.min.y + PAD;
    let lines = galleys
        .into_iter()
        .map(|g| {
            let gx = if rtl {
                rect.max.x - PAD - g.size().x
            } else {
                rect.min.x + PAD
            };
            let pos = egui::pos2(gx, y);
            y += g.size().y;
            (pos, g)
        })
        .collect();
    // The time sits at the end of the message's reading direction.
    let time_x = if rtl {
        rect.min.x + PAD
    } else {
        rect.max.x - PAD - time.size().x
    };
    Bubble {
        rect,
        rtl,
        lines,
        time: (egui::pos2(time_x, y + 2.0), time),
        height: size.y,
    }
}

/// Draws one message; right-click copies its text.
fn bubble_ui(ui: &mut Ui, tx: Tx, item: &mut ChatItem) {
    let row_width = ui.available_width();
    let b = layout_bubble(ui, tx, item, row_width);
    let (row, _) = ui.allocate_exact_size(egui::vec2(row_width, b.height), Sense::hover());
    let offset = row.min.to_vec2();
    let rect = b.rect.translate(offset);
    let (fill, text_color, _) = bubble_colors(ui, item.line.mine);
    let resp = ui.interact(
        rect,
        Id::new(("chat-bubble", item.line.mine, item.line.id)),
        Sense::click(),
    );
    let painter = ui.painter();
    painter.rect_filled(rect, 12.0, fill);
    for (pos, g) in b.lines {
        painter.galley(pos + offset, g, text_color);
    }
    painter.galley(b.time.0 + offset, b.time.1, text_color);
    let raw = item.line.text.clone();
    resp.context_menu(|ui| {
        if ui
            .add(icons::icon_text_button(tx, Icon::Copy, tx.t(S::Copy)))
            .clicked()
        {
            ui.ctx().copy_text(raw.clone());
            ui.close();
        }
    });
    ui.add_space(GAP);
}

/// Multi-line message box: Enter sends, Shift+Enter adds a line. Returns `true` when the user
/// asked to send.
fn message_box(ui: &mut Ui, tx: Tx, chat: &mut ChatState, id: Id, width: f32) -> (Response, bool) {
    let focused = ui.memory(|m| m.has_focus(id));
    let rtl = message_is_rtl(&chat.draft, tx.rtl());
    let mut layouter = |ui: &Ui, buf: &dyn TextBuffer, wrap_width: f32| -> Arc<Galley> {
        let raw = buf.as_str();
        let font = egui::TextStyle::Body.resolve(ui.style());
        // Not editing: show it shaped, wrapped in logical order by turning the breaking spaces
        // into line breaks (same character count, so egui's cursor indices stay valid).
        let shown = (!focused).then(|| {
            let mut measure = |s: &str| {
                ui.painter()
                    .layout_no_wrap(s.to_owned(), font.clone(), Color32::WHITE)
                    .size()
                    .x
            };
            let mut wrapped = raw.to_owned();
            for r in wrap_ranges(raw, wrap_width.max(20.0), &mut measure) {
                if raw.get(r.end..).is_some_and(|rest| rest.starts_with(' ')) {
                    wrapped.replace_range(r.end..r.end + 1, "\n");
                }
            }
            shape_for_edit(&wrapped, base_dir(rtl))
        });
        let mut job = LayoutJob::simple(
            shown.flatten().unwrap_or_else(|| raw.to_owned()),
            font,
            ui.visuals().text_color(),
            wrap_width,
        );
        job.halign = Align::LEFT;
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let resp = ui.add(
        egui::TextEdit::multiline(&mut chat.draft)
            .id(id)
            .hint_text(tx.t(S::ChatInputHint))
            .desired_rows(2)
            .desired_width(width)
            .return_key(KeyboardShortcut::new(Modifiers::SHIFT, Key::Enter))
            .horizontal_align(if rtl { Align::Max } else { Align::Min })
            .interactive(chat.allowed)
            .layouter(&mut layouter),
    );
    // Plain Enter (Shift+Enter is the newline above; Ctrl/Alt+Enter do nothing).
    let enter = resp.has_focus()
        && ui.input(|i| {
            i.events.iter().any(|e| {
                matches!(e, egui::Event::Key { key: Key::Enter, pressed: true, modifiers, .. }
                    if !modifiers.shift && !modifiers.ctrl && !modifiers.alt)
            })
        });
    (resp, enter)
}

/// The conversation (messages above, message box below) for `session`.
pub fn conversation_ui(
    ui: &mut Ui,
    tx: Tx,
    chat: &mut ChatState,
    backend: &dyn Backend,
    session: SessionId,
    off_text: S,
) {
    let input_id = Id::new(("chat-input", session.0));
    let reserved = 150.0;
    let scroll_h = (ui.available_height() - reserved).max(140.0);
    egui::ScrollArea::vertical()
        .id_salt(("chat-scroll", session.0))
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .max_height(scroll_h)
        .show(ui, |ui| {
            if chat.items.is_empty() {
                theme::column(ui, tx.rtl(), |ui| {
                    ui.add_space(8.0);
                    ui.label(theme::caption(ui, tx.t(S::ChatEmpty)));
                });
            }
            for item in chat.items.iter_mut() {
                bubble_ui(ui, tx, item);
            }
        });
    ui.separator();
    if !chat.allowed {
        theme::row(ui, tx.rtl(), |ui| {
            let dark = ui.visuals().dark_mode;
            icons::icon(ui, tx, Icon::Lock, icons::BUTTON_ICON, theme::danger(dark));
            ui.label(theme::caption(ui, tx.t(off_text)));
        });
    }
    let mut send = false;
    theme::row(ui, tx.rtl(), |ui| {
        let field_w = (ui.available_width() - 48.0).max(80.0);
        let (resp, enter) = message_box(ui, tx, chat, input_id, field_w);
        let chars = chat.draft.chars().count();
        let ok = chat.allowed && !chat.draft.trim().is_empty() && chars <= max_chat_chars();
        let button = ui.add_enabled_ui(ok, |ui| {
            icons::icon_button(ui, tx, Icon::Send, tx.t(S::ChatSend))
        });
        if (enter && ok) || button.inner.clicked() {
            send = true;
            resp.request_focus();
        }
    });
    if send {
        chat.send_draft(backend, session);
    }
    status_line(ui, tx, chat, input_id);
}

/// Preview of an Arabic draft, length counter, and why the last send failed.
fn status_line(ui: &mut Ui, tx: Tx, chat: &ChatState, input_id: Id) {
    let focused = ui.memory(|m| m.has_focus(input_id));
    let max = max_chat_chars();
    let chars = chat.draft.chars().count();
    theme::column(ui, tx.rtl(), |ui| {
        if focused && has_rtl(&chat.draft) {
            preview_ui(ui, tx, &chat.draft);
        }
        if chars > max * 9 / 10 {
            let count = tx.digits(&chars.to_string());
            let limit = tx.digits(&max.to_string());
            let text = tx.f(S::ChatCount, &[("count", &count), ("max", &limit)]);
            let dark = ui.visuals().dark_mode;
            let color = if chars > max {
                theme::danger(dark)
            } else {
                ui.visuals().weak_text_color()
            };
            ui.label(RichText::new(text).small().color(color));
        }
        let error = match chat.error {
            Some(ChatSendError::TooLong { max }) => {
                Some(tx.f(S::ChatTooLong, &[("max", &tx.digits(&max.to_string()))]))
            }
            Some(ChatSendError::RateLimited) => Some(tx.t(S::ChatRateLimited).to_owned()),
            Some(ChatSendError::Unavailable) => Some(tx.t(S::ChatUnavailable).to_owned()),
            Some(ChatSendError::NotAllowed) if chat.allowed => {
                Some(tx.t(S::ChatOffHost).to_owned())
            }
            _ => None,
        };
        if let Some(e) = error {
            let dark = ui.visuals().dark_mode;
            ui.label(RichText::new(e).small().color(theme::danger(dark)));
        } else {
            ui.label(theme::caption(ui, tx.t(S::ChatSendHint)));
        }
    });
}

/// The draft as it will look, wrapped in logical order and shaped line by line.
fn preview_ui(ui: &mut Ui, tx: Tx, draft: &str) {
    let text = display_text(draft);
    let rtl = message_is_rtl(&text, tx.rtl());
    let font = egui::TextStyle::Small.resolve(ui.style());
    let color = ui.visuals().weak_text_color();
    let max = ui.available_width().max(40.0);
    let mut measure = |s: &str| {
        ui.painter()
            .layout_no_wrap(s.to_owned(), font.clone(), color)
            .size()
            .x
    };
    let ranges = wrap_ranges(&text, max, &mut measure);
    let layout = if rtl {
        egui::Layout::top_down(Align::Max)
    } else {
        egui::Layout::top_down(Align::Min)
    };
    ui.with_layout(layout, |ui| {
        // At most a few lines; the box itself shows the rest.
        for r in ranges.iter().rev().take(4).rev() {
            let line = text.get(r.clone()).unwrap_or("");
            ui.label(
                RichText::new(shape_for_display(line, base_dir(rtl)))
                    .small()
                    .color(color),
            );
        }
    });
}

/// Toolbar toggle with the chat icon and a badge counting unread messages. Returns `true`
/// when clicked.
pub fn chat_toggle(ui: &mut Ui, tx: Tx, chat: &ChatState) -> bool {
    let unread = chat.unread();
    let tip = if unread > 0 {
        format!(
            "{}\n{}",
            tx.t(S::Chat),
            tx.f(S::ChatUnread, &[("count", &tx.digits(&unread.to_string()))])
        )
    } else {
        tx.t(S::Chat).to_owned()
    };
    let resp = icons::icon_toggle(ui, tx, chat.is_open(), Icon::Chat, &tip);
    if unread > 0 {
        badge(ui, tx, resp.rect, unread);
    }
    resp.clicked()
}

/// Small red count at the top reading-end corner of `rect`.
fn badge(ui: &Ui, tx: Tx, rect: Rect, count: usize) {
    let dark = ui.visuals().dark_mode;
    let label = if count > 99 {
        "99+".to_owned()
    } else {
        count.to_string()
    };
    let galley = ui.painter().layout_no_wrap(
        tx.n(&label),
        egui::FontId::proportional(10.5),
        Color32::WHITE,
    );
    let w = (galley.size().x + 8.0).max(16.0);
    let center_x = if tx.rtl() {
        rect.min.x + 4.0
    } else {
        rect.max.x - 4.0
    };
    let badge = Rect::from_center_size(egui::pos2(center_x, rect.min.y + 4.0), egui::vec2(w, 16.0));
    let painter = ui.painter();
    painter.rect_filled(badge, 8.0, theme::danger(dark));
    painter.galley(badge.center() - galley.size() / 2.0, galley, Color32::WHITE);
}

/// The chat side panel of a viewer session (at the UI's reading end).
pub fn side_panel(
    ui: &mut Ui,
    tx: Tx,
    chat: &mut ChatState,
    backend: &dyn Backend,
    session: SessionId,
) {
    if !chat.is_open() {
        return;
    }
    let panel = if tx.rtl() {
        egui::Panel::left("chat-panel")
    } else {
        egui::Panel::right("chat-panel")
    };
    panel
        .resizable(true)
        .default_size(320.0)
        .size_range(240.0..=560.0)
        .frame(
            egui::Frame::new()
                .fill(ui.visuals().window_fill)
                .inner_margin(egui::Margin::same(12)),
        )
        .show(ui, |ui| {
            let mut close = false;
            theme::row(ui, tx.rtl(), |ui| {
                let accent = theme::accent(ui.visuals().dark_mode);
                icons::icon(ui, tx, Icon::Chat, 18.0, accent);
                ui.label(RichText::new(tx.t(S::Chat)).strong());
                theme::row_end(ui, tx.rtl(), |ui| {
                    if icons::icon_button(ui, tx, Icon::Close, tx.t(S::Cancel)).clicked() {
                        close = true;
                    }
                });
            });
            conversation_ui(ui, tx, chat, backend, session, S::ChatOffViewer);
            if close {
                chat.set_open(false);
            }
        });
}

/// The host user's chat window for one connected viewer: opens when the viewer writes (or
/// from the banner), lets the host user reply and turn chat off for the session.
pub fn host_window(
    ctx: &egui::Context,
    tx: Tx,
    chat: &mut ChatState,
    peer_name: &str,
    backend: &dyn Backend,
    session: SessionId,
) {
    if !chat.is_open() {
        return;
    }
    let mut open = true;
    let title = tx.f(S::ChatWith, &[("name", peer_name)]);
    egui::Window::new(title)
        .id(Id::new(("host-chat", session.0)))
        .open(&mut open)
        .default_size([360.0, 440.0])
        .min_width(280.0)
        .collapsible(true)
        .show(ctx, |ui| {
            theme::row(ui, tx.rtl(), |ui| {
                let (icon, label) = if chat.allowed() {
                    (Icon::Lock, S::ChatTurnOff)
                } else {
                    (Icon::Unlock, S::ChatTurnOn)
                };
                if ui
                    .add(icons::icon_text_button(tx, icon, tx.t(label)))
                    .clicked()
                {
                    backend.set_chat_allowed(session, !chat.allowed());
                }
            });
            conversation_ui(ui, tx, chat, backend, session, S::ChatOffHost);
        });
    if !open {
        chat.set_open(false);
    }
}

/// Flash the taskbar button when a message arrives while the window is in the background
/// (no sound, no focus stealing).
pub fn notify_if_unfocused(ctx: &egui::Context) {
    let focused = ctx.input(|i| i.viewport().focused).unwrap_or(false);
    if !focused {
        ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
            egui::UserAttentionType::Informational,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{DigitStyle, Lang};

    fn line(id: u64, mine: bool, text: &str) -> ChatLine {
        ChatLine {
            id,
            mine,
            text: text.into(),
            at_unix_ms: 1_700_000_000_000,
        }
    }

    /// Character count as width: deterministic wrapping tests.
    fn chars(s: &str) -> f32 {
        s.chars().count() as f32
    }

    fn wrap(text: &str, max: f32) -> Vec<&str> {
        wrap_ranges(text, max, &mut chars)
            .into_iter()
            .map(|r| &text[r])
            .collect()
    }

    /// Backend that records the chat messages the UI sends.
    #[derive(Default)]
    struct ChatRecorder(std::sync::Mutex<Vec<String>>);

    impl Backend for ChatRecorder {
        fn local_info(&self) -> crate::backend::LocalInfo {
            crate::backend::LocalInfo {
                device_id: crate::backend::DeviceId(1),
                device_name: String::new(),
                addresses: vec![],
                one_time_code: String::new(),
            }
        }
        fn set_device_name(&self, _: String) {}
        fn refresh_one_time_code(&self) {}
        fn connect(
            &self,
            _: crate::backend::ConnectTarget,
            _: Option<String>,
        ) -> crate::backend::AttemptId {
            crate::backend::AttemptId(0)
        }
        fn cancel_connect(&self, _: crate::backend::AttemptId) {}
        fn forget_host_key(&self, _: crate::backend::AttemptId) -> bool {
            false
        }
        fn respond_to_request(&self, _: crate::backend::RequestId, _: bool) {}
        fn send_input(&self, _: SessionId, _: tlp_desk_proto::InputEvent) {}
        fn set_view_only(&self, _: SessionId, _: bool) {}
        fn disconnect(&self, _: SessionId) {}
        fn select_monitor(&self, _: SessionId, _: u32) {}
        fn send_chat(&self, _: SessionId, text: &str) -> Result<ChatLine, ChatSendError> {
            self.0.lock().unwrap().push(text.to_owned());
            Ok(line(1, true, text))
        }
    }

    fn key_enter(shift: bool) -> egui::Event {
        egui::Event::Key {
            key: Key::Enter,
            physical_key: Some(Key::Enter),
            pressed: true,
            repeat: false,
            modifiers: if shift {
                Modifiers::SHIFT
            } else {
                Modifiers::NONE
            },
        }
    }

    #[test]
    fn enter_sends_and_shift_enter_adds_a_line() {
        for lang in [Lang::En, Lang::Ar] {
            let ctx = egui::Context::default();
            theme::install(&ctx);
            let tx = Tx::new(lang, DigitStyle::Western);
            let backend = ChatRecorder::default();
            let mut chat = ChatState::new(true);
            chat.set_open(true);
            let session = SessionId(7);
            let run = |chat: &mut ChatState, events: Vec<egui::Event>| {
                let input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(360.0, 600.0))),
                    events,
                    ..Default::default()
                };
                let mut out = ctx.run_ui(input, |ui| {
                    conversation_ui(ui, tx, chat, &backend, session, S::ChatOffViewer)
                });
                out.textures_delta.clear();
            };
            run(&mut chat, vec![]);
            ctx.memory_mut(|m| m.request_focus(Id::new(("chat-input", session.0))));
            run(&mut chat, vec![]);
            run(&mut chat, vec![egui::Event::Text("مرحبا".into())]);
            run(&mut chat, vec![key_enter(true)]);
            assert_eq!(chat.draft, "مرحبا\n", "{lang:?}");
            assert!(backend.0.lock().unwrap().is_empty());
            run(&mut chat, vec![egui::Event::Text("world 👍".into())]);
            run(&mut chat, vec![key_enter(false)]);
            assert_eq!(
                *backend.0.lock().unwrap(),
                vec!["مرحبا\nworld 👍"],
                "{lang:?}"
            );
            assert!(chat.draft.is_empty());
            assert_eq!(chat.lines().count(), 1);
            // The bubbles render (and a blank draft is never sent).
            run(&mut chat, vec![key_enter(false)]);
            assert_eq!(backend.0.lock().unwrap().len(), 1);
        }
    }

    #[test]
    fn chat_off_disables_sending() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let tx = Tx::new(Lang::Ar, DigitStyle::ArabicIndic);
        let backend = ChatRecorder::default();
        let mut chat = ChatState::new(false);
        chat.draft = "رسالة".into();
        let session = SessionId(3);
        for events in [vec![], vec![key_enter(false)]] {
            ctx.memory_mut(|m| m.request_focus(Id::new(("chat-input", session.0))));
            let mut out = ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| conversation_ui(ui, tx, &mut chat, &backend, session, S::ChatOffHost),
            );
            out.textures_delta.clear();
        }
        assert!(backend.0.lock().unwrap().is_empty());
        assert_eq!(chat.draft, "رسالة");
    }

    #[test]
    fn direction_comes_from_the_first_strong_character() {
        assert!(message_is_rtl("مرحبا PC-01", false));
        assert!(!message_is_rtl("PC-01 مرحبا", true));
        assert!(message_is_rtl("123 مرحبا", false));
        // No letters: follow the UI.
        assert!(message_is_rtl("👍 123", true));
        assert!(!message_is_rtl("👍 123", false));
        // First paragraph neutral: the next one decides.
        assert!(message_is_rtl("...\nمرحبا", false));
    }

    #[test]
    fn wrapping_is_logical_and_bounded() {
        assert_eq!(wrap("short", 10.0), vec!["short"]);
        assert_eq!(
            wrap("واحد اثنان ثلاثة أربعة", 11.0),
            vec!["واحد اثنان", "ثلاثة أربعة"]
        );
        assert_eq!(wrap("a\n\nb", 10.0), vec!["a", "", "b"]);
        // A word longer than a line is split between characters, never lost.
        assert_eq!(wrap("abcdefghij kl", 4.0), vec!["abcd", "efgh", "ij", "kl"]);
        let long = "كلمة ".repeat(200);
        for max in [1.0, 3.0, 17.0, 60.0] {
            let lines = wrap(&long, max);
            assert!(lines.iter().all(|l| chars(l) <= max.max(1.0)), "{max}");
            let joined: String = lines.concat();
            assert_eq!(joined.replace(' ', ""), long.replace(' ', ""));
        }
        // Even a width that fits nothing makes progress one character at a time.
        assert_eq!(wrap("عرب", 0.0), vec!["ع", "ر", "ب"]);
    }

    #[test]
    fn control_characters_are_hidden_but_marks_kept() {
        let t = display_text("a\u{0}b\tc\r\nد\u{200F}\u{200D}\u{200C}\u{1b}[31m");
        assert_eq!(t, "ab c\nد\u{200F}\u{200D}\u{200C}[31m");
    }

    #[test]
    fn clock_follows_language_and_digits() {
        let en = Tx::new(Lang::En, DigitStyle::Western);
        assert_eq!(clock_text(en, (15, 7)), "3:07 PM");
        assert_eq!(clock_text(en, (0, 0)), "12:00 AM");
        let ar = Tx::new(Lang::Ar, DigitStyle::ArabicIndic);
        let shown = clock_text(ar, (15, 7));
        for c in ['٣', '٠', '٧', ':', 'م'] {
            assert!(shown.contains(c), "{shown:?}");
        }
        assert!(!shown.chars().any(|c| c.is_ascii_digit()), "{shown:?}");
    }

    #[test]
    fn unread_counts_only_peer_messages_while_closed() {
        let mut c = ChatState::new(true);
        c.push(line(1, false, "مرحبا"));
        c.push(line(1, true, "أهلاً"));
        assert_eq!(c.unread(), 1);
        c.set_open(true);
        assert_eq!(c.unread(), 0);
        c.push(line(2, false, "hi"));
        assert_eq!(c.unread(), 0);
        for i in 0..(MAX_LINES as u64 + 10) {
            c.push(line(i + 10, false, "x"));
        }
        assert_eq!(c.lines().count(), MAX_LINES);
    }

    /// Lays out `item` in a real egui context with the app's fonts.
    fn bubble_for(lang: Lang, item: ChatLine, width: f32) -> (Bubble, Vec<Vec<char>>) {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        theme::apply_lang(&ctx, lang);
        let tx = Tx::new(lang, DigitStyle::Western);
        let mut out = None;
        // Fonts are ready from the second frame on.
        for _ in 0..2 {
            let mut item = ChatItem {
                text: display_text(&item.text),
                hm: (9, 30),
                line: item.clone(),
                wrap: None,
            };
            let mut o = ctx.run_ui(egui::RawInput::default(), |ui| {
                out = Some(layout_bubble(ui, tx, &mut item, width));
            });
            o.textures_delta.clear();
        }
        let bubble = out.expect("laid out");
        // Visible glyphs of each line, left to right on screen.
        let order = bubble
            .lines
            .iter()
            .map(|(_, g)| {
                let mut glyphs: Vec<(f32, char)> = g
                    .rows
                    .iter()
                    .flat_map(|r| r.row.glyphs.iter())
                    .filter(|gl| gl.advance_width > 0.0)
                    .map(|gl| (gl.pos.x, gl.chr))
                    .collect();
                glyphs.sort_by(|a, b| a.0.total_cmp(&b.0));
                glyphs.into_iter().map(|(_, c)| c).collect()
            })
            .collect();
        (bubble, order)
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.5
    }

    #[test]
    fn bubble_side_follows_ui_and_text_alignment_follows_message() {
        let w = 400.0;
        for (lang, mine, text, at_right, rtl) in [
            // English UI: mine on the right, theirs on the left.
            (Lang::En, true, "Hello there", true, false),
            (Lang::En, false, "Hello there", false, false),
            // Arabic message in the English UI: still right-aligned text.
            (Lang::En, false, "مرحباً بك", false, true),
            // Arabic UI: mirrored sides; English message stays left-aligned text.
            (Lang::Ar, true, "مرحباً بك", false, true),
            (Lang::Ar, false, "Hello there", true, false),
            // Shorter than the time below them: the alignment is visible.
            (Lang::En, false, "نعم", false, true),
            (Lang::Ar, true, "OK", false, false),
        ] {
            let (b, _) = bubble_for(lang, line(1, mine, text), w);
            let what = format!("{lang:?} mine={mine} {text}");
            assert_eq!(b.rtl, rtl, "{what}");
            if at_right {
                assert!(near(b.rect.max.x, w), "{what}: {:?}", b.rect);
            } else {
                assert!(near(b.rect.min.x, 0.0), "{what}: {:?}", b.rect);
            }
            let short = text.chars().count() <= 3;
            for (pos, g) in &b.lines {
                if rtl {
                    assert!(near(pos.x + g.size().x, b.rect.max.x - PAD), "{what}");
                    assert!(!short || pos.x > b.rect.min.x + PAD + 1.0, "{what}");
                } else {
                    assert!(near(pos.x, b.rect.min.x + PAD), "{what}");
                    let end = pos.x + g.size().x;
                    assert!(!short || end < b.rect.max.x - PAD - 1.0, "{what}");
                }
            }
            // The time is inside the bubble, below the text.
            let (tpos, tg) = &b.time;
            assert!(b.rect.contains(*tpos) && tpos.x + tg.size().x <= b.rect.max.x + 0.5);
        }
    }

    #[test]
    fn arabic_bubble_text_reads_right_to_left_on_screen() {
        let text = "مرحبا بالعالم";
        let (b, order) = bubble_for(Lang::En, line(1, false, text), 600.0);
        assert_eq!(b.lines.len(), 1);
        let expected: Vec<char> = text.chars().rev().collect();
        assert_eq!(order[0], expected);
        // Mixed: the Latin name and number keep their own order inside the Arabic sentence.
        let (_, order) = bubble_for(
            Lang::En,
            line(2, false, "افتح report-2026.pdf من فضلك"),
            600.0,
        );
        let shown: String = order[0].iter().collect();
        assert!(shown.contains("report-2026.pdf"), "{shown:?}");
        // The last word is visually leftmost.
        assert!(shown.starts_with("كلضف"), "{shown:?}");
    }

    #[test]
    fn long_arabic_message_wraps_into_bounded_lines_in_reading_order() {
        let text = "هذه رسالة طويلة جداً تكتب بالعربية لاختبار التفاف الأسطر داخل الفقاعة دون أن يضيع ترتيب الكلمات";
        let w = 260.0;
        let (b, order) = bubble_for(Lang::Ar, line(1, true, text), w);
        assert!(b.lines.len() > 1, "{}", b.lines.len());
        assert!(b.rect.width() <= w * BUBBLE_SHARE + 0.5);
        // The first line on screen holds the first words: its rightmost glyph is the
        // message's first letter.
        assert_eq!(order[0].last(), text.chars().next().as_ref());
        assert_eq!(
            order.last().and_then(|l| l.first()),
            text.chars().last().as_ref()
        );
    }
}
