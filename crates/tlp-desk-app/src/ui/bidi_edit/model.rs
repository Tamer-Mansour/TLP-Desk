//! Pure model of the bidi text editor: lines, caret slots and visual navigation. No egui here
//! so every rule is unit-tested with plain strings (Arabic, mixed Arabic/Latin/digits).
//!
//! # Vocabulary
//!
//! * **caret** / **anchor**: logical `char` index into the whole text (0..=len). This is what
//!   is stored and edited.
//! * **segment**: one visual line: a hard line (split at `\n`) or one wrapped part of it.
//! * **cluster**: a base character plus the combining marks after it. The caret never sits
//!   inside a cluster.
//! * **slot**: a position between clusters in *screen* order, `0` = left edge of the segment,
//!   `clusters` = right edge. A caret is drawn at the slot it maps to ([`Segment::slot_of`]).
//!
//! A logical boundary is drawn at the leading edge of the cluster after it (its right edge for
//! an RTL cluster), or at the trailing edge of the last cluster at the end of the segment.
//! Left/Right move one slot on screen, so they always move the caret in the arrow's direction,
//! whatever the text direction is under it.

use crate::i18n::shaping::is_arabic_mark;
use crate::i18n::{bidi_line, BaseDir, BidiLine};

/// A base character with its combining marks, `start..end` in text `char` indices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cluster {
    pub start: usize,
    pub end: usize,
    /// In a right-to-left run.
    pub rtl: bool,
}

/// One visual line of the text.
#[derive(Debug, Clone)]
pub struct Segment {
    /// First character (text index).
    pub start: usize,
    /// One past the last character (a `\n` after it is not part of the segment).
    pub end: usize,
    /// Bidi mapping; `None` for lines without right-to-left characters (shown as typed).
    pub line: Option<BidiLine>,
    pub clusters: Vec<Cluster>,
    /// Cluster indices from the left edge of the screen to the right.
    pub visual: Vec<usize>,
    /// Screen position of every cluster (inverse of `visual`).
    pub pos_of: Vec<usize>,
}

impl Segment {
    /// Builds the segment `chars[start..end]` (no `\n` inside).
    pub fn new(chars: &[char], start: usize, end: usize, base: BaseDir) -> Self {
        let slice = chars.get(start..end).unwrap_or_default();
        let text: String = slice.iter().collect();
        let line = bidi_line(&text, base);
        let mut clusters: Vec<Cluster> = Vec::new();
        let mut cluster_of_char = vec![0usize; slice.len()];
        for (i, &c) in slice.iter().enumerate() {
            if i == 0 || !is_arabic_mark(c) {
                clusters.push(Cluster {
                    start: start + i,
                    end: start + i + 1,
                    rtl: line
                        .as_ref()
                        .is_some_and(|l| l.rtl.get(i).copied().unwrap_or(false)),
                });
            } else if let Some(last) = clusters.last_mut() {
                last.end = start + i + 1;
            }
            cluster_of_char[i] = clusters.len().saturating_sub(1);
        }
        let visual: Vec<usize> = match &line {
            Some(l) => l
                .visual_logical
                .iter()
                .filter_map(|&li| {
                    let ci = *cluster_of_char.get(li)?;
                    (clusters.get(ci)?.start == start + li).then_some(ci)
                })
                .collect(),
            None => (0..clusters.len()).collect(),
        };
        let mut pos_of = vec![0; clusters.len()];
        for (p, &ci) in visual.iter().enumerate() {
            if let Some(slot) = pos_of.get_mut(ci) {
                *slot = p;
            }
        }
        Self {
            start,
            end,
            line,
            clusters,
            visual,
            pos_of,
        }
    }

    /// Number of caret slots minus one (the right edge's slot number).
    pub fn last_slot(&self) -> usize {
        self.clusters.len()
    }

    /// The paragraph's base direction is right-to-left (right-aligned).
    pub fn base_rtl(&self) -> bool {
        self.line.as_ref().is_some_and(|l| l.base_rtl)
    }

    fn leading_slot(&self, i: usize) -> usize {
        let rtl = self.clusters.get(i).is_some_and(|c| c.rtl);
        self.pos_of.get(i).copied().unwrap_or(0) + usize::from(rtl)
    }

    fn trailing_slot(&self, i: usize) -> usize {
        let rtl = self.clusters.get(i).is_some_and(|c| c.rtl);
        self.pos_of.get(i).copied().unwrap_or(0) + usize::from(!rtl)
    }

    /// Screen slots where the logical boundary `caret` can be drawn: the leading edge of the
    /// cluster after it and the trailing edge of the cluster before it. They are the same slot
    /// inside a run and two different ones where a left-to-right and a right-to-left run meet.
    pub fn spots(&self, caret: usize) -> Vec<usize> {
        let mut out = Vec::with_capacity(2);
        if let Some(i) = self.clusters.iter().position(|c| c.start == caret) {
            out.push(self.leading_slot(i));
            if i > 0 {
                out.push(self.trailing_slot(i - 1));
            }
        } else if caret >= self.end {
            if let Some(last) = self.clusters.len().checked_sub(1) {
                out.push(self.trailing_slot(last));
            }
        }
        if out.is_empty() {
            out.push(0);
        }
        out.dedup();
        out
    }

    /// Where a caret is drawn by default: next to the character before it (so the caret stays
    /// beside what was just typed or deleted), at the start of the line next to the first one.
    pub fn default_slot(&self, caret: usize) -> usize {
        if let Some(p) = self
            .clusters
            .iter()
            .position(|c| c.end == caret && c.start < caret)
        {
            self.trailing_slot(p)
        } else if let Some(n) = self.clusters.iter().position(|c| c.start == caret) {
            self.leading_slot(n)
        } else {
            0
        }
    }

    /// Slot of the segment's logical end (trailing edge of the last cluster).
    pub fn default_slot_at_end(&self) -> usize {
        match self.clusters.len().checked_sub(1) {
            Some(last) => self.trailing_slot(last),
            None => 0,
        }
    }

    /// Carets that can be drawn at `slot` (one or two: where runs of different direction meet).
    pub fn carets_at(&self, slot: usize) -> Vec<usize> {
        self.boundaries()
            .filter(|&b| self.spots(b).contains(&slot))
            .collect()
    }

    /// Every caret position of the segment: cluster starts, then the end.
    pub fn boundaries(&self) -> impl Iterator<Item = usize> + '_ {
        self.clusters
            .iter()
            .map(|c| c.start)
            .chain(std::iter::once(self.end))
    }

    /// Logical start/end caret of the segment.
    pub fn logical_range(&self) -> (usize, usize) {
        (self.start, self.end)
    }
}

/// Splits `chars` into hard lines (at `\n`) and wraps them: `measure(start, end)` returns the
/// on-screen width of `chars[start..end]`. Wrapping breaks after a space, else between
/// characters when a word is wider than `max_width`. `None` = never wrap.
pub fn split_segments(
    chars: &[char],
    base: BaseDir,
    max_width: Option<f32>,
    measure: &mut dyn FnMut(usize, usize) -> f32,
) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut line_start = 0;
    loop {
        let line_end = chars
            .get(line_start..)
            .and_then(|rest| rest.iter().position(|&c| c == '\n'))
            .map_or(chars.len(), |p| line_start + p);
        let mut pieces = wrap_line(line_start, line_end, max_width, measure);
        prefer_space_breaks(chars, &mut pieces);
        for (s, e) in pieces {
            out.push(Segment::new(chars, s, e, base));
        }
        if line_end >= chars.len() {
            break;
        }
        line_start = line_end + 1;
    }
    out
}

/// Breaks `start..end` into pieces that fit `max_width`.
fn wrap_line(
    start: usize,
    end: usize,
    max_width: Option<f32>,
    measure: &mut dyn FnMut(usize, usize) -> f32,
) -> Vec<(usize, usize)> {
    let Some(max) = max_width.filter(|m| m.is_finite() && *m > 0.0) else {
        return vec![(start, end)];
    };
    let mut pieces = Vec::new();
    let mut s = start;
    while s < end {
        if measure(s, end) <= max {
            pieces.push((s, end));
            return pieces;
        }
        // Largest e in (s, end) that fits (at least one character per piece).
        let (mut lo, mut hi) = (s + 1, end);
        while lo < hi {
            let mid = lo + (hi - lo).div_ceil(2);
            if measure(s, mid) <= max {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        pieces.push((s, lo));
        s = lo;
    }
    if pieces.is_empty() {
        pieces.push((start, end));
    }
    pieces
}

/// `wrap_line` breaks at an arbitrary character; move each break to just after a space so
/// words stay whole (a word wider than the line keeps its character break).
fn prefer_space_breaks(chars: &[char], pieces: &mut [(usize, usize)]) {
    for i in 0..pieces.len().saturating_sub(1) {
        let (s, e) = pieces[i];
        let next_end = pieces[i + 1].1;
        if chars.get(e) == Some(&' ') && e + 1 < next_end {
            // The break is right before a space: let the space hang at the end of this line.
            pieces[i].1 = e + 1;
            pieces[i + 1].0 = e + 1;
            continue;
        }
        let mid_word = e > 0
            && chars.get(e).is_some_and(|c| !c.is_whitespace())
            && chars.get(e - 1).is_some_and(|c| !c.is_whitespace());
        if !mid_word {
            continue;
        }
        if let Some(sp) = chars
            .get(s..e)
            .and_then(|sl| sl.iter().rposition(|c| *c == ' '))
        {
            let new_end = s + sp + 1;
            if new_end > s && new_end < e {
                pieces[i].1 = new_end;
                pieces[i + 1].0 = new_end;
            }
        }
    }
}

/// A drawable caret place: the logical boundary plus where on its line it is drawn (two
/// places are possible where runs of different direction meet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pos {
    /// Segment (visual line) it is drawn in.
    pub seg: usize,
    /// Logical caret (text `char` index).
    pub caret: usize,
    /// Screen slot in the segment.
    pub slot: usize,
}

/// The laid-out text: characters plus segments.
#[derive(Debug, Clone)]
pub struct Layout {
    pub chars: Vec<char>,
    pub segs: Vec<Segment>,
}

impl Layout {
    /// Index of the segment that owns `caret` (the later one at a soft wrap).
    pub fn seg_of(&self, caret: usize) -> usize {
        self.segs
            .iter()
            .rposition(|s| s.start <= caret)
            .unwrap_or(0)
    }

    /// Snaps `caret` back to the start of the cluster that contains it.
    pub fn snap(&self, caret: usize) -> usize {
        let caret = caret.min(self.chars.len());
        let seg = match self.segs.get(self.seg_of(caret)) {
            Some(s) => s,
            None => return caret,
        };
        if caret >= seg.end {
            return caret.min(seg.end.max(seg.start));
        }
        seg.clusters
            .iter()
            .find(|c| c.start <= caret && caret < c.end)
            .map_or(caret, |c| c.start)
    }

    /// The default [`Pos`] of a logical caret.
    pub fn pos(&self, caret: usize) -> Pos {
        let caret = caret.min(self.chars.len());
        let seg = self.seg_of(caret);
        let slot = self.segs.get(seg).map_or(0, |s| s.default_slot(caret));
        Pos { seg, caret, slot }
    }

    /// `pos` if it still describes a drawable place for its caret, else the default for the
    /// caret (the text may have changed since it was stored).
    pub fn valid_pos(&self, pos: Pos) -> Pos {
        let ok = self
            .segs
            .get(pos.seg)
            .is_some_and(|s| pos.slot <= s.last_slot() && s.spots(pos.caret).contains(&pos.slot));
        if ok {
            pos
        } else {
            self.pos(pos.caret)
        }
    }

    /// One slot to the left (`right == false`) or right on screen.
    pub fn move_visual(&self, from: Pos, right: bool) -> Pos {
        let Some(seg) = self.segs.get(from.seg) else {
            return from;
        };
        let last = seg.last_slot();
        let mut slot = from.slot;
        loop {
            if right {
                if slot >= last {
                    break;
                }
                slot += 1;
            } else {
                if slot == 0 {
                    break;
                }
                slot -= 1;
            }
            if let Some(caret) = seg
                .carets_at(slot)
                .into_iter()
                .min_by_key(|b| b.abs_diff(from.caret))
            {
                return Pos {
                    seg: from.seg,
                    caret,
                    slot,
                };
            }
        }
        // Past the edge of the line: continue on the neighbouring line.
        let neighbour = if right {
            from.seg.checked_add(1)
        } else {
            from.seg.checked_sub(1)
        };
        if let Some((ni, nseg)) = neighbour.and_then(|ni| Some((ni, self.segs.get(ni)?))) {
            let slot = if right { 0 } else { nseg.last_slot() };
            if let Some(&caret) = nseg.carets_at(slot).first() {
                return Pos {
                    seg: ni,
                    caret,
                    slot,
                };
            }
        }
        from
    }

    /// Word-wise visual move (Ctrl+Arrow): skip spaces, then the word, in the arrow's direction.
    pub fn move_word(&self, from: Pos, right: bool) -> Pos {
        let mut cur = from;
        let mut in_word = false;
        for _ in 0..=self.chars.len() {
            let next = self.move_visual(cur, right);
            if next == cur {
                break;
            }
            let (lo, hi) = (cur.caret.min(next.caret), cur.caret.max(next.caret));
            let space = self
                .chars
                .get(lo..hi)
                .is_some_and(|s| s.iter().all(|c| c.is_whitespace()));
            if space && in_word && lo != hi {
                break;
            }
            in_word |= !space;
            cur = next;
        }
        cur
    }

    /// The place one line up/down whose screen x is closest to `want_x`; `x_of(segment, slot)`
    /// gives slot positions. First/last line: jump to the text start/end like egui does.
    pub fn move_vertical(
        &self,
        from: Pos,
        up: bool,
        want_x: f32,
        x_of: &dyn Fn(usize, usize) -> f32,
    ) -> Pos {
        let target = if up {
            match from.seg.checked_sub(1) {
                Some(t) => t,
                None => return self.pos(0),
            }
        } else if from.seg + 1 < self.segs.len() {
            from.seg + 1
        } else {
            return self.pos(self.chars.len());
        };
        self.pos_at_x(target, want_x, x_of).unwrap_or(from)
    }

    /// The place in segment `i` closest to screen `x`.
    pub fn pos_at_x(&self, i: usize, x: f32, x_of: &dyn Fn(usize, usize) -> f32) -> Option<Pos> {
        let seg = self.segs.get(i)?;
        let mut best: Option<(f32, Pos)> = None;
        for slot in 0..=seg.last_slot() {
            let d = (x_of(i, slot) - x).abs();
            if best.is_none_or(|(bd, _)| d < bd) {
                if let Some(&caret) = seg.carets_at(slot).first() {
                    best = Some((
                        d,
                        Pos {
                            seg: i,
                            caret,
                            slot,
                        },
                    ));
                }
            }
        }
        best.map(|(_, p)| p)
    }

    /// Range a Backspace removes: the cluster before the caret (or a line break).
    pub fn back_range(&self, caret: usize) -> Option<std::ops::Range<usize>> {
        if caret == 0 {
            return None;
        }
        for seg in &self.segs {
            if let Some(c) = seg.clusters.iter().find(|c| c.end == caret) {
                return Some(c.start..c.end);
            }
        }
        Some(caret - 1..caret)
    }

    /// Range Delete removes: the cluster at the caret (or a line break).
    pub fn forward_range(&self, caret: usize) -> Option<std::ops::Range<usize>> {
        if caret >= self.chars.len() {
            return None;
        }
        for seg in &self.segs {
            if let Some(c) = seg.clusters.iter().find(|c| c.start == caret) {
                return Some(c.start..c.end);
            }
        }
        Some(caret..caret + 1)
    }

    /// The word around `caret` (letters/digits), or the single character there.
    pub fn word_at(&self, caret: usize) -> std::ops::Range<usize> {
        let is_word = |c: char| c.is_alphanumeric() || is_arabic_mark(c) || c == '_';
        let n = self.chars.len();
        let at = |i: usize| self.chars.get(i).copied();
        let probe = if at(caret).is_some_and(is_word) {
            caret
        } else if caret > 0 && at(caret - 1).is_some_and(is_word) {
            caret - 1
        } else {
            return caret..(caret + 1).min(n);
        };
        let mut s = probe;
        while s > 0 && at(s - 1).is_some_and(is_word) {
            s -= 1;
        }
        let mut e = probe;
        while e < n && at(e).is_some_and(is_word) {
            e += 1;
        }
        s..e
    }

    /// Clusters fully inside `lo..hi`, as `(segment, cluster)` pairs, for selection painting.
    pub fn selected_clusters(&self, lo: usize, hi: usize) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        for (si, seg) in self.segs.iter().enumerate() {
            for (ci, c) in seg.clusters.iter().enumerate() {
                if c.start >= lo && c.end <= hi {
                    out.push((si, ci));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(text: &str) -> Layout {
        layout_with(text, BaseDir::Auto)
    }

    fn layout_with(text: &str, base: BaseDir) -> Layout {
        let chars: Vec<char> = text.chars().collect();
        let segs = split_segments(&chars, base, None, &mut |_, _| 0.0);
        Layout { chars, segs }
    }

    /// Walk one slot at a time; return the visited places.
    fn walk(l: &Layout, from: Pos, right: bool) -> Vec<Pos> {
        let mut out = vec![from];
        let mut cur = from;
        for _ in 0..50 {
            let next = l.move_visual(cur, right);
            if next == cur {
                break;
            }
            out.push(next);
            cur = next;
        }
        out
    }

    fn carets(v: &[Pos]) -> Vec<usize> {
        v.iter().map(|p| p.caret).collect()
    }

    #[test]
    fn latin_text_moves_left_to_right() {
        let l = layout("abc");
        assert_eq!(carets(&walk(&l, l.pos(0), true)), vec![0, 1, 2, 3]);
        assert_eq!(carets(&walk(&l, l.pos(3), false)), vec![3, 2, 1, 0]);
    }

    #[test]
    fn arabic_right_arrow_moves_toward_the_start_of_the_word_visually() {
        // "مرحبا": logical start is the rightmost letter. From the logical end (screen left)
        // pressing Right walks back through the word to its logical start.
        let l = layout("مرحبا");
        assert_eq!(carets(&walk(&l, l.pos(5), true)), vec![5, 4, 3, 2, 1, 0]);
        assert_eq!(carets(&walk(&l, l.pos(0), false)), vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn arrows_visit_every_screen_place_exactly_once_in_mixed_text() {
        for text in [
            "abc مرحبا 123",
            "مرحبا abc def مرحبا",
            "الرقم ١٢٣ هنا",
            "a ب c",
            "ب a ب",
            "(نص) abc",
        ] {
            let l = layout(text);
            let n = l.chars.len();
            let left_end = *walk(&l, l.pos(0), false).last().unwrap();
            assert_eq!(left_end.slot, 0, "{text}");
            let visited = walk(&l, left_end, true);
            let slots: Vec<usize> = visited.iter().map(|p| p.slot).collect();
            assert_eq!(slots, (0..=n).collect::<Vec<_>>(), "{text}: {visited:?}");
            // Every drawn place is a valid place of its caret.
            for p in &visited {
                assert!(l.segs[0].spots(p.caret).contains(&p.slot), "{text}: {p:?}");
            }
            // Walking back retraces the same places.
            let mut back: Vec<usize> = walk(&l, *visited.last().unwrap(), false)
                .iter()
                .map(|p| p.slot)
                .collect();
            back.reverse();
            assert_eq!(back, slots, "{text}");
            // And every logical caret is reachable somewhere (arrows, Home or End).
            let mut reached: Vec<usize> = carets(&visited);
            reached.sort_unstable();
            reached.dedup();
            let missing: Vec<usize> = (0..=n).filter(|c| !reached.contains(c)).collect();
            for m in missing {
                assert!(
                    l.segs[0].spots(m).iter().any(|s| *s <= n),
                    "{text}: caret {m} has no place"
                );
            }
        }
    }

    #[test]
    fn caret_at_a_run_boundary_has_two_places() {
        // "ab مرحبا": the boundary before the first Arabic letter can be drawn after "b"
        // (trailing edge of the previous run) or at the right end of the Arabic word.
        let l = layout("ab مرحبا");
        let seg = &l.segs[0];
        let spots = seg.spots(3);
        assert_eq!(spots.len(), 2, "{spots:?}");
        let first_arabic = seg.clusters.iter().position(|c| c.start == 3).unwrap();
        assert!(seg.clusters[first_arabic].rtl);
        assert!(spots.contains(&(seg.pos_of[first_arabic] + 1)));
        // By default the caret sits next to the character before it.
        assert_eq!(seg.default_slot(3), seg.spots(3)[1]);
        assert_eq!(seg.default_slot(0), seg.spots(0)[0]);
    }

    #[test]
    fn every_slot_has_a_caret_in_pure_runs() {
        for text in ["مرحبا", "abc", "مرحبا بالعالم", "12 ٣٤"] {
            let l = layout(text);
            let seg = &l.segs[0];
            for slot in 0..=seg.last_slot() {
                assert!(!seg.carets_at(slot).is_empty(), "{text} slot {slot}");
            }
        }
    }

    #[test]
    fn backspace_and_delete_remove_whole_clusters() {
        // beh + fatha + taa: Backspace at the end removes taa, then the beh-with-fatha pair.
        let l = layout("بَت");
        assert_eq!(l.back_range(3), Some(2..3));
        assert_eq!(l.back_range(2), Some(0..2));
        assert_eq!(l.forward_range(0), Some(0..2));
        assert_eq!(l.forward_range(2), Some(2..3));
        assert_eq!(l.back_range(0), None);
        assert_eq!(l.forward_range(3), None);
    }

    #[test]
    fn snap_moves_a_caret_out_of_a_cluster() {
        let l = layout("بَت");
        assert_eq!(l.snap(1), 0);
        assert_eq!(l.snap(2), 2);
        assert_eq!(l.snap(99), 3);
    }

    #[test]
    fn lines_split_at_newlines() {
        let l = layout("مرحبا\nabc\n");
        assert_eq!(l.segs.len(), 3);
        assert_eq!(l.segs[0].logical_range(), (0, 5));
        assert_eq!(l.segs[1].logical_range(), (6, 9));
        assert_eq!(l.segs[2].logical_range(), (10, 10));
        assert_eq!(l.back_range(6), Some(5..6), "Backspace joins lines");
        assert_eq!(l.seg_of(5), 0);
        assert_eq!(l.seg_of(6), 1);
        // Right at the visual right end of an Arabic line continues on the next line.
        let all = walk(&l, l.pos(5), true);
        assert!(all.iter().any(|p| p.seg == 1), "{all:?}");
    }

    #[test]
    fn wrapping_prefers_spaces_and_keeps_every_char_once() {
        let chars: Vec<char> = "مرحبا بالعالم الجميل".chars().collect();
        // Width = number of characters; at most 8 per line.
        let segs = split_segments(&chars, BaseDir::Auto, Some(8.0), &mut |s, e| (e - s) as f32);
        let pieces: Vec<(usize, usize)> = segs.iter().map(|g| (g.start, g.end)).collect();
        for (s, e) in &pieces {
            // A trailing space may hang past the limit.
            assert!(e - s <= 9, "{s}..{e}");
        }
        assert_eq!(pieces.first().unwrap().0, 0);
        assert_eq!(pieces.last().unwrap().1, chars.len());
        for w in pieces.windows(2) {
            assert_eq!(w[0].1, w[1].0, "pieces are contiguous");
        }
        // First piece ends right after a space.
        assert_eq!(chars[pieces[0].1 - 1], ' ');
    }

    #[test]
    fn move_vertical_picks_the_closest_slot() {
        let l = layout("abcd\nwxyz");
        // x of slot s = 10 * s on both lines.
        let x = |_: usize, s: usize| 10.0 * s as f32;
        let down = |c: usize| l.move_vertical(l.pos(c), false, 20.0, &x).caret;
        let up = |c: usize| l.move_vertical(l.pos(c), true, 20.0, &x).caret;
        assert_eq!(down(2), 7);
        assert_eq!(up(7), 2);
        assert_eq!(up(2), 0, "top line: start");
        assert_eq!(down(7), 9, "bottom line: end");
        assert_eq!(l.pos_at_x(0, 26.0, &x).unwrap().caret, 3);
    }

    #[test]
    fn word_moves_and_selection() {
        let l = layout("abc def");
        assert_eq!(l.move_word(l.pos(0), true).caret, 3);
        assert_eq!(l.move_word(l.pos(3), true).caret, 7);
        assert_eq!(l.word_at(5), 4..7);
        assert_eq!(l.word_at(3), 0..3);
        let l = layout("مرحبا بك");
        assert_eq!(l.word_at(2), 0..5);
        assert_eq!(l.selected_clusters(0, 2).len(), 2);
    }
}
