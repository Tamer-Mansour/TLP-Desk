//! Locale-aware formatting of digits, numbers, bitrates, file sizes and durations.
//!
//! Only *display* goes through here. Anything copied to the clipboard (IDs, codes, IPs) uses
//! the raw Western-digit value.
//!
//! Results are *logical-order* text: pass them through `Tx::d` (shaping + bidi) for display,
//! exactly once.

use std::borrow::Cow;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{tr_raw, Lang, S};

/// How digits are displayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DigitStyle {
    /// 0123456789
    #[default]
    Western,
    /// ٠١٢٣٤٥٦٧٨٩ (U+0660..U+0669)
    ArabicIndic,
}

impl DigitStyle {
    pub const ALL: [DigitStyle; 2] = [DigitStyle::Western, DigitStyle::ArabicIndic];

    fn decimal_sep(self) -> char {
        match self {
            DigitStyle::Western => '.',
            DigitStyle::ArabicIndic => '\u{066B}', // ARABIC DECIMAL SEPARATOR
        }
    }

    fn thousands_sep(self) -> char {
        match self {
            DigitStyle::Western => ',',
            DigitStyle::ArabicIndic => '\u{066C}', // ARABIC THOUSANDS SEPARATOR
        }
    }
}

/// Convert ASCII digits in `s` to the requested style (other characters untouched).
pub fn localize_digits(s: &str, style: DigitStyle) -> Cow<'_, str> {
    match style {
        DigitStyle::Western => Cow::Borrowed(s),
        DigitStyle::ArabicIndic if !s.bytes().any(|b| b.is_ascii_digit()) => Cow::Borrowed(s),
        DigitStyle::ArabicIndic => Cow::Owned(
            s.chars()
                .map(|c| match c {
                    '0'..='9' => char::from_u32(0x0660 + (c as u32 - '0' as u32)).unwrap_or(c),
                    other => other,
                })
                .collect(),
        ),
    }
}

/// Convert Arabic-Indic (and Extended Arabic-Indic) digits back to ASCII. Used for anything
/// the user types (address box) and for clipboard copies.
pub fn to_western_digits(s: &str) -> String {
    s.chars()
        .map(|c| match c as u32 {
            0x0660..=0x0669 => char::from_u32('0' as u32 + (c as u32 - 0x0660)).unwrap_or(c),
            0x06F0..=0x06F9 => char::from_u32('0' as u32 + (c as u32 - 0x06F0)).unwrap_or(c),
            _ => c,
        })
        .collect()
}

/// Format a number with `decimals` fraction digits and grouped thousands.
pub fn number(value: f64, decimals: usize, style: DigitStyle) -> String {
    let raw = format!("{:.*}", decimals, value.abs());
    let (int, frac) = raw.split_once('.').unwrap_or((&raw, ""));
    let mut grouped = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(style.thousands_sep());
        }
        grouped.push(c);
    }
    let mut out = String::new();
    if value < 0.0 && raw.chars().any(|c| c.is_ascii_digit() && c != '0') {
        out.push('-');
    }
    out.push_str(&grouped);
    if !frac.is_empty() {
        out.push(style.decimal_sep());
        out.push_str(frac);
    }
    localize_digits(&out, style).into_owned()
}

/// "12.4 Mbps" from kilobits per second.
pub fn bitrate(kbps: u32, lang: Lang, style: DigitStyle) -> String {
    let mbps = f64::from(kbps) / 1000.0;
    format!("{} {}", number(mbps, 1, style), tr_raw(lang, S::UnitMbps))
}

/// Human file size with binary units: "1.5 MB".
pub fn file_size(bytes: u64, lang: Lang, style: DigitStyle) -> String {
    let units = [S::UnitBytes, S::UnitKB, S::UnitMB, S::UnitGB, S::UnitTB];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    let decimals = if u == 0 { 0 } else { 1 };
    format!("{} {}", number(v, decimals, style), tr_raw(lang, units[u]))
}

/// Session duration as `h:mm:ss` (or `m:ss` under an hour).
pub fn duration(d: Duration, style: DigitStyle) -> String {
    let s = d.as_secs();
    let (h, m, sec) = (s / 3600, (s / 60) % 60, s % 60);
    let raw = if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    };
    localize_digits(&raw, style).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_roundtrip() {
        let ar = localize_digits("482 913 057", DigitStyle::ArabicIndic);
        assert_eq!(ar, "٤٨٢ ٩١٣ ٠٥٧");
        assert_eq!(to_western_digits(&ar), "482 913 057");
        assert_eq!(to_western_digits("۱۲۳"), "123");
        assert!(matches!(
            localize_digits("abc", DigitStyle::ArabicIndic),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn number_formatting() {
        assert_eq!(number(1234567.891, 2, DigitStyle::Western), "1,234,567.89");
        assert_eq!(number(1234.5, 1, DigitStyle::ArabicIndic), "١٬٢٣٤٫٥");
        assert_eq!(number(-12.0, 0, DigitStyle::Western), "-12");
        assert_eq!(number(999.0, 0, DigitStyle::Western), "999");
    }

    #[test]
    fn unit_formatting() {
        assert_eq!(bitrate(12_400, Lang::En, DigitStyle::Western), "12.4 Mbps");
        assert_eq!(file_size(1536, Lang::En, DigitStyle::Western), "1.5 KB");
        assert_eq!(file_size(512, Lang::En, DigitStyle::Western), "512 B");
        assert!(file_size(3 * 1024 * 1024, Lang::Ar, DigitStyle::ArabicIndic).starts_with("٣٫٠ "));
        assert_eq!(
            duration(Duration::from_secs(3725), DigitStyle::Western),
            "1:02:05"
        );
        assert_eq!(
            duration(Duration::from_secs(65), DigitStyle::ArabicIndic),
            "١:٠٥"
        );
    }
}
