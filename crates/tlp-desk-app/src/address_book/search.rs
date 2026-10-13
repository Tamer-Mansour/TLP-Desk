//! Arabic-aware, diacritics-insensitive search over saved devices.
//!
//! Both the query and the searched text are *folded* with [`fold`] and then compared by
//! substring, term by term. Folding makes the spellings people actually type match each
//! other:
//!
//! * harakat and other Arabic marks (fatha, damma, kasra, shadda, sukun, tanwin, superscript
//!   alef, Quranic marks) and tatweel are removed: `مُحَمَّد` finds `محمد` and vice versa;
//! * hamza / madda forms of alef become a bare alef (`أ إ آ ٱ` -> `ا`), `ى` -> `ي`,
//!   `ة` -> `ه`, `ؤ` -> `و`, `ئ` -> `ي`;
//! * Arabic-Indic and Persian digits become `0-9`; bidi controls, ZWJ and ZWNJ are dropped;
//! * Latin text is lowercased (full Unicode lowercase).

use super::Entry;

/// Fold `s` for comparison (see the module docs).
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if is_ignorable(c) {
            continue;
        }
        let c = match c {
            'أ' | 'إ' | 'آ' | 'ٱ' | 'ٲ' | 'ٳ' => 'ا',
            'ى' | 'ئ' | 'ی' => 'ي',
            'ة' => 'ه',
            'ؤ' => 'و',
            'ك' | 'ک' => 'ك',
            '٠'..='٩' => digit(c, '٠'),
            '۰'..='۹' => digit(c, '۰'),
            _ => c,
        };
        out.extend(c.to_lowercase());
    }
    out
}

fn digit(c: char, zero: char) -> char {
    let d = u32::from(c).saturating_sub(u32::from(zero));
    char::from_digit(d, 10).unwrap_or(c)
}

/// Marks and invisible characters that never change what a word "is".
fn is_ignorable(c: char) -> bool {
    matches!(c,
        // Harakat, tanwin, shadda, sukun, extended Arabic marks.
        '\u{0610}'..='\u{061A}'
        | '\u{064B}'..='\u{065F}'
        // Superscript alef.
        | '\u{0670}'
        // Quranic annotation marks.
        | '\u{06D6}'..='\u{06DC}'
        | '\u{06DF}'..='\u{06E8}'
        | '\u{06EA}'..='\u{06ED}'
        // Tatweel (kashida).
        | '\u{0640}'
        // ZWNJ, ZWJ, LRM, RLM, bidi embeddings / overrides / isolates, ALM.
        | '\u{200C}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2066}'..='\u{2069}'
        | '\u{061C}'
    )
}

/// A prepared query: folded terms, all of which must match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    terms: Vec<String>,
}

impl Query {
    /// Split `text` on whitespace and fold each term. An empty query matches everything.
    pub fn new(text: &str) -> Self {
        Self {
            terms: text
                .split_whitespace()
                .map(fold)
                .filter(|t| !t.is_empty())
                .collect(),
        }
    }

    /// Whether the query has no terms.
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Whether every term occurs in one of the entry's searchable fields (alias, announced
    /// name, group, tags, notes, device ID with or without spaces, IPs, MAC).
    pub fn matches(&self, entry: &Entry) -> bool {
        if self.terms.is_empty() {
            return true;
        }
        let hay = haystack(entry);
        self.terms.iter().all(|t| hay.contains(t.as_str()))
    }
}

fn haystack(e: &Entry) -> String {
    let mut parts: Vec<String> = vec![
        e.alias.clone(),
        e.name.clone(),
        e.group.clone(),
        e.notes.clone(),
        e.device_id.to_string(),
        format!("{:09}", e.device_id.0),
    ];
    parts.extend(e.tags.iter().cloned());
    parts.extend(e.last_ips.iter().map(ToString::to_string));
    if let Some(mac) = e.mac {
        parts.push(mac.to_string());
    }
    // A separator no folded term can contain, so terms never match across fields.
    fold(&parts.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address_book::MacAddr;
    use crate::backend::DeviceId;

    fn entry(alias: &str) -> Entry {
        let mut e = Entry::new(DeviceId(482_913_057));
        e.alias = alias.into();
        e
    }

    #[test]
    fn arabic_matches_with_and_without_harakat() {
        let plain = entry("حاسوب محمد");
        let voweled = entry("حاسُوب مُحَمَّد");
        for q in ["محمد", "مُحَمَّد", "مـحـمـد", "حاسوب محمد"] {
            assert!(Query::new(q).matches(&plain), "{q} vs plain");
            assert!(Query::new(q).matches(&voweled), "{q} vs voweled");
        }
        assert!(!Query::new("أحمد").matches(&plain));
    }

    #[test]
    fn arabic_letter_variants_match_each_other() {
        let e = entry("مكتبة أحمد في المستشفى");
        for q in ["مكتبه", "احمد", "إحمد", "المستشفي", "مكتبة"] {
            assert!(Query::new(q).matches(&e), "{q}");
        }
        let e = entry("حاسوب\u{200F}\u{200C}المدير");
        assert!(Query::new("حاسوبالمدير").matches(&e));
    }

    #[test]
    fn latin_is_case_insensitive_and_terms_are_anded() {
        let mut e = entry("Office PC");
        e.group = "Accounting".into();
        e.tags = vec!["Printer-Room".into()];
        assert!(Query::new("office").matches(&e));
        assert!(Query::new("OFFICE accounting").matches(&e));
        assert!(Query::new("printer-room").matches(&e));
        assert!(!Query::new("office warehouse").matches(&e));
    }

    #[test]
    fn ids_ips_and_mac_are_searchable_in_any_digit_style() {
        let mut e = entry("");
        e.push_ip("192.168.1.20".parse().unwrap());
        e.mac = Some(MacAddr([0, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e]));
        for q in [
            "482913057",
            "482 913",
            "٤٨٢٩١٣٠٥٧",
            "192.168.1.20",
            "١٩٢.١٦٨",
            "4d:5e",
        ] {
            assert!(Query::new(q).matches(&e), "{q}");
        }
        assert!(!Query::new("10.0.0.1").matches(&e));
    }

    #[test]
    fn empty_query_matches_everything() {
        assert!(Query::new("   ").is_empty());
        assert!(Query::new("").matches(&entry("x")));
    }

    #[test]
    fn terms_do_not_match_across_fields() {
        let mut e = entry("abc");
        e.group = "def".into();
        assert!(!Query::new("cde").matches(&e));
    }
}
