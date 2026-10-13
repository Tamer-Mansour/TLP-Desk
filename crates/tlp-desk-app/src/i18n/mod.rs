//! Table-driven i18n. Every user-facing string lives in the [`strings!`] table below, with an
//! English and an Arabic entry per key; the macro makes a missing translation a compile error
//! and `every_key_has_english_and_arabic` fails on an empty or untranslated one.
//!
//! Display pipeline (Arabic is first-class, not an afterthought):
//! * static strings: [`Tx::t`] returns Arabic pre-shaped and in visual order (cached);
//! * templates: [`Tx::f`] substitutes logical-order arguments, then shapes the whole sentence;
//! * dynamic text (device names from the network, IPs, error details): [`Tx::d`];
//! * numbers / IDs / codes / IPs: [`Tx::n`] applies the user's digit style, then shapes.
//!
//! Each piece of text must be shaped **exactly once**; never feed a display string back in.

pub mod format;
pub mod shaping;

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

pub use format::DigitStyle;
pub use shaping::{
    has_rtl, ltr_isolate, shape_for_display, shape_for_edit, visual_for_egui, BaseDir,
};

/// UI language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    En,
    Ar,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::En, Lang::Ar];

    /// Whether layouts must be mirrored for this language.
    pub fn is_rtl(self) -> bool {
        matches!(self, Lang::Ar)
    }

    /// The language's own name (display-ready), shown in the language picker.
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Ar => tr(Lang::Ar, S::LanguageNameAr),
        }
    }

    pub fn parse(code: &str) -> Option<Lang> {
        match code.trim().to_ascii_lowercase().as_str() {
            "en" | "english" => Some(Lang::En),
            "ar" | "arabic" => Some(Lang::Ar),
            _ => None,
        }
    }

    /// Map a BCP-47 locale such as `ar-SA` or `en-US` to a supported language.
    pub fn from_locale(locale: &str) -> Lang {
        let primary = locale.split(['-', '_']).next().unwrap_or("");
        if primary.eq_ignore_ascii_case("ar") {
            Lang::Ar
        } else {
            Lang::En
        }
    }

    /// Language of the OS user interface (first run default).
    pub fn detect() -> Lang {
        sys_locale::get_locale().map_or(Lang::En, |l| Lang::from_locale(&l))
    }

    fn base_dir(self) -> BaseDir {
        if self.is_rtl() {
            BaseDir::Rtl
        } else {
            BaseDir::Ltr
        }
    }
}

macro_rules! strings {
    ($($key:ident => $en:expr, $ar:expr;)*) => {
        /// String keys. Use with [`Tx::t`].
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum S { $($key),* }

        impl S {
            pub const ALL: &'static [S] = &[$(S::$key),*];
        }

        const EN: &[&str] = &[$($en),*];
        const AR: &[&str] = &[$($ar),*];
    };
}

strings! {
    // Brand: kept in Latin script in both languages.
    AppTitle => "TLP-Desk", "TLP-Desk";
    LanguageNameAr => "Arabic", "العربية";
    Language => "Language", "اللغة";

    // Home — this device
    ThisDevice => "This device", "هذا الجهاز";
    DeviceId => "Device ID", "معرّف الجهاز";
    Copy => "Copy", "نسخ";
    Copied => "Copied", "تم النسخ";
    DeviceName => "Name", "الاسم";
    LocalAddress => "Local address", "العنوان المحلي";
    OneTimeCode => "One-time code", "رمز لمرة واحدة";
    NewCode => "New code", "رمز جديد";
    OneTimeCodeHint => "Share it only with someone you trust.", "لا تشاركه إلا مع شخص تثق به.";

    // Home — connect
    ConnectTitle => "Connect to a device", "الاتصال بجهاز";
    AddressHint => "ID, name or IP address", "المعرّف أو الاسم أو عنوان IP";
    Connect => "Connect", "اتصال";
    NearbyDevices => "Devices on this network", "الأجهزة على هذه الشبكة";
    Searching => "Searching for devices…", "جارٍ البحث عن أجهزة…";
    Online => "Available", "متاح";
    Offline => "Unavailable", "غير متاح";
    InputPreview => "Shown as:", "يظهر هكذا:";
    CodeHint => "One-time code (optional)", "رمز لمرة واحدة (اختياري)";
    CodeFieldHint => "Leave it empty to ask the other side to accept.", "اتركه فارغاً ليُطلب من الطرف الآخر الموافقة.";

    // Connection progress / errors
    Connecting => "Connecting to {target}…", "جارٍ الاتصال بـ {target}…";
    WaitingForAccept => "Waiting for the other side to accept…", "بانتظار موافقة الطرف الآخر…";
    Cancel => "Cancel", "إلغاء";
    ConnectFailedTitle => "Could not connect", "تعذّر الاتصال";
    ErrNotFound => "Device not found on this network.", "لم يُعثر على الجهاز في هذه الشبكة.";
    ErrDeclined => "The connection was declined.", "رُفض طلب الاتصال.";
    ErrWrongCode => "Wrong code or password.", "الرمز أو كلمة المرور غير صحيحة.";
    ErrRateLimited => "Too many attempts. Try again later.", "محاولات كثيرة. حاول لاحقاً.";
    ErrNotAllowed => "This device does not accept connections from you.", "هذا الجهاز لا يقبل اتصالات منك.";
    ErrVersion => "The other device runs an incompatible version.", "الجهاز الآخر يعمل بإصدار غير متوافق.";
    ErrUnreachable => "The device cannot be reached.", "تعذّر الوصول إلى الجهاز.";
    ErrIdentity => "Identity check failed. Connection stopped.", "فشل التحقق من الهوية. أُوقف الاتصال.";
    ErrTimeout => "The connection timed out.", "انتهت مهلة الاتصال.";
    ErrOther => "Connection failed: {detail}", "فشل الاتصال: {detail}";
    ErrNotLan => "Only addresses on the local network can be used.", "لا يمكن استخدام إلا عناوين الشبكة المحلية.";
    ErrBadCode => "The one-time code has 6 digits.", "الرمز لمرة واحدة مكوّن من ٦ أرقام.";
    KeyChangedTitle => "Warning: this device's identity has changed", "تحذير: تغيّرت هوية هذا الجهاز";
    KeyChangedBody => "{target} presented a different key than in earlier connections. Someone may be intercepting the connection, or the device was reinstalled. Continue only if you are sure it was reinstalled.", "قدّم {target} مفتاحاً مختلفاً عن المفتاح المحفوظ من الاتصالات السابقة. قد يكون هناك من يعترض الاتصال، أو أُعيد تثبيت الجهاز. لا تتابع إلا إذا كنت متأكداً من إعادة تثبيته.";
    KeyPrevious => "Remembered key", "المفتاح المحفوظ";
    KeyPresented => "Presented key", "المفتاح المقدَّم";
    ForgetKeyAndConnect => "Forget old key and connect", "انسَ المفتاح القديم واتصل";
    ErrInvalidAddress => "Enter a 9-digit ID, a device name or an IP address.", "أدخل معرّفاً من ٩ أرقام أو اسم جهاز أو عنوان IP.";
    Ok => "OK", "حسناً";

    // Incoming request (host side)
    IncomingTitle => "Incoming connection", "طلب اتصال وارد";
    IncomingBody => "{name} wants to view and control this device.", "يريد {name} عرض هذا الجهاز والتحكم به.";
    IdLabel => "ID", "المعرّف";
    AddressLabel => "Address", "العنوان";
    Accept => "Accept", "قبول";
    Decline => "Decline", "رفض";
    HostBanner => "{name} is connected to this device", "{name} متصل بهذا الجهاز";
    EndSession => "End session", "إنهاء الجلسة";
    HostPortInUse => "Port {port} is already in use, probably by another running TLP-Desk. This device cannot accept connections, but you can still connect to other devices.", "المنفذ {port} مستخدم بالفعل، وربما يعمل TLP-Desk مسبقاً. لا يستطيع هذا الجهاز استقبال اتصالات، لكن ما زال بإمكانك الاتصال بأجهزة أخرى.";
    HostUnavailable => "This device cannot accept connections: {detail}", "لا يستطيع هذا الجهاز استقبال اتصالات: {detail}";
    NoNetwork => "Networking is unavailable: {detail}", "الشبكة غير متاحة: {detail}";

    // Session
    SessionWith => "Connected to {name}", "متصل بـ {name}";
    FitToWindow => "Fit", "ملاءمة";
    OriginalSize => "Actual size", "الحجم الفعلي";
    Fullscreen => "Full screen", "ملء الشاشة";
    ExitFullscreen => "Exit full screen", "إنهاء ملء الشاشة";
    ViewOnly => "View only", "عرض فقط";
    Stats => "Stats", "الإحصاءات";
    Disconnect => "Disconnect", "قطع الاتصال";
    WaitingForVideo => "Waiting for video…", "بانتظار الفيديو…";
    StatFps => "Frame rate", "معدل الإطارات";
    StatLatency => "Latency", "زمن الاستجابة";
    StatBitrate => "Bitrate", "معدل البيانات";
    StatResolution => "Resolution", "الدقة";
    StatDuration => "Duration", "المدة";
    UnitFps => "fps", "إطار/ث";
    UnitMs => "ms", "م.ث";
    UnitMbps => "Mbps", "ميغابت/ث";
    UnitBytes => "B", "بايت";
    UnitKB => "KB", "ك.ب";
    UnitMB => "MB", "م.ب";
    UnitGB => "GB", "غ.ب";
    UnitTB => "TB", "ت.ب";
    SessionEnded => "The session has ended.", "انتهت الجلسة.";
    KeysCaptured => "Keyboard goes to the remote PC", "لوحة المفاتيح موجّهة إلى الجهاز البعيد";
    KeysCapturedHint => "Win, Alt+Tab and other system keys are sent to the remote PC while this window is focused.", "تُرسل مفاتيح النظام مثل Win وAlt+Tab إلى الجهاز البعيد ما دامت هذه النافذة نشطة.";
    PermissionViewOnly => "The host allows viewing only.", "المضيف يسمح بالعرض فقط.";
    HostUsingInput => "The person at the other device is using the mouse or keyboard. Your control resumes when they stop.", "الشخص على الجهاز الآخر يستخدم الفأرة أو لوحة المفاتيح. يُستأنف تحكمك عندما يتوقف.";
    Screens => "Screen", "الشاشة";
    ScreenTooltip => "Screen {n}: {res}", "الشاشة {n}: {res}";
    ScreenPrimary => "Main screen", "الشاشة الرئيسية";

    // Settings
    Settings => "Settings", "الإعدادات";
    Back => "Back", "رجوع";
    Digits => "Digits", "الأرقام";
    // Digit ranges are LTR-isolated: after Arabic letters UAX #9 would show "0-9" as "9-0".
    DigitsWestern => "Western (0-9)", "غربية (\u{2066}0-9\u{2069})";
    DigitsArabicIndic => "Arabic-Indic (\u{2066}٠-٩\u{2069})", "هندية (\u{2066}٠-٩\u{2069})";
    DigitsHint => "IDs, codes and addresses are always copied with 0-9 digits.", "تُنسخ المعرّفات والرموز والعناوين دائماً بالأرقام \u{2066}0-9\u{2069}.";
    DeviceNameHint => "Other devices on the network see this name.", "تظهر هذه التسمية للأجهزة الأخرى على الشبكة.";
    Save => "Save", "حفظ";
    DeviceNameRestart => "Restart TLP-Desk to announce the new name on the network.", "أعد تشغيل TLP-Desk لإعلان الاسم الجديد على الشبكة.";
    Appearance => "Language and display", "اللغة والعرض";
    ThisDeviceSettings => "This device", "هذا الجهاز";

    // =======================================================================================
    // FEATURE HOOK: feature lanes add their keys ONLY inside their own section below (English
    // + Arabic on one line, natural Arabic wording, "TLP-Desk" stays in Latin letters). Keeping
    // to your section avoids merge conflicts between parallel branches.
    // =======================================================================================
    // --- files ---

    // --- clipboard ---

    // --- chat ---

    // --- audio ---

    // --- service ---

    // --- privacy ---

    // --- settings/ux ---
    // Settings sections (TLP-settings-tray)
    SettingsIncoming => "Incoming connections", "الاتصالات الواردة";
    SettingsAllowList => "Allowed networks", "الشبكات المسموح بها";
    SettingsQuality => "Picture quality", "جودة الصورة";
    SettingsStartup => "Start-up and notification area", "بدء التشغيل ومنطقة الإشعارات";
    IncomingEnabled => "Accept incoming connections", "استقبال الاتصالات الواردة";
    IncomingPaused => "Incoming connections are paused. Other devices cannot connect to this one.", "الاتصالات الواردة متوقفة مؤقتاً. لا تستطيع الأجهزة الأخرى الاتصال بهذا الجهاز.";
    DefaultPermissions => "What connected devices may do", "ما يُسمح للأجهزة المتصلة بفعله";
    DefaultPermissionsHint => "Applies to the next connection. You can take any of these back during a session.", "يُطبَّق على الاتصال التالي، ويمكنك سحب أي منها أثناء الجلسة.";
    PermView => "See the screen", "رؤية الشاشة";
    PermControl => "Use the mouse", "استخدام الفأرة";
    PermKeyboard => "Use the keyboard", "استخدام لوحة المفاتيح";
    PermClipboard => "Share the clipboard", "مشاركة الحافظة";
    PermFiles => "Transfer files", "نقل الملفات";
    PermAudio => "Hear the sound", "سماع الصوت";
    PermRestart => "Restart this device", "إعادة تشغيل هذا الجهاز";
    PermPrivacy => "Privacy mode", "وضع الخصوصية";
    PermChat => "Chat", "المحادثة";
    PermTunnel => "Port forwarding", "تمرير المنافذ";
    PermWhiteboard => "Draw on the screen", "الرسم على الشاشة";
    PermSystem => "Ctrl+Alt+Del and locking", "\u{2066}Ctrl+Alt+Del\u{2069} وقفل الجهاز";
    AcceptTimeout => "Decline unanswered requests after", "رفض الطلبات التي لم يُرَدّ عليها بعد";
    UnitSeconds => "s", "ث";
    AllowOneTimeCode => "Allow connecting with the one-time code", "السماح بالاتصال بالرمز لمرة واحدة";
    LocalGrace => "Pause remote control after I use my own mouse or keyboard for", "إيقاف التحكم عن بُعد مؤقتاً بعد استخدامي للفأرة أو لوحة المفاتيح لمدة";
    PointerMarker => "Show where the remote pointer is on this screen", "إظهار موضع مؤشر الطرف البعيد على هذه الشاشة";
    AllowListHint => "Only devices whose address is inside one of these ranges can connect to this device.", "لا يمكن الاتصال بهذا الجهاز إلا من أجهزة تقع عناوينها ضمن أحد هذه النطاقات.";
    AllowListPlaceholder => "e.g. 192.168.1.0/24", "مثال: \u{2066}192.168.1.0/24\u{2069}";
    AllowListAdd => "Add", "إضافة";
    AllowListRemove => "Remove", "إزالة";
    AllowListReset => "Restore defaults", "استعادة الافتراضي";
    AllowListInvalid => "Enter an address or a range such as 192.168.1.0/24.", "أدخل عنواناً أو نطاقاً مثل \u{2066}192.168.1.0/24\u{2069}.";
    AllowListDuplicate => "This range is already in the list.", "هذا النطاق موجود في القائمة.";
    AllowListPublic => "{range} includes addresses outside the local network. Add it only if you are sure.", "يتضمن النطاق {range} عناوين خارج الشبكة المحلية. لا تضفه إلا إذا كنت متأكداً.";
    AllowListAddAnyway => "Add anyway", "إضافة رغم ذلك";
    AllowListLast => "At least one range is needed. To refuse everyone, turn off incoming connections.", "يلزم نطاق واحد على الأقل. لرفض الجميع أوقف استقبال الاتصالات الواردة.";
    QualitySpeed => "Speed", "السرعة";
    QualityBalanced => "Balanced", "متوازن";
    QualityBest => "Quality", "الجودة";
    QualityHint => "Used when you connect to another device.", "يُستخدم عند اتصالك بجهاز آخر.";
    StartWithWindows => "Start TLP-Desk when I sign in to Windows", "تشغيل TLP-Desk عند تسجيل الدخول إلى Windows";
    StartMinimized => "Start hidden in the notification area", "البدء مخفياً في منطقة الإشعارات";
    CloseToTray => "Keep running in the notification area when I close the window", "الاستمرار في العمل في منطقة الإشعارات عند إغلاق النافذة";
    AutostartFailed => "Could not change the start-up setting: {detail}", "تعذّر تغيير إعداد بدء التشغيل: {detail}";
    // Tray (TLP-settings-tray)
    TrayOpen => "Open TLP-Desk", "فتح TLP-Desk";
    TrayCopyId => "Copy ID", "نسخ المعرّف";
    TrayNewCode => "New one-time code", "رمز جديد لمرة واحدة";
    TrayQuit => "Quit", "إنهاء";
    TrayTooltip => "TLP-Desk — ID {id}", "TLP-Desk — المعرّف {id}";
    TrayTooltipPaused => "TLP-Desk — ID {id} (incoming paused)", "TLP-Desk — المعرّف {id} (الاتصالات الواردة متوقفة)";
    TrayTooltipRequest => "{name} wants to connect", "يطلب {name} الاتصال";
    // Host banner (TLP-settings-tray)
    BannerMore => "and {n} more", "و{n} آخرون";
    // Connection log (TLP-settings-tray)
    ConnectionLog => "Connection log", "سجل الاتصالات";
    LogSearchHint => "Search by name, ID or address", "ابحث بالاسم أو المعرّف أو العنوان";
    LogFilterAll => "All", "الكل";
    LogIncoming => "Incoming", "وارد";
    LogOutgoing => "Outgoing", "صادر";
    LogFilterRefused => "Refused", "مرفوض";
    LogExport => "Export CSV", "تصدير \u{2066}CSV\u{2069}";
    LogExported => "Saved to {path}", "حُفظ في {path}";
    LogExportFailed => "Could not export: {detail}", "تعذّر التصدير: {detail}";
    LogEmpty => "No connections yet.", "لا توجد اتصالات بعد.";
    LogNoMatch => "Nothing matches the search.", "لا توجد نتائج مطابقة.";
    LogRefresh => "Refresh", "تحديث";
    LogCount => "Entries: {n}", "عدد السجلات: {n}";
    LogColTime => "When", "الوقت";
    LogColDirection => "Direction", "الاتجاه";
    LogColFingerprint => "Key fingerprint", "بصمة المفتاح";
    LogColAuth => "Authorized by", "طريقة التفويض";
    LogColPermissions => "Permissions", "الصلاحيات";
    LogColEnd => "Ended", "سبب الانتهاء";
    LogAuthAccept => "Accepted on screen", "قبول على الشاشة";
    LogAuthPassword => "Password", "كلمة المرور";
    LogUnknown => "Unknown", "غير معروف";
    LogEndClosedHere => "Ended on this device", "أُنهيت من هذا الجهاز";
    LogEndClosedRemotely => "Ended by the other side", "أنهاها الطرف الآخر";
    LogEndRevoked => "Permission withdrawn", "سُحبت الصلاحية";
    LogEndError => "Connection lost", "انقطع الاتصال";
    LogEndAppExit => "TLP-Desk was closed", "أُغلق TLP-Desk";
    LogEndDeclined => "Declined", "رُفض الطلب";
    LogEndTimedOut => "Not answered", "لم يُرَدّ على الطلب";
    LogEndWrongCredentials => "Wrong code or password", "رمز أو كلمة مرور خاطئة";
    LogEndRateLimited => "Too many attempts", "محاولات كثيرة";
    LogEndNotAllowed => "Not allowed", "غير مسموح";

    // --- perf ---

    // --- power ---
}

/// Display-ready (pre-shaped) Arabic table, built once.
fn shaped_ar() -> &'static [String] {
    static SHAPED: OnceLock<Vec<String>> = OnceLock::new();
    SHAPED.get_or_init(|| {
        AR.iter()
            .map(|s| shape_for_display(s, BaseDir::Rtl).into_owned())
            .collect()
    })
}

/// English table prepared the same way (strips bidi controls, orders embedded Arabic).
fn shaped_en() -> &'static [String] {
    static SHAPED: OnceLock<Vec<String>> = OnceLock::new();
    SHAPED.get_or_init(|| {
        EN.iter()
            .map(|s| shape_for_display(s, BaseDir::Ltr).into_owned())
            .collect()
    })
}

/// Display-ready string for `key` (Arabic is in visual order for egui).
pub fn tr(lang: Lang, key: S) -> &'static str {
    match lang {
        Lang::En => shaped_en()[key as usize].as_str(),
        Lang::Ar => shaped_ar()[key as usize].as_str(),
    }
}

/// Raw (logical-order, unshaped) string. Use for text the OS renders itself (window title) or
/// as input to a template that is shaped afterwards.
pub fn tr_raw(lang: Lang, key: S) -> &'static str {
    match lang {
        Lang::En => EN[key as usize],
        Lang::Ar => AR[key as usize],
    }
}

/// Display-ready string with `{name}` placeholders substituted, shaped after substitution so
/// that user content (e.g. an Arabic device name) is ordered correctly within the sentence.
pub fn tr_fmt(lang: Lang, key: S, args: &[(&str, &str)]) -> String {
    let mut s = tr_raw(lang, key).to_owned();
    for (name, value) in args {
        s = s.replace(&format!("{{{name}}}"), value);
    }
    shape_for_display(&s, lang.base_dir()).into_owned()
}

/// The one display helper the UI uses for all text. Small and `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tx {
    pub lang: Lang,
    pub digits: DigitStyle,
}

impl Tx {
    pub fn new(lang: Lang, digits: DigitStyle) -> Self {
        Self { lang, digits }
    }

    /// Static UI string, display-ready.
    pub fn t(self, key: S) -> &'static str {
        tr(self.lang, key)
    }

    /// Logical-order static string (for OS-rendered text or further templating).
    pub fn raw(self, key: S) -> &'static str {
        tr_raw(self.lang, key)
    }

    /// Template with logical-order arguments, display-ready.
    pub fn f(self, key: S, args: &[(&str, &str)]) -> String {
        tr_fmt(self.lang, key, args)
    }

    /// Dynamic text (device names, error details…), display-ready. Direction comes from the
    /// text itself.
    pub fn d(self, text: &str) -> String {
        shape_for_display(text, BaseDir::Auto).into_owned()
    }

    /// Logical-order text in the UI paragraph direction (e.g. a "number unit" pair), display-ready.
    pub fn p(self, text: &str) -> String {
        shape_for_display(text, self.lang.base_dir()).into_owned()
    }

    /// Numbers, IDs, codes, IPs for use *inside* a template or sentence: digit style applied
    /// and wrapped in an LTR isolate so digit groups keep their order in RTL text.
    pub fn digits(self, text: &str) -> String {
        if has_rtl(text) {
            return text.to_owned(); // a name, not a number
        }
        ltr_isolate(&format::localize_digits(text, self.digits))
    }

    /// Standalone number, ID, code or IP: digit style applied, display-ready. Always laid out
    /// left-to-right (digit groups are never reordered).
    pub fn n(self, text: &str) -> String {
        if text
            .chars()
            .any(|c| c.is_alphabetic() && has_rtl(&c.to_string()))
        {
            return self.d(text);
        }
        let localized = format::localize_digits(text, self.digits);
        visual_for_egui(&localized).into_owned()
    }

    pub fn rtl(self) -> bool {
        self.lang.is_rtl()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keys whose text is intentionally identical in both languages (brand names).
    const SAME_IN_ALL: &[S] = &[S::AppTitle];

    #[test]
    fn every_key_has_english_and_arabic() {
        assert_eq!(EN.len(), S::ALL.len());
        assert_eq!(AR.len(), S::ALL.len());
        for (i, key) in S::ALL.iter().enumerate() {
            assert_eq!(*key as usize, i, "key order mismatch for {key:?}");
            let en = tr_raw(Lang::En, *key);
            let ar = tr_raw(Lang::Ar, *key);
            assert!(!en.trim().is_empty(), "{key:?} has empty English text");
            assert!(!ar.trim().is_empty(), "{key:?} has empty Arabic text");
            if !SAME_IN_ALL.contains(key) {
                assert!(
                    has_rtl(ar),
                    "{key:?} Arabic text contains no Arabic: {ar:?}"
                );
                // English may mention the Arabic digits by example only.
                if *key != S::DigitsArabicIndic {
                    assert!(!has_rtl(en), "{key:?} English text contains Arabic: {en:?}");
                }
            }
        }
    }

    #[test]
    fn placeholders_match_between_languages() {
        fn placeholders(s: &str) -> Vec<String> {
            let mut v: Vec<String> = s
                .split('{')
                .skip(1)
                .filter_map(|p| p.split_once('}').map(|(n, _)| n.to_owned()))
                .collect();
            v.sort();
            v
        }
        for key in S::ALL {
            assert_eq!(
                placeholders(tr_raw(Lang::En, *key)),
                placeholders(tr_raw(Lang::Ar, *key)),
                "placeholder mismatch for {key:?}"
            );
        }
    }

    #[test]
    fn arabic_display_strings_are_prepared_once() {
        assert_eq!(tr(Lang::En, S::Connect), "Connect");
        for key in S::ALL {
            let raw = tr_raw(Lang::Ar, *key);
            let shown = tr(Lang::Ar, *key);
            // Display text is a permutation of the source (egui does the glyph shaping).
            // Bidi control characters are consumed by the display pipeline.
            let mut a: Vec<char> = raw
                .chars()
                .filter(|c| !matches!(*c as u32, 0x2066..=0x2069))
                .collect();
            let mut b: Vec<char> = shown.chars().collect();
            a.sort();
            b.sort();
            assert_eq!(a.len(), b.len(), "{key:?}");
        }
    }

    #[test]
    fn digit_ranges_keep_their_order() {
        for key in [S::DigitsWestern, S::DigitsHint] {
            assert!(
                tr(Lang::Ar, key).contains("0-9"),
                "{key:?}: {:?}",
                tr(Lang::Ar, key)
            );
        }
    }

    #[test]
    fn format_substitutes_and_shapes() {
        let en = tr_fmt(Lang::En, S::HostBanner, &[("name", "Office-PC")]);
        assert_eq!(en, "Office-PC is connected to this device");
        let ar = tr_fmt(Lang::Ar, S::HostBanner, &[("name", "Office-PC")]);
        assert!(ar.contains("Office-PC"));
        assert!(!ar.contains('{'));
        // RTL paragraph: name is logically first, so it is visually last (rightmost).
        assert!(ar.ends_with("Office-PC"), "{ar:?}");
    }

    #[test]
    fn digit_style_applies_to_numbers_only() {
        let tx = Tx::new(Lang::Ar, DigitStyle::ArabicIndic);
        // Screen order is verified with real egui layout in shaping::tests.
        assert_eq!(tx.n("482 913 057"), visual_for_egui("٤٨٢ ٩١٣ ٠٥٧"));
        assert_eq!(
            Tx::new(Lang::Ar, DigitStyle::Western).n("192.168.1.2"),
            "192.168.1.2"
        );
    }

    #[test]
    fn lang_parse_and_locale_detection() {
        assert_eq!(Lang::parse("AR"), Some(Lang::Ar));
        assert_eq!(Lang::parse("en"), Some(Lang::En));
        assert_eq!(Lang::parse("fr"), None);
        assert_eq!(Lang::from_locale("ar-SA"), Lang::Ar);
        assert_eq!(Lang::from_locale("ar_EG"), Lang::Ar);
        assert_eq!(Lang::from_locale("en-US"), Lang::En);
        assert_eq!(Lang::from_locale("fr-FR"), Lang::En);
    }
}
