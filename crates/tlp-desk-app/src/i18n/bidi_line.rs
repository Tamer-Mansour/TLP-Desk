//! Per-character view of the bidi reordering that [`super::shape_for_display`] applies, for
//! editors that need to map between the logical text (what is stored and edited) and what egui
//! lays out and shows (needed because egui has no bidi caret, see `ui::bidi_edit`).
//!
//! [`bidi_line`] runs the same pipeline as `shape_for_display` (UAX #9 visual order, then the
//! pre-compensation for egui's own reversal of Arabic runs) but carries the logical index of
//! every character along. A test pins its display string to `shape_for_display`'s output.

use unicode_bidi::{Level, ParagraphBidiInfo};

use super::shaping::{
    is_arabic_mark, is_arabic_script, is_bidi_control, mirror, uses_arabic_face, BaseDir,
};

/// A single line (no `\n`) with its bidi mapping. All indices count `char`s of the line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BidiLine {
    /// The string to hand to egui (a permutation of the line, brackets mirrored in RTL runs).
    pub display: Vec<char>,
    /// `display_logical[p]` = logical index of the character at display position `p`.
    pub display_logical: Vec<usize>,
    /// `visual_logical[q]` = logical index of the character at screen position `q`, counted
    /// from the left. Marks follow their base character.
    pub visual_logical: Vec<usize>,
    /// `rtl[i]`: logical character `i` is in a right-to-left run (odd embedding level).
    pub rtl: Vec<bool>,
    /// The paragraph's base direction is right-to-left.
    pub base_rtl: bool,
}

impl BidiLine {
    /// Display position of every logical character (inverse of `display_logical`).
    pub fn display_of_logical(&self) -> Vec<usize> {
        let mut inv = vec![0; self.display_logical.len()];
        for (p, &l) in self.display_logical.iter().enumerate() {
            if let Some(slot) = inv.get_mut(l) {
                *slot = p;
            }
        }
        inv
    }
}

type Pairs = Vec<(char, usize)>;

/// Reverse `segment` by clusters (base + following marks stay together, marks after base).
fn reversed_clusters(segment: &[(char, usize)], mirror_brackets: bool, out: &mut Pairs) {
    let mut clusters: Vec<&[(char, usize)]> = Vec::new();
    let mut start = 0;
    for (i, &(c, _)) in segment.iter().enumerate() {
        if i > start && !is_arabic_mark(c) {
            clusters.push(segment.get(start..i).unwrap_or_default());
            start = i;
        }
    }
    if start < segment.len() {
        clusters.push(segment.get(start..).unwrap_or_default());
    }
    for cluster in clusters.into_iter().rev() {
        for (n, &(c, l)) in cluster.iter().enumerate() {
            out.push((if n == 0 && mirror_brackets { mirror(c) } else { c }, l));
        }
    }
}

/// Mirror of `shaping::compensate_for_egui` on index-carrying characters.
fn compensate(visual: &[(char, usize)]) -> Pairs {
    let mut out = Vec::with_capacity(visual.len());
    let flush = |out: &mut Pairs, seg: &[(char, usize)]| {
        if seg.iter().any(|&(c, _)| uses_arabic_face(c))
            && seg.iter().any(|&(c, _)| is_arabic_script(c))
        {
            reversed_clusters(seg, false, out);
        } else {
            out.extend_from_slice(seg);
        }
    };
    let mut start = 0;
    let mut current: Option<bool> = None;
    for (i, &(c, _)) in visual.iter().enumerate() {
        let arabic = uses_arabic_face(c) || (is_arabic_mark(c) && current == Some(true));
        if current.is_some_and(|a| a != arabic) {
            flush(&mut out, visual.get(start..i).unwrap_or_default());
            start = i;
        }
        current = Some(arabic);
    }
    flush(&mut out, visual.get(start..).unwrap_or_default());
    out
}

/// Bidi mapping of one line, or `None` when the line has no right-to-left character, contains
/// bidi control characters (they would change the character count), or is empty.
pub fn bidi_line(line: &str, base: BaseDir) -> Option<BidiLine> {
    if line.is_empty() || !super::has_rtl(line) || line.chars().any(is_bidi_control) {
        return None;
    }
    let level = match base {
        BaseDir::Rtl => Some(Level::rtl()),
        BaseDir::Ltr => Some(Level::ltr()),
        BaseDir::Auto => None,
    };
    let info = ParagraphBidiInfo::new(line, level);
    let (levels, runs) = info.visual_runs(0..line.len());
    // Byte offset -> char index.
    let mut char_of_byte = vec![0usize; line.len() + 1];
    let mut chars: Vec<(usize, char)> = Vec::new();
    for (ci, (bi, c)) in line.char_indices().enumerate() {
        for slot in char_of_byte.iter_mut().skip(bi).take(c.len_utf8()) {
            *slot = ci;
        }
        chars.push((bi, c));
    }
    if let Some(last) = char_of_byte.last_mut() {
        *last = chars.len();
    }
    let mut rtl = vec![false; chars.len()];
    let mut visual: Pairs = Vec::with_capacity(chars.len());
    for run in runs {
        let first = char_of_byte.get(run.start).copied().unwrap_or(0);
        let end = char_of_byte.get(run.end).copied().unwrap_or(chars.len());
        let run_rtl = levels.get(run.start).is_some_and(|l| l.is_rtl());
        let segment: Pairs = chars
            .get(first..end)
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(n, &(_, c))| (c, first + n))
            .collect();
        for &(_, l) in &segment {
            if let Some(r) = rtl.get_mut(l) {
                *r = run_rtl;
            }
        }
        if run_rtl {
            reversed_clusters(&segment, true, &mut visual);
        } else {
            visual.extend(segment);
        }
    }
    if visual.len() != chars.len() {
        return None;
    }
    let display = compensate(&visual);
    Some(BidiLine {
        display: display.iter().map(|&(c, _)| c).collect(),
        display_logical: display.iter().map(|&(_, l)| l).collect(),
        visual_logical: visual.iter().map(|&(_, l)| l).collect(),
        rtl,
        base_rtl: info.paragraph_level.is_rtl(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::shape_for_display;

    const SAMPLES: [&str; 8] = [
        "مرحبا",
        "مرحبا بالعالم",
        "abc مرحبا 123",
        "مرحبا abc def مرحبا",
        "الرقم ١٢٣ هنا",
        "كَتَبَ (نص) لا",
        "PC-01 حاسوب 192.168.1.2",
        "لا",
    ];

    #[test]
    fn display_matches_shape_for_display() {
        for base in [BaseDir::Auto, BaseDir::Rtl, BaseDir::Ltr] {
            for s in SAMPLES {
                let line = bidi_line(s, base).expect("rtl text");
                let shaped = shape_for_display(s, base);
                assert_eq!(
                    line.display.iter().collect::<String>(),
                    shaped,
                    "{s:?} {base:?}"
                );
            }
        }
    }

    #[test]
    fn mappings_are_permutations() {
        for s in SAMPLES {
            let line = bidi_line(s, BaseDir::Auto).expect("rtl text");
            let n = s.chars().count();
            for map in [&line.display_logical, &line.visual_logical] {
                let mut sorted = map.clone();
                sorted.sort_unstable();
                assert_eq!(sorted, (0..n).collect::<Vec<_>>(), "{s:?}");
            }
            let inv = line.display_of_logical();
            for (p, &l) in line.display_logical.iter().enumerate() {
                assert_eq!(inv[l], p);
            }
        }
    }

    #[test]
    fn pure_arabic_is_all_rtl_and_screen_order_is_reversed() {
        let line = bidi_line("بتث", BaseDir::Auto).expect("rtl");
        assert!(line.base_rtl && line.rtl.iter().all(|r| *r));
        // The first typed letter is rightmost on screen.
        assert_eq!(line.visual_logical, vec![2, 1, 0]);
    }

    #[test]
    fn latin_run_inside_arabic_keeps_its_order_and_is_ltr() {
        let line = bidi_line("ب ab ت", BaseDir::Auto).expect("rtl");
        let ltr: Vec<usize> = line
            .visual_logical
            .iter()
            .copied()
            .filter(|&l| !line.rtl[l])
            .collect();
        // "ab" are logical 2 and 3 and appear in that order from the left.
        assert!(ltr.windows(2).any(|w| w == [2, 3]), "{ltr:?}");
        assert!(!line.rtl[2] && !line.rtl[3]);
    }

    #[test]
    fn plain_latin_empty_and_controls_have_no_mapping() {
        assert!(bidi_line("abc 123", BaseDir::Auto).is_none());
        assert!(bidi_line("", BaseDir::Auto).is_none());
        assert!(bidi_line("ب\u{200F}ت", BaseDir::Auto).is_none());
    }

    #[test]
    fn marks_stay_after_their_base_in_screen_order() {
        let line = bidi_line("بَت", BaseDir::Auto).expect("rtl");
        // Logical: beh(0) fatha(1) teh(2). Screen: teh, then beh with its fatha after it.
        assert_eq!(line.visual_logical, vec![2, 0, 1]);
    }
}
