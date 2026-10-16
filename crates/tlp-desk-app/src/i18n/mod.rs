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

mod bidi_line;
pub mod format;
pub mod shaping;

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

pub use bidi_line::{bidi_line, BidiLine};
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
    HostInputBlocked => "Remote typing and clicks are blocked by a window running as administrator (or a secure screen) on this PC. Switch to a normal window, or run TLP-Desk as a service.", "الكتابة والنقر عن بُعد محجوبان بسبب نافذة تعمل بصلاحيات المسؤول (أو شاشة آمنة) على هذا الجهاز. انتقل إلى نافذة عادية أو شغّل TLP-Desk كخدمة.";
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
    AllScreens => "All screens", "كل الشاشات";
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
    FsFiles => "Files: browse and copy between the two PCs", "الملفات: تصفّح وانسخ بين الجهازين";
    FsTitle => "Files", "الملفات";
    FsClose => "Close files", "إغلاق الملفات";
    FsThisPc => "This PC", "هذا الجهاز";
    FsRemotePc => "Remote PC", "الجهاز البعيد";
    FsDrives => "Drives and folders", "الأقراص والمجلدات";
    FsForward => "Forward", "إلى الأمام";
    FsUp => "Up one folder", "المجلد الأعلى";
    FsRefresh => "Refresh", "تحديث";
    FsNewFolder => "New folder", "مجلد جديد";
    FsRename => "Rename", "إعادة تسمية";
    FsDelete => "Delete", "حذف";
    FsUpload => "Copy to the remote PC", "نسخ إلى الجهاز البعيد";
    FsDownload => "Copy to this PC", "نسخ إلى هذا الجهاز";
    FsName => "Name", "الاسم";
    FsSize => "Size", "الحجم";
    FsModified => "Modified", "آخر تعديل";
    FsSortHint => "Sort by this column (click again to reverse)", "رتّب حسب هذا العمود (انقر مرة أخرى لعكس الترتيب)";
    FsEmpty => "This folder is empty.", "هذا المجلد فارغ.";
    FsLoading => "Loading…", "جارٍ التحميل…";
    FsLocalDisk => "Local disk", "قرص محلي";
    FsHome => "Personal folder", "المجلد الشخصي";
    FsDesktop => "Desktop", "سطح المكتب";
    FsDocuments => "Documents", "المستندات";
    FsDownloads => "Downloads", "التنزيلات";
    FsPictures => "Pictures", "الصور";
    FsMusic => "Music", "الموسيقى";
    FsVideos => "Videos", "مقاطع الفيديو";
    FsFreeOf => "{free} free of {total}", "{free} متاحة من {total}";
    FsCreate => "Create", "إنشاء";
    FsRenameTitle => "Rename “{name}”", "إعادة تسمية «{name}»";
    FsInvalidName => "This name cannot be used for a file or folder.", "لا يمكن استخدام هذا الاسم لملف أو مجلد.";
    FsDeleteTitle => "Delete permanently?", "حذف نهائي؟";
    FsDeleteBody => "{n} item(s) will be deleted permanently from {where}, including everything inside folders. This cannot be undone.", "سيُحذف {n} من العناصر نهائياً من {where}، مع كل ما في داخل المجلدات. لا يمكن التراجع عن ذلك.";
    FsDeleteConfirm => "Delete permanently", "احذف نهائياً";
    FsPickFolder => "Open a folder in both panes first.", "افتح مجلداً في الجهتين أولاً.";
    FsPasteRemote => "Paste files copied on the remote PC", "الصق الملفات المنسوخة على الجهاز البعيد";
    FsPasteRemoteHint => "{n} item(s) were copied on the remote PC. They are pasted into the folder open on this PC (or Downloads).", "نُسخ {n} من العناصر على الجهاز البعيد. تُلصق في المجلد المفتوح على هذا الجهاز (أو في التنزيلات).";
    FsPasteLocal => "Paste files copied on this PC", "الصق الملفات المنسوخة على هذا الجهاز";
    FsTransferStarted => "Copying {n} item(s)…", "جارٍ نسخ {n} من العناصر…";
    FsNothingToPaste => "No files are copied.", "لا توجد ملفات منسوخة.";
    FsErrNotFound => "It no longer exists.", "لم يعد موجوداً.";
    FsErrDenied => "Access denied.", "تم رفض الوصول.";
    FsErrExists => "An item with this name already exists.", "يوجد عنصر بهذا الاسم بالفعل.";
    FsErrNotEmpty => "The folder is not empty.", "المجلد ليس فارغاً.";
    FsErrInvalidPath => "This path cannot be used.", "لا يمكن استخدام هذا المسار.";
    FsErrNoSpace => "The disk is full.", "القرص ممتلئ.";
    FsErrBusy => "The file is in use by another program.", "الملف قيد الاستخدام في برنامج آخر.";
    FsErrNoPermission => "The remote PC does not allow file access in this session.", "لا يسمح الجهاز البعيد بالوصول إلى الملفات في هذه الجلسة.";
    FsErrTimeout => "The remote PC did not answer in time.", "لم يستجب الجهاز البعيد في الوقت المحدد.";
    FsErrTransfers => "Copying files is not available in this version yet.", "نسخ الملفات غير متاح في هذا الإصدار بعد.";
    FsErrOther => "Error: {detail}", "خطأ: {detail}";
    CloseTransfers => "Hide the transfer list", "إخفاء قائمة النقل";
    FilesPanelTitle => "File transfer", "نقل الملفات";
    SendFiles => "Send files…", "إرسال ملفات…";
    SendFolder => "Send folder…", "إرسال مجلد…";
    DropToSend => "Drop to send to {name}", "أفلت الملفات لإرسالها إلى {name}";
    NoTransfers => "No transfers yet. Drag files onto the window or use the buttons.", "لا توجد عمليات نقل بعد. اسحب الملفات إلى النافذة أو استخدم الأزرار.";
    TransferPreparing => "Preparing…", "جارٍ التحضير…";
    TransferPaused => "Paused", "متوقف مؤقتاً";
    TransferDone => "Done", "اكتمل";
    TransferDoneFailures => "Done, {n} file(s) failed", "اكتمل، وتعذّر نقل {n} ملف";
    TransferCancelled => "Cancelled", "أُلغي";
    TransferLost => "Connection lost", "انقطع الاتصال";
    TransferFailed => "Failed: {reason}", "فشل: {reason}";
    TransferFiles => "{done} of {total} files", "{done} من {total} ملف";
    TransferRate => "{size}/s", "{size}/ث";
    TransferEta => "{time} left", "المتبقي {time}";
    TransferRetry => "Retry (continues where it stopped)", "إعادة المحاولة (تكمل من حيث توقفت)";
    TransferCancel => "Cancel transfer", "إلغاء النقل";
    OpenFolder => "Open folder", "فتح المجلد";
    ClearFinished => "Clear finished", "مسح المنتهية";
    FilesNotAllowed => "The host has not allowed file transfer for this session.", "لم يسمح المضيف بنقل الملفات في هذه الجلسة.";
    FailNoSpace => "not enough free disk space", "لا توجد مساحة كافية على القرص";
    FailDeclined => "the other side declined", "رفض الطرف الآخر";
    FailHash => "data was corrupted in transit", "تلفت البيانات أثناء النقل";
    FailPath => "invalid file name or path", "اسم الملف أو مساره غير صالح";
    FailPermission => "not allowed", "غير مسموح";
    FailNotFound => "file not found", "الملف غير موجود";
    FailBusy => "file is in use", "الملف قيد الاستخدام";

    // --- clipboard ---
    ClipboardSync => "Sync clipboard", "مزامنة الحافظة";
    ClipboardNotAllowed => "The other device does not allow clipboard sync.", "لا يسمح الجهاز الآخر بمزامنة الحافظة.";
    ClipboardSent => "Clipboard sent", "أُرسلت الحافظة";
    ClipboardReceived => "Clipboard received", "استُلمت الحافظة";
    ClipboardTooLarge => "The clipboard content is too large to sync.", "محتوى الحافظة أكبر من أن تتم مزامنته.";
    ClipboardFailed => "The clipboard could not be synced.", "تعذّرت مزامنة الحافظة.";
    ClipboardSettings => "Clipboard", "الحافظة";
    ClipboardSyncDefault => "Sync the clipboard in new sessions", "مزامنة الحافظة في الجلسات الجديدة";
    ClipboardSyncHint => "Copied text, formatted text and images move between both devices while the host allows it.", "ينتقل النص المنسوخ والنص المنسّق والصور بين الجهازين ما دام المضيف يسمح بذلك.";

    // --- chat ---
    Chat => "Chat", "الدردشة";
    ChatWith => "Chat with {name}", "دردشة مع {name}";
    ChatInputHint => "Write a message…", "اكتب رسالة…";
    ChatSend => "Send", "إرسال";
    ChatSendHint => "Enter sends, Shift+Enter starts a new line.", "يرسل Enter الرسالة، ويبدأ Shift+Enter سطراً جديداً.";
    ChatEmpty => "No messages yet. Say hello!", "لا توجد رسائل بعد. ابدأ بالتحية!";
    ChatOffViewer => "The other side has turned chat off.", "أوقف الطرف الآخر الدردشة.";
    ChatOffHost => "Chat is turned off for this session.", "الدردشة متوقفة في هذه الجلسة.";
    ChatTurnOff => "Turn off chat", "إيقاف الدردشة";
    ChatTurnOn => "Allow chat", "السماح بالدردشة";
    ChatTooLong => "Too long: at most {max} characters.", "الرسالة طويلة: الحد الأقصى {max} حرف.";
    ChatCount => "{count} / {max}", "{count} / {max} حرف";
    ChatRateLimited => "You are sending too fast. Wait a moment.", "ترسل بسرعة كبيرة. انتظر لحظة.";
    ChatUnavailable => "Chat is not available in this session.", "الدردشة غير متاحة في هذه الجلسة.";
    ChatUnread => "{count} unread", "{count} غير مقروءة";
    TimeAm => "AM", "ص";
    TimePm => "PM", "م";

    // --- audio ---
    AudioMute => "Mute the remote sound", "كتم صوت الجهاز البعيد";
    AudioUnmute => "Play the remote sound", "تشغيل صوت الجهاز البعيد";
    AudioNotAllowed => "The other device does not share its sound.", "لا يشارك الجهاز الآخر صوته.";
    AudioVolume => "Volume", "مستوى الصوت";
    AudioDeviceFailed => "The remote sound cannot be played: no audio output device is available.", "تعذّر تشغيل صوت الجهاز البعيد: لا يتوفر جهاز لإخراج الصوت.";
    TypingSettings => "Typing", "الكتابة";
    TypeWithMyLayout => "Type text using my keyboard layout", "الكتابة بتخطيط لوحة المفاتيح لدي";
    TypeWithMyLayoutHint => "Letters are sent as text, so Arabic typed here appears as Arabic even if the other PC uses an English layout. Shortcuts and special keys still work as usual.", "تُرسل الحروف نصاً، فيظهر ما تكتبه بالعربية هنا عربياً حتى لو كان تخطيط الجهاز الآخر إنجليزياً. وتبقى الاختصارات والمفاتيح الخاصة تعمل كالمعتاد.";
    AudioSettings => "Sound", "الصوت";
    AudioOnConnect => "Play the remote computer's sound when connecting", "تشغيل صوت الجهاز البعيد عند الاتصال";
    AudioOnConnectHint => "Sound plays only if the other device allows it. You can mute it at any time from the session toolbar.", "لا يُشغَّل الصوت إلا إذا سمح به الجهاز الآخر، ويمكنك كتمه في أي وقت من شريط أدوات الجلسة.";

    // --- service ---
    ServiceDisplayName => "TLP-Desk Service", "خدمة TLP-Desk";
    ServiceDescription => "Lets trusted devices on your local network reach this computer with TLP-Desk, including at the lock and sign-in screens (unattended access).", "تتيح للأجهزة الموثوقة في شبكتك المحلية الوصول إلى هذا الحاسوب عبر TLP-Desk، حتى عند شاشة القفل وشاشة تسجيل الدخول (الوصول غير المراقَب).";
    ServiceHosting => "Unattended access is on: the TLP-Desk service receives connections for this device.", "الوصول غير المراقَب مفعّل: خدمة TLP-Desk تستقبل الاتصالات لهذا الجهاز.";
    RemoteLocked => "The remote computer is locked. Use Ctrl+Alt+Del to sign in.", "الحاسوب البعيد مقفل. استخدم Ctrl+Alt+Del لتسجيل الدخول.";
    RemoteSecureDesktop => "The remote computer shows a secure screen (sign-in, lock or an administrator prompt).", "يعرض الحاسوب البعيد شاشة آمنة (تسجيل الدخول أو القفل أو طلب صلاحيات المسؤول).";

    // --- privacy ---
    PrivacyMode => "Privacy mode", "وضع الخصوصية";
    PrivacyTurnOff => "Turn off privacy mode", "إيقاف وضع الخصوصية";
    PrivacyNotAllowed => "The other device has not allowed privacy mode.", "لم يسمح الجهاز الآخر بوضع الخصوصية.";
    PrivacyConfirmTitle => "Turn on privacy mode?", "تشغيل وضع الخصوصية؟";
    // Short sentences: RTL labels do not wrap (see theme::apply_lang).
    PrivacyConfirmScreen => "The screen of {name} turns black and its keyboard and mouse stop working.", "ستصبح شاشة {name} سوداء وتتوقف لوحة مفاتيحه وفأرته.";
    PrivacyConfirmControl => "You keep seeing and controlling it.", "وتبقى أنت قادراً على رؤيته والتحكم به.";
    PrivacyConfirmExit => "Someone there can end it: hold Ctrl+Alt+Shift+P for 3 s, or press Ctrl+Alt+Del.", "ويمكن لمن عنده إنهاؤه بالضغط مطولاً على Ctrl+Alt+Shift+P لمدة ٣ ثوانٍ، أو Ctrl+Alt+Del.";
    PrivacyTurnOn => "Turn on", "تشغيل";
    PrivacyOnBanner => "Privacy mode is on: the other screen is black, its keyboard and mouse are blocked.", "وضع الخصوصية مفعّل: شاشة الجهاز الآخر سوداء ولوحة مفاتيحه وفأرته معطّلة.";
    PrivacyEnded => "Privacy mode is off: ended on the other device, or not available there.", "وضع الخصوصية متوقف: أُنهي من الجهاز الآخر أو أنه غير متاح هناك.";
    PrivacyHostOn => "Privacy mode is on", "وضع الخصوصية مفعّل";
    // Shown on the black privacy screen of the host, always in both languages.
    PrivacyScreenTitle => "This PC is being used remotely with TLP-Desk", "هذا الحاسوب قيد الاستخدام عن بُعد عبر TLP-Desk";
    PrivacyScreenViewer => "Connected: {name} ({id})", "متصل: {name} ({id})";
    PrivacyScreenExit => "To end privacy mode here, hold Ctrl+Alt+Shift+P for 3 seconds or press Ctrl+Alt+Del.", "لإنهاء وضع الخصوصية من هنا، اضغط مطولاً على Ctrl+Alt+Shift+P لمدة ٣ ثوانٍ أو اضغط Ctrl+Alt+Del.";

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

    // --- unattended ---
    UnattendedTitle => "Unattended access", "الوصول دون مرافقة";
    UnattendedHint => "With a permanent password you can connect to this device when nobody is there to accept.", "بكلمة مرور دائمة يمكنك الاتصال بهذا الجهاز حتى لو لم يكن أحد بجانبه ليوافق.";
    PasswordIsSet => "A permanent password is set.", "تم تعيين كلمة مرور دائمة.";
    PasswordNotSet => "No permanent password is set.", "لم تُعيَّن كلمة مرور دائمة.";
    CurrentPassword => "Current password", "كلمة المرور الحالية";
    NewPassword => "New password", "كلمة المرور الجديدة";
    RepeatPassword => "Repeat the new password", "أعد كتابة كلمة المرور الجديدة";
    SetPassword => "Set password", "تعيين كلمة المرور";
    ChangePassword => "Change password", "تغيير كلمة المرور";
    RemovePassword => "Remove password", "إزالة كلمة المرور";
    ShowPassword => "Show password", "إظهار كلمة المرور";
    HidePassword => "Hide password", "إخفاء كلمة المرور";
    PasswordStrength => "Strength: {level}", "القوة: {level}";
    StrengthTooShort => "too short (at least {n} characters)", "قصيرة جداً (\u{2066}{n}\u{2069} أحرف على الأقل)";
    StrengthWeak => "weak", "ضعيفة";
    StrengthFair => "fair", "مقبولة";
    StrengthStrong => "strong", "قوية";
    PasswordsDiffer => "The two new passwords are not the same.", "كلمتا المرور الجديدتان غير متطابقتين.";
    PasswordSaved => "The permanent password was saved.", "حُفظت كلمة المرور الدائمة.";
    PasswordRemoved => "The permanent password was removed. Password sign-in is off.", "أُزيلت كلمة المرور الدائمة. تسجيل الدخول بكلمة المرور متوقف.";
    ErrCurrentPassword => "The current password is wrong.", "كلمة المرور الحالية غير صحيحة.";
    ErrPasswordSave => "The password could not be saved: {detail}", "تعذّر حفظ كلمة المرور: {detail}";
    ErrPasswordUnavailable => "This device cannot accept connections, so no password can be set.", "لا يستطيع هذا الجهاز استقبال اتصالات، لذا لا يمكن تعيين كلمة مرور.";
    PasswordAdminNote => "TLP-Desk runs as administrator: the current password is not needed.", "يعمل TLP-Desk بصلاحيات المسؤول: لا حاجة إلى كلمة المرور الحالية.";
    PasswordUserScope => "Saved for your Windows account only, because TLP-Desk is not running as administrator. To connect before anyone signs in, install the TLP-Desk service.", "حُفظت لحساب Windows الخاص بك فقط لأن TLP-Desk لا يعمل بصلاحيات المسؤول. للاتصال قبل أن يسجّل أحد الدخول، ثبّت خدمة TLP-Desk.";
    SignInWith => "Sign in with", "تسجيل الدخول بواسطة";
    SignInCode => "One-time code", "رمز لمرة واحدة";
    SignInPassword => "Password", "كلمة المرور";
    PasswordFieldHint => "The other device's permanent password", "كلمة المرور الدائمة للجهاز الآخر";
    PasswordFieldNote => "The password is never sent: both sides only prove they know it.", "لا تُرسل كلمة المرور أبداً: يثبت الطرفان فقط أنهما يعرفانها.";
    ErrPasswordEmpty => "Enter the password.", "أدخل كلمة المرور.";
    ErrLockedOut => "Too many wrong attempts. You are locked out for {minutes} minutes.", "محاولات خاطئة كثيرة. تم إيقافك لمدة \u{2066}{minutes}\u{2069} دقائق.";
    SystemMenu => "System", "النظام";
    RestartRemote => "Restart remote PC", "إعادة تشغيل الجهاز البعيد";
    LockRemote => "Lock remote PC", "قفل الجهاز البعيد";
    SendCtrlAltDel => "Send Ctrl+Alt+Del", "إرسال Ctrl+Alt+Del";
    CtrlAltDelHint => "Needs the TLP-Desk service on the remote PC.", "يتطلب خدمة TLP-Desk على الجهاز البعيد.";
    SystemNotAllowed => "The other device does not allow this.", "الجهاز الآخر لا يسمح بذلك.";
    RestartConfirmTitle => "Restart the remote PC?", "إعادة تشغيل الجهاز البعيد؟";
    RestartConfirmBody => "All programs on {name} will be closed and unsaved work will be lost. TLP-Desk reconnects automatically when the PC is back.", "ستُغلق كل البرامج على {name} وسيضيع العمل غير المحفوظ. يعيد TLP-Desk الاتصال تلقائياً عند عودة الجهاز.";
    RestartConfirm => "Restart", "إعادة التشغيل";
    LockConfirmTitle => "Lock the remote PC?", "قفل الجهاز البعيد؟";
    LockConfirmBody => "{name} will be locked. Whoever uses it must sign in again.", "سيُقفل {name}. على من يستخدمه تسجيل الدخول من جديد.";
    LockConfirm => "Lock", "قفل";
    RestartRequested => "Restart requested. Waiting for the remote PC to go down…", "طُلبت إعادة التشغيل. بانتظار توقف الجهاز البعيد…";
    Restarting => "Restarting… waiting for the remote PC", "جارٍ إعادة التشغيل… بانتظار الجهاز البعيد";
    RestartingAttempt => "Reconnect attempt {n}", "محاولة إعادة الاتصال {n}";
    RestartingNote => "Before anyone signs in, this works only if the TLP-Desk service is installed on the remote PC.", "قبل أن يسجّل أحد الدخول، لا يعمل هذا إلا إذا كانت خدمة TLP-Desk مثبتة على الجهاز البعيد.";
    StopWaiting => "Stop waiting", "إيقاف الانتظار";
    ReconnectFailedText => "The remote PC did not come back: {detail}", "لم يعد الجهاز البعيد: {detail}";
    // --- address book + Wake-on-LAN (TLP-addressbook) ---
    AbTitle => "Address book", "دفتر العناوين";
    AbSearchHint => "Search by name, group, ID or IP", "ابحث بالاسم أو المجموعة أو المعرّف أو IP";
    AbEmpty => "No saved devices yet. Save one from the devices on this network or after connecting to it.", "لا توجد أجهزة محفوظة بعد. احفظ جهازاً من الأجهزة على هذه الشبكة أو بعد الاتصال به.";
    AbNoMatches => "No saved device matches your search.", "لا يوجد جهاز محفوظ يطابق بحثك.";
    AbAllGroups => "All devices", "كل الأجهزة";
    AbFavorites => "Favorites", "المفضّلة";
    AbGroup => "Group", "المجموعة";
    AbSortName => "Name", "الاسم";
    AbSortLastSeen => "Last seen", "آخر ظهور";
    AbSortBy => "Sort", "الترتيب";
    AbAdd => "Add", "إضافة";
    AbEdit => "Edit", "تعديل";
    AbDelete => "Delete", "حذف";
    AbSaveDevice => "Save to address book", "حفظ في دفتر العناوين";
    AbSavedDevice => "In the address book", "محفوظ في دفتر العناوين";
    AbSavedToast => "{name} was saved to the address book.", "حُفظ {name} في دفتر العناوين.";
    AbFavoriteAdd => "Add to favorites", "إضافة إلى المفضّلة";
    AbFavoriteRemove => "Remove from favorites", "إزالة من المفضّلة";
    AbWake => "Wake up", "إيقاظ";
    AbWakeTooltip => "Send a Wake-on-LAN magic packet to switch this PC on", "إرسال حزمة إيقاظ عبر الشبكة لتشغيل هذا الجهاز";
    AbWakeNoMac => "The MAC address is unknown. Connect once while the PC is on, or add it with Edit.", "عنوان MAC غير معروف. اتصل بالجهاز مرة وهو يعمل، أو أضفه من «تعديل».";
    AbWakeSent => "Magic packet sent to {name}. It can take a minute to start.", "أُرسلت حزمة الإيقاظ إلى {name}. قد يستغرق التشغيل دقيقة.";
    AbWakeFailed => "Could not send the magic packet: {detail}", "تعذّر إرسال حزمة الإيقاظ: {detail}";
    AbAddTitle => "Add a device", "إضافة جهاز";
    AbEditTitle => "Edit device", "تعديل الجهاز";
    AbAlias => "Alias", "الاسم المستعار";
    AbAliasHint => "Your own name for this device", "اسم تختاره لهذا الجهاز";
    AbAnnouncedName => "Announced name", "الاسم المُعلَن";
    AbTags => "Tags", "الوسوم";
    AbTagsHint => "Separated by commas", "افصل بينها بفواصل";
    AbNotes => "Notes", "ملاحظات";
    AbMac => "MAC address", "عنوان MAC";
    AbMacHint => "For Wake-on-LAN, like 00:1a:2b:3c:4d:5e", "للإيقاظ عبر الشبكة، مثل \u{2066}00:1a:2b:3c:4d:5e\u{2069}";
    AbMacLearned => "learned automatically", "عُرف تلقائياً";
    AbLastAddresses => "Last addresses", "آخر العناوين";
    AbLastConnected => "Last connected", "آخر اتصال";
    AbPinnedKey => "Pinned key", "المفتاح المثبَّت";
    AbNever => "Never", "لم يحدث";
    AbErrBadId => "Enter a 9-digit device ID.", "أدخل معرّف جهاز من ٩ أرقام.";
    AbErrBadMac => "This is not a valid MAC address.", "هذا ليس عنوان MAC صالحاً.";
    AbErrDuplicate => "This device is already in the address book.", "هذا الجهاز موجود في دفتر العناوين.";
    AbDeleteTitle => "Delete device", "حذف الجهاز";
    AbDeleteBody => "Remove {name} from the address book? Its pinned key stays trusted.", "هل تريد إزالة {name} من دفتر العناوين؟ سيبقى مفتاحه المثبَّت موثوقاً.";
    AbImport => "Import", "استيراد";
    AbExport => "Export", "تصدير";
    AbImportTitle => "Import an address book", "استيراد دفتر عناوين";
    AbExportTitle => "Export the address book", "تصدير دفتر العناوين";
    AbFilePath => "File path", "مسار الملف";
    AbImportDone => "Import finished: {added} added, {updated} updated.", "اكتمل الاستيراد: أُضيف {added} وحُدّث {updated}.";
    AbExportDone => "The address book was exported to {path}", "صُدّر دفتر العناوين إلى {path}";
    AbFileError => "File error: {detail}", "خطأ في الملف: {detail}";
    AbSaveError => "Could not save the address book: {detail}", "تعذّر حفظ دفتر العناوين: {detail}";
    AbRecovered => "The address book file was damaged. It was kept as {path}.", "كان ملف دفتر العناوين تالفاً، وحُفظت نسخة منه باسم {path}.";
    AbRestoredBackup => "The address book was restored from its backup copy.", "استُعيد دفتر العناوين من نسخته الاحتياطية.";
    AbOfferSave => "Save {name} to the address book?", "هل تريد حفظ {name} في دفتر العناوين؟";
    AbNotNow => "Not now", "ليس الآن";
    AgoJustNow => "just now", "الآن";
    AgoMinute1 => "1 minute ago", "منذ دقيقة";
    AgoMinute2 => "2 minutes ago", "منذ دقيقتين";
    AgoMinutesFew => "{n} minutes ago", "منذ {n} دقائق";
    AgoMinutesMany => "{n} minutes ago", "منذ {n} دقيقة";
    AgoHour1 => "1 hour ago", "منذ ساعة";
    AgoHour2 => "2 hours ago", "منذ ساعتين";
    AgoHoursFew => "{n} hours ago", "منذ {n} ساعات";
    AgoHoursMany => "{n} hours ago", "منذ {n} ساعة";
    AgoDay1 => "1 day ago", "منذ يوم";
    AgoDay2 => "2 days ago", "منذ يومين";
    AgoDaysFew => "{n} days ago", "منذ {n} أيام";
    AgoDaysMany => "{n} days ago", "منذ {n} يوماً";
    AgoDaysOther => "{n} days ago", "منذ {n} يوم";
    // --- device info (TLP-cli-devinfo) ---
    DeviceInfoTitle => "Device info", "معلومات الجهاز";
    DeviceInfoRefresh => "Refresh", "تحديث";
    DeviceInfoLoading => "Asking the other device…", "جارٍ سؤال الجهاز الآخر…";
    DeviceInfoNoAnswer => "The other device did not answer. It may run an older TLP-Desk.", "لم يُجب الجهاز الآخر. ربما يعمل بإصدار أقدم من TLP-Desk.";
    DeviceInfoComputer => "Computer name", "اسم الحاسوب";
    DeviceInfoSystem => "System", "النظام";
    DeviceInfoVersion => "Version", "الإصدار";
    DeviceInfoCpu => "Processor", "المعالج";
    DeviceInfoThreads => "Threads", "مسارات المعالجة";
    DeviceInfoMemory => "Memory", "الذاكرة";
    DeviceInfoUptime => "Running for", "مدة التشغيل";
    DeviceInfoDisks => "Disks", "الأقراص";
    DeviceInfoDiskFree => "{free} free of {total}", "{free} متاح من {total}";
    UptimeDays => "{d} d {h} h {m} min", "{d} ي {h} س {m} د";
    UptimeHours => "{h} h {m} min", "{h} س {m} د";
    UptimeMinutes => "{m} min", "{m} د";
    // Port forwarding (TLP-tunnel)
    PortForwarding => "Port forwarding", "إعادة توجيه المنافذ";
    TunnelHint => "Open a port on this PC that leads to a service on the remote PC, for example its local web server.", "افتح منفذاً على هذا الجهاز يوصل إلى خدمة على الجهاز البعيد، مثل خادم الويب المحلي فيه.";
    TunnelLocalPort => "Port on this PC", "المنفذ على هذا الجهاز";
    TunnelRemoteHost => "Remote address", "العنوان البعيد";
    TunnelRemoteHostHint => "127.0.0.1 = the remote PC itself", "\u{2066}127.0.0.1\u{2069} تعني الجهاز البعيد نفسه";
    TunnelRemotePort => "Remote port", "المنفذ البعيد";
    TunnelAdd => "Start forwarding", "بدء التوجيه";
    TunnelStop => "Stop", "إيقاف";
    TunnelLanBind => "Let other devices on my network use this port", "السماح للأجهزة الأخرى على شبكتي باستخدام هذا المنفذ";
    TunnelLanBindWarning => "Anyone on your network can then reach the remote service through this PC.", "عندها يستطيع أي شخص على شبكتك الوصول إلى الخدمة البعيدة عبر هذا الجهاز.";
    TunnelNone => "No port is forwarded yet.", "لا يوجد منفذ موجَّه بعد.";
    TunnelRuleStats => "{open} open, {total} total, sent {sent}, received {received}", "{open} مفتوح، {total} إجمالاً، أُرسل {sent}، استُقبل {received}";
    TunnelFailedCount => "{n} failed", "{n} فشل";
    TunnelBadPort => "Enter a port between 1 and 65535.", "أدخل منفذاً بين ١ و٦٥٥٣٥.";
    TunnelErrNotAllowed => "The other side has not allowed port forwarding.", "لم يسمح الطرف الآخر بإعادة توجيه المنافذ.";
    TunnelErrRevoked => "The other side stopped port forwarding.", "أوقف الطرف الآخر إعادة توجيه المنافذ.";
    TunnelErrLocalPort => "This port cannot be opened on this PC: {detail}", "تعذّر فتح هذا المنفذ على هذا الجهاز: {detail}";
    TunnelErrTargetNotAllowed => "Only addresses on the remote PC's local network can be reached.", "لا يمكن الوصول إلا إلى عناوين الشبكة المحلية للجهاز البعيد.";
    TunnelErrBadTarget => "Enter the remote address as an IP address, for example 192.168.1.10.", "أدخل العنوان البعيد كعنوان IP، مثل \u{2066}192.168.1.10\u{2069}.";
    TunnelErrTooMany => "Too many forwarded ports.", "عدد المنافذ الموجَّهة كبير جداً.";
    TunnelErrNoAnswer => "The other side did not answer.", "لم يُجب الطرف الآخر.";
    TunnelErrOther => "Port forwarding failed: {detail}", "فشلت إعادة توجيه المنافذ: {detail}";
    HostTunnelsAllow => "Allow port forwarding", "السماح بإعادة توجيه المنافذ";
    HostTunnelsActive => "{n} forwarded connections open", "{n} اتصالات موجَّهة مفتوحة";
    // whiteboard (TLP-whiteboard)
    WbDraw => "Draw on screen", "الرسم على الشاشة";
    WbDrawHint => "While drawing, the mouse draws on the other device's screen instead of controlling it.", "أثناء الرسم يرسم الفأر على شاشة الجهاز الآخر بدلاً من التحكم به.";
    WbNotAllowed => "The other device does not allow drawing on its screen.", "لا يسمح الجهاز الآخر بالرسم على شاشته.";
    WbPen => "Pen", "قلم";
    WbHighlighter => "Highlighter", "قلم تمييز";
    WbArrow => "Arrow", "سهم";
    WbRectangle => "Rectangle", "مستطيل";
    WbEraser => "Eraser", "ممحاة";
    WbUndo => "Undo", "تراجع";
    WbRedo => "Redo", "إعادة";
    WbClearAll => "Clear all", "مسح الكل";
    WbFade => "Fade out", "إخفاء تلقائي";
    WbFadeHint => "Drawings disappear a few seconds after you finish them.", "تختفي الرسوم بعد بضع ثوانٍ من الانتهاء منها.";
    WbThin => "Thin line", "خط رفيع";
    WbMedium => "Medium line", "خط متوسط";
    WbThick => "Thick line", "خط عريض";
    WbRed => "Red", "أحمر";
    WbOrange => "Orange", "برتقالي";
    WbYellow => "Yellow", "أصفر";
    WbGreen => "Green", "أخضر";
    WbBlue => "Blue", "أزرق";
    WbPurple => "Purple", "بنفسجي";
    WbBlack => "Black", "أسود";
    WbWhite => "White", "أبيض";
    WbHostCleared => "The other side cleared the drawings.", "مسح الطرف الآخر الرسوم.";
    // recording (TLP-recording)
    RecStart => "Record this session to a video file on this computer", "تسجيل هذه الجلسة في ملف فيديو على هذا الجهاز";
    RecStop => "Stop recording", "إيقاف التسجيل";
    RecActive => "Recording {time}", "جارٍ التسجيل {time}";
    RecStarting => "Starting the recording…", "جارٍ بدء التسجيل…";
    RecErrDiskFull => "Recording stopped: the disk is almost full.", "توقف التسجيل: القرص ممتلئ تقريباً.";
    RecErrNoFolder => "Recording is not possible: no folder for videos was found.", "التسجيل غير ممكن: لم يُعثر على مجلد للفيديو.";
    RecErrFailed => "Recording failed: {detail}", "فشل التسجيل: {detail}";
    Recordings => "Recordings", "التسجيلات";
    RecordingsHint => "Recordings stay on this computer. The other side is not notified.", "تبقى التسجيلات على هذا الجهاز، ولا يُبلَّغ الطرف الآخر بها.";
    RecordingsEmpty => "No recordings yet. Use the record button during a session.", "لا توجد تسجيلات بعد. استخدم زر التسجيل أثناء الجلسة.";
    RecOpenFolder => "Open folder", "فتح المجلد";
    RecRefresh => "Refresh", "تحديث";
    RecPlay => "Play", "تشغيل";
    RecPause => "Pause", "إيقاف مؤقت";
    RecDelete => "Delete", "حذف";
    RecDeleteTitle => "Delete this recording?", "حذف هذا التسجيل؟";
    RecDeleteBody => "{name} will be deleted permanently.", "سيُحذف {name} نهائياً.";
    RecIncomplete => "Interrupted", "منقطع";
    RecRepair => "Repair", "إصلاح";
    RecRepairHint => "The recording was interrupted. Repairing finishes the file so every player can open it.", "انقطع التسجيل. الإصلاح يُكمل الملف ليفتحه أي مشغّل.";
    RecErrOpen => "Cannot open this recording: {detail}", "تعذّر فتح هذا التسجيل: {detail}";
    RecErrAction => "The operation failed: {detail}", "فشلت العملية: {detail}";
    PlayTitle => "Playback: {name}", "تشغيل: {name}";
    PlaySpeed => "Speed", "السرعة";
    PlayClose => "Close", "إغلاق";
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
