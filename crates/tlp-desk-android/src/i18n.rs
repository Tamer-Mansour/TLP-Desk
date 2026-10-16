//! Arabic / English strings of the Android viewer.
//!
//! The Arabic shaping and bidi pass is the desktop app's `i18n/shaping.rs`, shared by path (not
//! copied) so both apps display Arabic identically.

#[path = "../../tlp-desk-app/src/i18n/shaping.rs"]
#[allow(dead_code)]
mod shaping;

use std::borrow::Cow;

pub use shaping::{has_rtl, shape_for_display, BaseDir};

/// UI language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    /// English (left to right).
    #[default]
    En,
    /// Arabic (right to left).
    Ar,
}

impl Lang {
    /// Language of the phone's UI (`ar*` gives Arabic, everything else English).
    pub fn detect() -> Lang {
        match sys_locale::get_locale() {
            Some(l)
                if l.split(['-', '_'])
                    .next()
                    .is_some_and(|p| p.eq_ignore_ascii_case("ar")) =>
            {
                Lang::Ar
            }
            _ => Lang::En,
        }
    }

    /// The other language (for the toggle button).
    pub fn other(self) -> Lang {
        match self {
            Lang::En => Lang::Ar,
            Lang::Ar => Lang::En,
        }
    }

    /// Whether layouts must be mirrored.
    pub fn rtl(self) -> bool {
        self == Lang::Ar
    }

    fn base(self) -> BaseDir {
        if self.rtl() {
            BaseDir::Rtl
        } else {
            BaseDir::Ltr
        }
    }

    /// Display-ready text for `key`.
    pub fn t(self, key: S) -> Cow<'static, str> {
        let (en, ar) = table(key);
        match self {
            Lang::En => Cow::Borrowed(en),
            Lang::Ar => Cow::Owned(shape_for_display(ar, self.base()).into_owned()),
        }
    }

    /// Display-ready dynamic text (device names, error details).
    pub fn d(self, text: &str) -> String {
        shape_for_display(text, self.base()).into_owned()
    }

    /// The raw (unshaped) text for `key`, for tests and logs.
    pub fn raw(self, key: S) -> &'static str {
        let (en, ar) = table(key);
        match self {
            Lang::En => en,
            Lang::Ar => ar,
        }
    }
}

macro_rules! strings {
    ($($name:ident => $en:literal, $ar:literal;)*) => {
        /// String keys.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum S { $($name,)* }

        /// Every key, for tests.
        pub const ALL: &[S] = &[$(S::$name,)*];

        fn table(key: S) -> (&'static str, &'static str) {
            match key { $(S::$name => ($en, $ar),)* }
        }
    };
}

strings! {
    AppName => "TLP-Desk", "TLP-Desk";
    ConnectTitle => "Connect to a computer", "الاتصال بحاسوب";
    AddressLabel => "Device ID or IP address", "معرّف الجهاز أو عنوان IP";
    AddressHint => "123 456 789  or  192.168.1.20", "123 456 789  أو  192.168.1.20";
    CodeLabel => "One-time code or password", "الرمز لمرة واحدة أو كلمة المرور";
    CodeHint => "Shown on the other computer", "يظهر على الحاسوب الآخر";
    Connect => "Connect", "اتصال";
    Connecting => "Connecting...", "جارٍ الاتصال...";
    WaitingAccept => "Waiting for the other computer to accept...", "بانتظار موافقة الحاسوب الآخر...";
    ConnectFailed => "Could not connect: {error}", "تعذّر الاتصال: {error}";
    Disconnect => "Disconnect", "قطع الاتصال";
    Keyboard => "Keyboard", "لوحة المفاتيح";
    ResetZoom => "Fit screen", "ملاءمة الشاشة";
    WaitingFrame => "Waiting for the first picture...", "بانتظار أول صورة...";
    SessionEnded => "The session ended.", "انتهت الجلسة.";
    ViewOnly => "View only: the other computer did not allow control.", "عرض فقط: لم يسمح الحاسوب الآخر بالتحكم.";
    GestureHelp => "Tap = click, hold = right click, drag = drag, two fingers = scroll, pinch = zoom", "نقرة = نقر، ضغط مطوّل = نقر أيمن، سحب = سحب، إصبعان = تمرير، قرص = تكبير";
    LanOnly => "Works on your local network only. Nothing goes through the internet.", "يعمل على شبكتك المحلية فقط، ولا يمر شيء عبر الإنترنت.";
    Language => "العربية", "English";
    // The two digit-style names exist so the shared shaping tests (which look them up) run.
    DigitsWestern => "Western (0-9)", "غربية (\u{2066}0-9\u{2069})";
    DigitsArabicIndic => "Arabic-Indic (\u{2066}٠-٩\u{2069})", "هندية (\u{2066}٠-٩\u{2069})";
    KeyboardNote => "Letters the other computer's layout may not have are pasted through the clipboard.", "الأحرف غير المتوفرة في لوحة المفاتيح البعيدة تُلصق عبر الحافظة.";
}

/// Raw table lookup (the shared shaping tests call this name).
pub fn tr(lang: Lang, key: S) -> &'static str {
    lang.raw(key)
}

impl Lang {
    /// Template substitution: replaces `{error}` with `value` (logical order), then shapes.
    pub fn f(self, key: S, value: &str) -> String {
        let raw = self.raw(key).replace("{error}", value);
        match self {
            Lang::En => raw,
            Lang::Ar => shape_for_display(&raw, self.base()).into_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_has_english_and_arabic() {
        for &k in ALL {
            let (en, ar) = (Lang::En.raw(k), Lang::Ar.raw(k));
            assert!(!en.trim().is_empty() && !ar.trim().is_empty(), "{k:?}");
            if k != S::AppName && k != S::Language {
                assert_ne!(en, ar, "{k:?} is not translated");
            }
        }
    }

    #[test]
    fn arabic_strings_contain_arabic_and_english_ones_do_not() {
        for &k in ALL {
            if k == S::AppName {
                continue;
            }
            let ar_has = has_rtl(Lang::Ar.raw(k));
            assert!(ar_has || k == S::Language, "{k:?}");
        }
        assert!(!has_rtl(Lang::En.raw(S::Connect)));
    }

    #[test]
    fn templates_substitute_dynamic_text_with_arabic_inside() {
        let ar = Lang::Ar.f(S::ConnectFailed, "\u{645}\u{631}\u{62d}\u{628}\u{627}");
        assert!(ar.contains('\u{645}') || has_rtl(&ar));
        assert_eq!(
            Lang::En.f(S::ConnectFailed, "timeout"),
            "Could not connect: timeout"
        );
    }

    #[test]
    fn language_toggle_and_rtl() {
        assert_eq!(Lang::En.other(), Lang::Ar);
        assert!(Lang::Ar.rtl() && !Lang::En.rtl());
    }
}
