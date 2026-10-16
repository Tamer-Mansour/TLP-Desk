//! Arabic display for egui: bidi ordering on top of egui's own shaping.
//!
//! **What egui (epaint 0.36) does:** it splits text into runs by font face and shapes every run
//! with `harfrust` (a HarfBuzz port). Arabic runs are shaped properly — contextual forms, lam-alef
//! and other ligatures, GPOS mark positioning for harakat — and emitted right-to-left *within
//! the run*. But it has no bidi algorithm: the runs themselves are always placed left-to-right
//! in string order, so in "هذا الجهاز" each word is fine but the word order is wrong, and digits
//! or Latin names inside Arabic sentences end up in the wrong place.
//!
//! **What we add:**
//! 1. [`unicode_bidi`] resolves embedding levels (UAX #9) for the paragraph and we compute the
//!    *visual* character order (right-to-left runs reversed by cluster, brackets mirrored).
//! 2. Because egui's shaper will reverse every run that uses the Arabic font face again, we
//!    pre-reverse exactly those runs (by cluster), i.e. hand egui each Arabic run in logical
//!    order (which is also what the shaper needs) while all runs are in visual order.
//!
//! The result is a permutation of the input characters (no characters are added or removed,
//! apart from bidi control characters), which also keeps `TextEdit` cursor indices in range.
//! `tests::egui_*` lay text out with the real epaint/harfrust pipeline and check glyph
//! positions, so a change in egui's behaviour is caught by `cargo test`.
//!
//! Limitation: reordering happens per explicit line (`\n`). Text that egui soft-wraps would be
//! wrapped in visual order, so RTL labels are laid out without wrapping (see `ui::theme`).

use std::borrow::Cow;

use unicode_bidi::{Level, ParagraphBidiInfo};

/// Paragraph direction used when resolving bidi levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseDir {
    /// Right-to-left paragraph (Arabic UI strings).
    Rtl,
    /// Left-to-right paragraph.
    Ltr,
    /// Take the direction from the first strong character (user content such as device names).
    Auto,
}

/// True if `c` belongs to a right-to-left script or is an Arabic-block character (Hebrew,
/// Arabic incl. Arabic-Indic digits, Syriac, Thaana, NKo, presentation forms).
fn is_rtl_char(c: char) -> bool {
    matches!(c as u32,
        0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x1EE00..=0x1EEFF)
}

/// Characters egui resolves to the bundled Arabic font face (no other bundled font has them).
pub(super) fn uses_arabic_face(c: char) -> bool {
    matches!(c as u32, 0x0600..=0x06FF | 0x0750..=0x077F | 0x0870..=0x08FF | 0xFB50..=0xFDFF
        | 0xFE70..=0xFEFF)
}

/// Arabic combining marks (harakat, Quranic marks, superscript alef).
pub fn is_arabic_mark(c: char) -> bool {
    matches!(c as u32,
        0x0610..=0x061A | 0x064B..=0x065F | 0x0670 | 0x06D6..=0x06DC | 0x06DF..=0x06E4
        | 0x06E7..=0x06E8 | 0x06EA..=0x06ED | 0x08D3..=0x08E1 | 0x08E3..=0x08FF)
}

/// Characters whose Unicode script is Arabic (not Common/Inherited). HarfBuzz guesses the run
/// direction from the first such character; runs without one are shaped left-to-right.
pub(super) fn is_arabic_script(c: char) -> bool {
    uses_arabic_face(c)
        && !is_arabic_mark(c)
        && !matches!(c as u32, 0x060C | 0x061B | 0x061F | 0x0640 | 0x06DD)
}

/// Bidi formatting characters (LRM/RLM, embeddings, isolates). They steer the algorithm but
/// have no glyph, so they are removed from display output.
pub(super) fn is_bidi_control(c: char) -> bool {
    matches!(c as u32, 0x200E | 0x200F | 0x202A..=0x202E | 0x2066..=0x2069)
}

/// Wrap `s` in a left-to-right isolate (LRI … PDI) so that, inside an RTL sentence, it is laid
/// out as one left-to-right unit. Use for IDs, IPs and codes substituted into templates
/// (otherwise digit groups separated by spaces or dots would be reordered right-to-left).
pub fn ltr_isolate(s: &str) -> String {
    format!("\u{2066}{s}\u{2069}")
}

/// True if the text contains any right-to-left character.
pub fn has_rtl(text: &str) -> bool {
    text.chars().any(is_rtl_char)
}

/// Prepare `text` for display by egui (see module docs).
///
/// Returns the input unchanged (borrowed) when it contains no RTL characters, so calling this on
/// IDs, IP addresses or Latin names is cheap.
pub fn shape_for_display(text: &str, base: BaseDir) -> Cow<'_, str> {
    if !has_rtl(text) {
        if text.chars().any(is_bidi_control) {
            return Cow::Owned(text.chars().filter(|c| !is_bidi_control(*c)).collect());
        }
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let (masked, pieces) = mask_isolates(line);
        let mut visual = compensate_for_egui(&visual_order(&masked, base));
        for (n, piece) in pieces.iter().enumerate() {
            visual = visual.replace(placeholder(n), &compensate_for_egui(piece));
        }
        out.push_str(&visual);
    }
    Cow::Owned(out)
}

/// Display text for a `TextEdit` layouter: same as [`shape_for_display`] but `None` unless the
/// result has exactly as many `char`s as the input (so egui's cursor indices stay in range).
pub fn shape_for_edit(text: &str, base: BaseDir) -> Option<String> {
    if !has_rtl(text) {
        return None;
    }
    let shaped = shape_for_display(text, base);
    (shaped.chars().count() == text.chars().count()).then(|| shaped.into_owned())
}

/// Text that is already in visual order but contains Arabic-script runs (e.g. Arabic-Indic
/// digits): make egui show it exactly in that order.
pub fn visual_for_egui(text: &str) -> Cow<'_, str> {
    if text.chars().any(uses_arabic_face) {
        Cow::Owned(compensate_for_egui(text))
    } else {
        Cow::Borrowed(text)
    }
}

/// Stand-in for the n-th isolated piece. Block elements (U+2580..U+259F) have bidi class ON, so
/// like a real isolate the placeholder acts as a neutral for the surrounding text. (Text that
/// itself contains block elements is not expected in UI strings.)
fn placeholder(n: usize) -> char {
    char::from_u32(0x2580 + n as u32).unwrap_or('\u{2580}')
}

/// Number of isolates per line that get placeholders (further ones are left to UAX #9).
const MAX_ISOLATES: usize = 32;

/// Replace each `LRI … PDI` isolate by a placeholder and return the pieces (controls removed).
/// The pieces are pasted back after reordering, so an isolated ID, code or IP is always shown
/// left-to-right with its digit groups in order — whatever digits it uses (Arabic-Indic digit
/// groups would otherwise be reordered right-to-left by UAX #9).
fn mask_isolates(line: &str) -> (Cow<'_, str>, Vec<String>) {
    if !line.contains('\u{2066}') {
        return (Cow::Borrowed(line), Vec::new());
    }
    let mut masked = String::with_capacity(line.len());
    let mut pieces = Vec::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{2066}' && pieces.len() < MAX_ISOLATES {
            let mut piece = String::new();
            for inner in chars.by_ref() {
                if inner == '\u{2069}' {
                    break;
                }
                if !is_bidi_control(inner) {
                    piece.push(inner);
                }
            }
            masked.push(placeholder(pieces.len()));
            pieces.push(piece);
        } else {
            masked.push(c);
        }
    }
    (Cow::Owned(masked), pieces)
}

/// Reorder a single line (no `\n`) from logical to visual order.
fn visual_order(line: &str, base: BaseDir) -> String {
    if line.is_empty() {
        return String::new();
    }
    let level = match base {
        BaseDir::Rtl => Some(Level::rtl()),
        BaseDir::Ltr => Some(Level::ltr()),
        BaseDir::Auto => None,
    };
    let info = ParagraphBidiInfo::new(line, level);
    let (levels, runs) = info.visual_runs(0..line.len());
    let mut out = String::with_capacity(line.len());
    for run in runs {
        let segment = &line[run.clone()];
        if levels[run.start].is_rtl() {
            push_reversed_clusters(&mut out, segment, true);
        } else {
            out.push_str(segment);
        }
    }
    out.retain(|c| !is_bidi_control(c));
    out
}

/// Undo, in advance, the reversal egui's shaper applies to right-to-left font runs.
fn compensate_for_egui(visual: &str) -> String {
    let mut out = String::with_capacity(visual.len());
    let mut start = 0;
    let mut current: Option<bool> = None;
    let flush = |out: &mut String, seg: &str| {
        if seg.chars().any(uses_arabic_face) && seg.chars().any(is_arabic_script) {
            push_reversed_clusters(out, seg, false);
        } else {
            out.push_str(seg);
        }
    };
    for (i, c) in visual.char_indices() {
        // Marks belong to the run of their base character.
        let arabic = uses_arabic_face(c) || (is_arabic_mark(c) && current == Some(true));
        if current.is_some_and(|a| a != arabic) {
            flush(&mut out, &visual[start..i]);
            start = i;
        }
        current = Some(arabic);
    }
    flush(&mut out, &visual[start..]);
    out
}

/// Push `segment` with its clusters (base char + following marks) in reverse order, keeping
/// marks after their base.
fn push_reversed_clusters(out: &mut String, segment: &str, mirror_brackets: bool) {
    let mut clusters: Vec<&str> = Vec::new();
    let mut start = 0;
    for (i, c) in segment.char_indices() {
        if i > start && !is_arabic_mark(c) {
            clusters.push(&segment[start..i]);
            start = i;
        }
    }
    if start < segment.len() {
        clusters.push(&segment[start..]);
    }
    for cluster in clusters.into_iter().rev() {
        let mut chars = cluster.chars();
        if let Some(first) = chars.next() {
            out.push(if mirror_brackets {
                mirror(first)
            } else {
                first
            });
            out.extend(chars);
        }
    }
}

/// Bidi mirroring for the paired punctuation we may show inside RTL runs.
pub(super) fn mirror(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '«' => '»',
        '»' => '«',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{epaint::text::Fonts, Color32, FontId};

    /// Lay `display` out with the real epaint + harfrust pipeline and the app's fonts; return
    /// the visible glyphs' characters sorted from left to right on screen.
    fn egui_screen_order(display: &str) -> Vec<char> {
        let mut fonts = Fonts::new(Default::default(), crate::ui::theme::font_definitions());
        let galley = fonts.with_pixels_per_point(1.0).layout_no_wrap(
            display.to_owned(),
            FontId::proportional(16.0),
            Color32::WHITE,
        );
        let mut glyphs: Vec<(f32, char)> = galley
            .rows
            .iter()
            .flat_map(|r| r.row.glyphs.iter())
            .filter(|g| g.advance_width > 0.0) // skip ligature continuations and marks
            .map(|g| (g.pos.x, g.chr))
            .collect();
        glyphs.sort_by(|a, b| a.0.total_cmp(&b.0));
        glyphs.into_iter().map(|(_, c)| c).collect()
    }

    /// What a reader expects to see, left to right: for pure-RTL text, the logical string
    /// reversed.
    fn reversed(s: &str) -> Vec<char> {
        s.chars().filter(|c| !is_arabic_mark(*c)).rev().collect()
    }

    #[test]
    fn latin_text_is_borrowed_unchanged() {
        let s = "192.168.1.20";
        assert!(matches!(
            shape_for_display(s, BaseDir::Rtl),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn egui_shows_arabic_sentence_in_rtl_order() {
        for s in ["هذا الجهاز", "الأجهزة على هذه الشبكة", "معرّف الجهاز"]
        {
            let shown = egui_screen_order(&shape_for_display(s, BaseDir::Rtl));
            assert_eq!(shown, reversed(s), "{s}");
        }
    }

    #[test]
    fn egui_without_our_bidi_gets_word_order_wrong() {
        // Documents why this module exists: raw text keeps words left-to-right.
        let s = "هذا الجهاز";
        assert_ne!(egui_screen_order(s), reversed(s));
    }

    #[test]
    fn egui_mixed_arabic_latin_and_ip() {
        // "متصل بـ PC-01": Latin run stays LTR and sits at the visual left end.
        let shown: String = egui_screen_order(&shape_for_display("متصل بـ PC-01", BaseDir::Rtl))
            .into_iter()
            .collect();
        assert!(shown.starts_with("PC-01"), "{shown:?}");
        let text = format!("العنوان {}", ltr_isolate("192.168.1.40"));
        let shown: String = egui_screen_order(&shape_for_display(&text, BaseDir::Rtl))
            .into_iter()
            .collect();
        assert!(shown.starts_with("192.168.1.40"), "{shown:?}");
    }

    #[test]
    fn egui_digit_ranges_in_table_strings() {
        use crate::i18n::{tr, Lang, S};
        let shown: String = egui_screen_order(tr(Lang::Ar, S::DigitsArabicIndic))
            .into_iter()
            .collect();
        assert!(shown.contains("٠-٩"), "{shown:?}");
        let shown: String = egui_screen_order(tr(Lang::Ar, S::DigitsWestern))
            .into_iter()
            .collect();
        assert!(shown.contains("0-9"), "{shown:?}");
    }

    #[test]
    fn egui_isolates_act_as_neutrals_in_rtl() {
        // Logical: ID first, then IP, then the Arabic status. In an RTL line the ID must be
        // rightmost and the status leftmost.
        let text = format!(
            "{} · {} · متاح",
            ltr_isolate("715 204 386"),
            ltr_isolate("192.168.1.40")
        );
        let shown: String = egui_screen_order(&shape_for_display(&text, BaseDir::Rtl))
            .into_iter()
            .collect();
        let id = shown.find("715 204 386").expect("id");
        let ip = shown.find("192.168.1.40").expect("ip");
        assert!(ip < id, "{shown:?}");
        assert!(shown.starts_with("حاتم"), "{shown:?}");
    }

    #[test]
    fn egui_arabic_indic_digits_keep_their_order() {
        let id = "٤٨٢ ٩١٣ ٠٥٧";
        let shown: String = egui_screen_order(&visual_for_egui(id))
            .into_iter()
            .collect();
        assert_eq!(shown, id);
        let text = format!("الاتصال بـ {}", ltr_isolate(id));
        let shown: String = egui_screen_order(&shape_for_display(&text, BaseDir::Rtl))
            .into_iter()
            .collect();
        assert!(shown.starts_with(id), "{shown:?}");
    }

    #[test]
    fn egui_shapes_with_joining_ligatures_and_harakat() {
        let mut fonts = Fonts::new(Default::default(), crate::ui::theme::font_definitions());
        let mut view = fonts.with_pixels_per_point(1.0);
        let font = FontId::proportional(16.0);
        let width = |view: &mut eframe::egui::epaint::text::FontsView<'_>, s: &str| {
            view.layout_no_wrap(s.to_owned(), font.clone(), Color32::WHITE)
                .size()
                .x
        };
        // Joined forms are narrower than isolated letters separated by ZWNJ.
        let joined = width(&mut view, &shape_for_display("بببب", BaseDir::Rtl));
        let isolated = width(&mut view, "ب\u{200C}ب\u{200C}ب\u{200C}ب");
        assert!(joined < isolated, "joined {joined} vs isolated {isolated}");
        // lam-alef takes the ligature form: narrower than lam and alef kept apart by ZWNJ.
        let lig = width(&mut view, &shape_for_display("لا", BaseDir::Rtl));
        let apart = width(&mut view, "ل\u{200C}ا");
        assert!(lig < apart, "ligature {lig} vs apart {apart}");
        // Harakat do not add width (positioned over the letter by GPOS).
        let plain = width(&mut view, &shape_for_display("كتب", BaseDir::Rtl));
        let voweled = width(&mut view, &shape_for_display("كَتَبَ", BaseDir::Rtl));
        assert!((plain - voweled).abs() < 0.5, "{plain} vs {voweled}");
    }

    #[test]
    fn harakat_stay_with_their_letter() {
        let s = shape_for_display("بَب", BaseDir::Rtl);
        // Visual order hands egui the Arabic run in logical order: beh, fatha, beh.
        assert_eq!(s, "بَب");
        let _ = shape_for_display("\u{064E}ب", BaseDir::Rtl); // leading mark must not panic
    }

    #[test]
    fn auto_direction_for_user_content() {
        let s = shape_for_display("PC مكتب", BaseDir::Auto);
        assert!(s.starts_with("PC "), "got {s:?}");
    }

    #[test]
    fn brackets_are_mirrored_in_rtl_runs() {
        let shown: String = egui_screen_order(&shape_for_display("(نص)", BaseDir::Rtl))
            .into_iter()
            .collect();
        assert!(
            shown.starts_with('(') && shown.ends_with(')'),
            "got {shown:?}"
        );
    }

    #[test]
    fn controls_never_reach_the_screen() {
        assert_eq!(
            shape_for_display(&ltr_isolate("10.0.0.1"), BaseDir::Ltr),
            "10.0.0.1"
        );
        assert!(
            !shape_for_display(&format!("نص {}", ltr_isolate("1")), BaseDir::Rtl)
                .chars()
                .any(is_bidi_control)
        );
    }

    #[test]
    fn edit_display_is_a_permutation() {
        for s in ["مكتب لا", "مرحباً بك", "PC-01 حاسوب", "كَتَبَ (نص)"] {
            let e = shape_for_edit(s, BaseDir::Auto).expect("permutation");
            let mut a: Vec<char> = e.chars().collect();
            let mut b: Vec<char> = s.chars().collect();
            a.sort();
            b.sort();
            assert_eq!(a.len(), b.len(), "{s:?}");
            assert!(
                a.iter().zip(&b).all(|(x, y)| x == y || mirror(*x) == *y),
                "{s:?}"
            );
        }
        assert_eq!(shape_for_edit("abc", BaseDir::Auto), None);
    }
}
