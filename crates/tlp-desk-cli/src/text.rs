//! Every user-facing CLI string, in English and Arabic (`--lang en|ar`, env `TLP_DESK_LANG`).
//!
//! The language is chosen once at start-up ([`set_lang`]); each function returns the text in
//! that language. Numbers, device IDs, addresses and codes stay in ASCII digits in both
//! languages so they can be copied into other commands and scripts. JSON output (`--json`) is
//! never translated: its keys and enum names are a stable machine interface.

use std::cell::Cell;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use tlp_desk_crypto::{DeviceId, Fingerprint};
use tlp_desk_host::{
    CancelReason, ConnectedViewer, EndReason as HostEnd, IncomingRequest, PrivacyScreenText,
    PrivacyStatus, PrivacyViewer, ScreenLine,
};
use tlp_desk_net::{AuthKind, DiscoveredDevice, PeerInfo};
use tlp_desk_proto::{Permissions, RejectReason};
use tlp_desk_viewer::{EndReason as ViewerEnd, MonitorDesc, ViewerStats};
use tlp_desk_viewer::{ForwardSpec, TunnelCloseReason, TunnelId, TunnelStatus, ViewerError};

/// Language of the human-readable output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Lang {
    /// English.
    #[default]
    En,
    /// Arabic (العربية).
    Ar,
}

static LANG: AtomicU8 = AtomicU8::new(0);

thread_local! {
    /// Per-thread override so tests can check both languages in parallel.
    static OVERRIDE: Cell<Option<Lang>> = const { Cell::new(None) };
}

/// Sets the process-wide output language (call once, before any output).
pub fn set_lang(lang: Lang) {
    LANG.store(lang as u8, Ordering::Relaxed);
}

/// The output language in effect on this thread.
pub fn lang() -> Lang {
    OVERRIDE.with(Cell::get).unwrap_or_else(|| {
        if LANG.load(Ordering::Relaxed) == Lang::Ar as u8 {
            Lang::Ar
        } else {
            Lang::En
        }
    })
}

/// Runs `f` with `lang` as this thread's language (tests).
#[cfg(test)]
pub fn with_lang<R>(lang: Lang, f: impl FnOnce() -> R) -> R {
    let prev = OVERRIDE.with(|o| o.replace(Some(lang)));
    let r = f();
    OVERRIDE.with(|o| o.set(prev));
    r
}

/// Picks the English or Arabic variant.
fn t<T>(en: T, ar: T) -> T {
    match lang() {
        Lang::En => en,
        Lang::Ar => ar,
    }
}

/// `--help` summary (clap needs a constant, so this one stays English).
pub const ABOUT: &str = "Headless TLP-Desk client for scripts and AI agents (LAN only)";

pub fn waiting_for_accept() -> &'static str {
    t(
        "Waiting for the host user to accept...",
        "بانتظار موافقة مستخدم الجهاز المضيف...",
    )
}

pub fn prompt_hint() -> &'static str {
    t(
        "Type y + Enter to accept, anything else to reject.",
        "اكتب y ثم Enter للقبول، وأي شيء آخر للرفض.",
    )
}

pub fn stopping() -> &'static str {
    t("Stopping.", "جارٍ الإيقاف.")
}

pub fn no_devices() -> &'static str {
    t("No devices found.", "لم يُعثر على أي جهاز.")
}

pub fn no_frame() -> &'static str {
    t(
        "No frame was decoded; snapshot not written.",
        "لم تُفك أي صورة؛ لم تُحفظ اللقطة.",
    )
}

pub fn error_line(err: &anyhow::Error) -> String {
    if let Some(fs) = err.downcast_ref::<tlp_desk_files::fs::FsClientError>() {
        return t(
            format!("error: {}", fs_error(fs)),
            format!("خطأ: {}", fs_error(fs)),
        );
    }
    use crate::exit::CliError;
    let cli = err.chain().find_map(|c| c.downcast_ref::<CliError>());
    let detail = match cli {
        Some(CliError::Interrupted) => t("interrupted".into(), "أُوقف بـ Ctrl+C".into()),
        Some(CliError::Timeout(s)) => {
            t(format!("no answer within {s} s"), format!("لا رد خلال {s} ث"))
        }
        Some(CliError::PermissionDenied(p)) => t(
            format!("the session lacks the {p} permission"),
            format!("الجلسة لا تملك صلاحية {p}"),
        ),
        Some(CliError::SessionEnded(r)) => t(
            format!("the session ended: {r}"),
            format!("انتهت الجلسة: {r}"),
        ),
        Some(CliError::NotFound(n)) => t(format!("{n} was not found"), format!("لم يُعثر على {n}")),
        None => format!("{err:#}"),
    };
    t(format!("error: {detail}"), format!("خطأ: {detail}"))
}

pub fn identity(id: DeviceId, fp: &Fingerprint, dir: &str) -> String {
    t(
        format!(
            "Device ID:   {id}\nFingerprint: {}\nKey store:   {dir}",
            fp.to_hex()
        ),
        format!(
            "معرّف الجهاز: {id}\nبصمة المفتاح: {}\nمخزن المفاتيح: {dir}",
            fp.to_hex()
        ),
    )
}

pub fn listening(id: DeviceId, addr: std::net::SocketAddr, name: &str) -> String {
    t(
        format!("Hosting as \"{name}\" - device ID {id}, listening on UDP {addr}"),
        format!("الاستضافة باسم \"{name}\" - معرّف الجهاز {id}، الاستماع على UDP {addr}"),
    )
}

pub fn code(code: &str) -> String {
    t(
        format!("One-time code: {code}"),
        format!("رمز الاستخدام لمرة واحدة: {code}"),
    )
}

pub fn code_disabled() -> &'static str {
    t(
        "One-time code access is off (use --code to enable). Viewers must be accepted.",
        "الدخول برمز لمرة واحدة معطّل (فعّله بـ --code). يجب قبول كل مشاهد يدويًا.",
    )
}

pub fn auto_accept() -> &'static str {
    t(
        "WARNING: --auto-accept is on: every LAN viewer that asks is accepted without a prompt.",
        "تحذير: الخيار --auto-accept مفعّل: يُقبل كل مشاهد على الشبكة المحلية دون سؤال.",
    )
}

pub fn incoming(r: &IncomingRequest) -> String {
    t(
        format!(
            "\nIncoming connection: \"{}\" (ID {}) from {}",
            r.peer.name, r.peer.device_id, r.peer.addr
        ),
        format!(
            "\nطلب اتصال وارد: \"{}\" (المعرّف {}) من {}",
            r.peer.name, r.peer.device_id, r.peer.addr
        ),
    )
}

pub fn accept_question() -> &'static str {
    t("Accept? [y/N] ", "قبول؟ [y/N] ")
}

pub fn auto_accepted(r: &IncomingRequest) -> String {
    t(
        format!("Auto-accepting {} ({})", r.peer.device_id, r.peer.addr),
        format!("قبول تلقائي لـ {} ({})", r.peer.device_id, r.peer.addr),
    )
}

pub fn cancelled(reason: CancelReason) -> &'static str {
    match reason {
        CancelReason::ViewerLeft => t(
            "The viewer gave up before an answer.",
            "انسحب المشاهد قبل الرد.",
        ),
        CancelReason::TimedOut => t("No answer in time: rejected.", "لم يصل رد في الوقت: رُفض."),
        CancelReason::HostShutdown => t(
            "Host stopping: request rejected.",
            "المضيف يتوقف: رُفض الطلب.",
        ),
    }
}

pub fn auth_failed(peer: &PeerInfo, kind: AuthKind, reason: RejectReason) -> String {
    t(
        format!(
            "Refused {} ({}) using {kind:?}: {reason:?}",
            peer.device_id, peer.addr
        ),
        format!(
            "رُفض {} ({}) عبر {kind:?}: {reason:?}",
            peer.device_id, peer.addr
        ),
    )
}

pub fn connected_viewer(v: &ConnectedViewer) -> String {
    t(
        format!(
            "CONNECTED: \"{}\" (ID {}) from {} via {:?}, permissions {}",
            v.peer.name,
            v.peer.device_id,
            v.peer.addr,
            v.auth,
            permissions(v.permissions)
        ),
        format!(
            "متصل: \"{}\" (المعرّف {}) من {} عبر {:?}، الصلاحيات {}",
            v.peer.name,
            v.peer.device_id,
            v.peer.addr,
            v.auth,
            permissions(v.permissions)
        ),
    )
}

pub fn viewer_left(peer: &PeerInfo, reason: &HostEnd) -> String {
    t(
        format!(
            "DISCONNECTED: {} ({}) - {reason:?}",
            peer.device_id, peer.addr
        ),
        format!("انقطع: {} ({}) - {reason:?}", peer.device_id, peer.addr),
    )
}

pub fn input_blocked() -> String {
    t(
        "INPUT BLOCKED: an administrator window or secure screen is in front",
        "الإدخال محجوب: نافذة مسؤول أو شاشة آمنة في المقدمة",
    )
    .into()
}

pub fn code_changed(code: &str) -> String {
    t(
        format!("New one-time code: {code}"),
        format!("رمز جديد لمرة واحدة: {code}"),
    )
}

/// Permission names joined with `+`. Kept in English in both languages: they are the same
/// words the JSON output and `--help` use.
pub fn permissions(p: Permissions) -> String {
    let v = permission_names(p);
    if v.is_empty() {
        t("none", "لا شيء").into()
    } else {
        v.join("+")
    }
}

/// Stable machine names of the permissions in `p` (also used in JSON).
pub fn permission_names(p: Permissions) -> Vec<&'static str> {
    let names = [
        (Permissions::VIEW, "view"),
        (Permissions::CONTROL, "mouse"),
        (Permissions::KEYBOARD, "keyboard"),
        (Permissions::CLIPBOARD, "clipboard"),
        (Permissions::FILES, "files"),
        (Permissions::AUDIO, "audio"),
        (Permissions::RESTART, "restart"),
        (Permissions::PRIVACY_MODE, "privacy"),
        (Permissions::CHAT, "chat"),
        (Permissions::TUNNEL, "tunnel"),
        (Permissions::WHITEBOARD, "whiteboard"),
        (Permissions::SYSTEM, "system"),
    ];
    names
        .iter()
        .filter(|(f, _)| p.contains(*f))
        .map(|(_, n)| *n)
        .collect()
}

pub fn device_row(d: &DiscoveredDevice) -> String {
    let addrs: Vec<String> = d.addrs.iter().map(|a| a.to_string()).collect();
    format!(
        "{}  {:<24}  {}  (v{}, {})",
        d.device_id,
        d.name,
        addrs.join(", "),
        d.protocol_version,
        discovered_via(d)
    )
}

/// How a device was discovered (stable machine name, also used in JSON).
pub fn discovered_via(d: &DiscoveredDevice) -> &'static str {
    match (d.via_mdns, d.via_broadcast) {
        (true, true) => "mdns+broadcast",
        (true, false) => "mdns",
        _ => "broadcast",
    }
}

pub fn searching(secs: u64) -> String {
    t(
        format!("Searching the LAN for {secs} s..."),
        format!("جارٍ البحث في الشبكة المحلية لمدة {secs} ث..."),
    )
}

pub fn connecting(target: &str) -> String {
    t(
        format!("Connecting to {target}..."),
        format!("جارٍ الاتصال بـ {target}..."),
    )
}

pub fn connected_host(peer: &PeerInfo, perms: Permissions) -> String {
    t(
        format!(
            "Connected to \"{}\" (ID {}) at {}, permissions {}",
            peer.name,
            peer.device_id,
            peer.addr,
            permissions(perms)
        ),
        format!(
            "تم الاتصال بـ \"{}\" (المعرّف {}) على {}، الصلاحيات {}",
            peer.name,
            peer.device_id,
            peer.addr,
            permissions(perms)
        ),
    )
}

pub fn stats_line(t_elapsed: Duration, s: &ViewerStats) -> String {
    // The quantiser is unknown (0) for encoders that do not signal it per slice.
    let qp = if s.qp > 0.0 {
        format!("{:.0}", s.qp)
    } else {
        "-".to_owned()
    };
    t(
        format!(
            "[{:>3}s] {:>5.1} fps  {:>8.0} kbps  rtt {:>5.1} ms  latency ~{:>5.1} ms  decode {:>5.1} ms  {}x{}  Q~{qp}  frames {}/{}",
            t_elapsed.as_secs(), s.fps, s.bitrate_kbps, s.rtt_ms, s.latency_ms, s.decode_ms,
            s.width, s.height, s.frames_decoded, s.frames_received
        ),
        format!(
            "[{:>3} ث] {:>5.1} إطار/ث  {:>8.0} ك.بت/ث  ذهاب وإياب {:>5.1} م.ث  تأخير ~{:>5.1} م.ث  فك {:>5.1} م.ث  {}x{}  Q~{qp}  إطارات {}/{}",
            t_elapsed.as_secs(), s.fps, s.bitrate_kbps, s.rtt_ms, s.latency_ms, s.decode_ms,
            s.width, s.height, s.frames_decoded, s.frames_received
        ),
    )
}

/// The dev-only link simulation of `connect` started (`Some`) or ended (`None`).
pub fn net_sim(sim: Option<tlp_desk_viewer::NetSim>) -> String {
    match sim {
        Some(s) => t(
            format!(
                "DEV: simulating {}% video loss, {} kbit/s link",
                s.loss_pct, s.bandwidth_kbps
            ),
            format!(
                "للمطورين: محاكاة فقدان {}% من الفيديو ووصلة {} ك.بت/ث",
                s.loss_pct, s.bandwidth_kbps
            ),
        ),
        None => t(
            "DEV: link simulation ended".to_owned(),
            "للمطورين: انتهت محاكاة الوصلة".to_owned(),
        ),
    }
}

pub fn session_ended(r: &ViewerEnd) -> String {
    t(
        format!("Session ended: {r:?}"),
        format!("انتهت الجلسة: {r:?}"),
    )
}

pub fn snapshot_saved(path: &str, w: u32, h: u32) -> String {
    t(
        format!("Snapshot {w}x{h} saved to {path}"),
        format!("حُفظت لقطة {w}x{h} في {path}"),
    )
}

pub fn key_changed_help() -> &'static str {
    t(
        "\n!!! THE HOST KEY HAS CHANGED !!!\n\
         Someone may be impersonating this host (man-in-the-middle), or it was reinstalled.\n\
         If you are sure the host was reinstalled, run again with --forget-key.",
        "\n!!! تغيّر مفتاح الجهاز المضيف !!!\n\
         قد يكون أحدهم ينتحل هذا الجهاز (هجوم الوسيط)، أو أُعيد تثبيته.\n\
         إن كنت متأكدًا من إعادة التثبيت، فأعد التشغيل مع --forget-key.",
    )
}

pub fn summary(s: &ViewerStats, secs: f32) -> String {
    let avg = s.frames_decoded as f32 / secs.max(0.001);
    t(
        format!(
            "Summary: {} frames decoded in {secs:.1} s ({avg:.1} fps avg)",
            s.frames_decoded
        ),
        format!(
            "الملخص: فُكّ {} إطارًا في {secs:.1} ث (بمعدل {avg:.1} إطار/ث)",
            s.frames_decoded
        ),
    )
}

pub fn host_priority(local_active: bool) -> &'static str {
    if local_active {
        t(
            "The host user is using the mouse/keyboard; your input is paused.",
            "مستخدم المضيف يستعمل الفأرة/لوحة المفاتيح؛ إدخالك متوقف مؤقتًا.",
        )
    } else {
        t(
            "The host user is idle again; your input is active.",
            "مستخدم المضيف لم يعد نشطًا؛ إدخالك مفعّل.",
        )
    }
}

pub fn monitors(list: &[MonitorDesc], active: u32) -> String {
    let mut out = String::from(t("Host monitors:", "شاشات المضيف:"));
    for m in list {
        out.push_str(&format!(
            "\n  {} {}: {}x{} ({}, {}){}{}",
            if m.index == active { "*" } else { " " },
            m.index,
            m.width,
            m.height,
            m.x,
            m.y,
            if m.primary {
                t(" primary", " رئيسية")
            } else {
                ""
            },
            if m.name.is_empty() {
                String::new()
            } else {
                format!(" \"{}\"", m.name)
            }
        ));
    }
    out
}

pub fn selecting_monitor(index: u32) -> String {
    t(
        format!("Switching to monitor {index}..."),
        format!("جارٍ التبديل إلى الشاشة {index}..."),
    )
}

pub fn privacy_allowed() -> &'static str {
    t(
        "Privacy mode is allowed: viewers may blank this PC's screen and block its keyboard/mouse.\n\
         To end it here: hold Ctrl+Alt+Shift+P for 3 seconds, or press Ctrl+Alt+Del.",
        "وضع الخصوصية مسموح: يمكن للمشاهدين جعل شاشة هذا الحاسوب سوداء وحجب لوحة مفاتيحه وفأرته.\n\
         لإنهائه من هنا: اضغط مطولاً على Ctrl+Alt+Shift+P لمدة ٣ ثوانٍ، أو اضغط Ctrl+Alt+Del.",
    )
}

/// The host's black privacy screen. Shown to whoever sits at the host, so it is always in
/// Arabic and English, whatever `--lang` says (same wording as the GUI's i18n table).
pub fn privacy_screen(viewers: &[PrivacyViewer]) -> PrivacyScreenText {
    let mut lines = vec![
        ScreenLine::title("هذا الحاسوب قيد الاستخدام عن بُعد عبر TLP-Desk"),
        ScreenLine::title("This PC is being used remotely with TLP-Desk"),
    ];
    for v in viewers {
        lines.push(ScreenLine::body(format!(
            "متصل: {} ({})",
            v.name, v.device_id
        )));
        lines.push(ScreenLine::body(format!(
            "Connected: {} ({})",
            v.name, v.device_id
        )));
    }
    lines.push(ScreenLine::body(
        "لإنهاء وضع الخصوصية من هنا، اضغط مطولاً على Ctrl+Alt+Shift+P لمدة ٣ ثوانٍ أو اضغط Ctrl+Alt+Del.",
    ));
    lines.push(ScreenLine::body(
        "To end privacy mode here, hold Ctrl+Alt+Shift+P for 3 seconds or press Ctrl+Alt+Del.",
    ));
    PrivacyScreenText { lines }
}

/// Host side: privacy mode changed for a viewer.
pub fn host_privacy(peer: &str, status: &PrivacyStatus) -> String {
    let cause = format!("{:?}", status.cause);
    if status.active {
        t(
            format!("PRIVACY MODE ON ({cause}; viewer {peer})"),
            format!("وضع الخصوصية مفعّل ({cause}؛ المشاهد {peer})"),
        )
    } else {
        t(
            format!("PRIVACY MODE OFF ({cause}; viewer {peer})"),
            format!("وضع الخصوصية متوقف ({cause}؛ المشاهد {peer})"),
        )
    }
}

pub fn requesting_privacy() -> &'static str {
    t(
        "Asking the host to turn privacy mode on...",
        "جارٍ طلب تشغيل وضع الخصوصية من المضيف...",
    )
}

/// Viewer side: the host's actual privacy-mode state.
pub fn viewer_privacy(enabled: bool) -> &'static str {
    if enabled {
        t(
            "Privacy mode is ON: the host screen is black and its keyboard/mouse are blocked.",
            "وضع الخصوصية مفعّل: شاشة المضيف سوداء ولوحة مفاتيحه وفأرته محجوبة.",
        )
    } else {
        t(
            "Privacy mode is OFF (turned off, refused, or ended on the host).",
            "وضع الخصوصية متوقف (أُوقف، أو رُفض، أو أُنهي على المضيف).",
        )
    }
}

pub fn wol_sent(mac: &str, to: &str) -> String {
    t(
        format!("Wake-on-LAN packet for {mac} sent ({to})."),
        format!("أُرسلت حزمة التشغيل عن بُعد إلى {mac} ({to})."),
    )
}

// --- permanent password ---

pub fn password_removed() -> String {
    t(
        "Permanent password removed: password sign-in is off.".to_owned(),
        "أُزيلت كلمة المرور الدائمة: تسجيل الدخول بكلمة المرور متوقف.".to_owned(),
    )
}

pub fn password_set(path: &str) -> String {
    t(
        format!("Permanent password set (only its Argon2id verifier is stored, in {path})."),
        format!("عُيّنت كلمة المرور الدائمة (يُخزَّن فقط ناتج Argon2id الخاص بها، في {path})."),
    )
}

pub fn password_too_short() -> String {
    let n = tlp_desk_crypto::MIN_PASSWORD_CHARS;
    t(
        format!("The password is too short: use at least {n} characters."),
        format!("كلمة المرور قصيرة جدًا: استخدم {n} أحرف على الأقل."),
    )
}

// --- device info ---

/// `bytes` in binary units with one decimal (`16.0 GiB` / `16.0 غ.ب`).
pub fn bytes(bytes: u64) -> String {
    let units = t(
        ["B", "KiB", "MiB", "GiB", "TiB", "PiB"],
        ["بايت", "ك.ب", "م.ب", "غ.ب", "ت.ب", "ب.ب"],
    );
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < units.len() {
        value /= 1024.0;
        unit += 1;
    }
    let name = units.get(unit).copied().unwrap_or_default();
    if unit == 0 {
        format!("{bytes} {name}")
    } else {
        format!("{value:.1} {name}")
    }
}

/// An uptime like `3 d 4 h 5 min` / `3 ي 4 س 5 د`.
pub fn uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    let (ud, uh, um) = t(("d", "h", "min"), ("ي", "س", "د"));
    if d > 0 {
        format!("{d} {ud} {h} {uh} {m} {um}")
    } else if h > 0 {
        format!("{h} {uh} {m} {um}")
    } else {
        format!("{m} {um}")
    }
}

/// The device-info block printed by `info`.
pub fn device_info(info: &tlp_desk_viewer::DeviceInfo) -> String {
    let (host, os, cpu, ram, up, disks, free_of) = t(
        (
            "Computer",
            "System",
            "Processor",
            "Memory",
            "Up for",
            "Disks",
            "free of",
        ),
        (
            "الحاسوب",
            "النظام",
            "المعالج",
            "الذاكرة",
            "مدة التشغيل",
            "الأقراص",
            "متاح من",
        ),
    );
    let threads = t("threads", "مسارات");
    let mut out = format!(
        "{host}: {}\n{os}: {} ({})\n{cpu}: {} ({} {threads})\n{ram}: {}\n{up}: {}\n{disks}:",
        info.hostname,
        info.os,
        info.version,
        info.cpu,
        info.cores,
        bytes(info.ram_bytes),
        uptime(info.uptime_s)
    );
    for d in &info.disks {
        let label = if d.label.is_empty() {
            String::new()
        } else {
            format!(" \"{}\"", d.label)
        };
        out.push_str(&format!(
            "\n  {}{label}  {} {free_of} {}",
            d.mount,
            bytes(d.free_bytes),
            bytes(d.total_bytes)
        ));
    }
    out
}

pub fn device_info_served(session: u64) -> String {
    t(
        format!("Sent device info to viewer session {session}"),
        format!("أُرسلت معلومات الجهاز إلى جلسة المشاهد {session}"),
    )
}

pub fn requesting_device_info() -> &'static str {
    t(
        "Asking the host for its device info...",
        "جارٍ طلب معلومات الجهاز من المضيف...",
    )
}

// --- tunnels: port forwarding (TLP-tunnel) ---

pub fn tunnels_allowed() -> &'static str {
    t(
        "Port forwarding is ALLOWED for viewers (--allow-tunnels): they can reach services on \
         this PC and its local network.",
        "إعادة توجيه المنافذ مسموحة للمشاهدين (--allow-tunnels): يمكنهم الوصول إلى الخدمات على \
         هذا الجهاز وشبكته المحلية.",
    )
}

pub fn tunnel_waiting_permission() -> &'static str {
    t(
        "The host has not allowed port forwarding yet. Waiting until the host user grants it \
         (Ctrl+C to stop)...",
        "لم يسمح المضيف بإعادة توجيه المنافذ بعد. بانتظار أن يمنحها مستخدم المضيف \
         (Ctrl+C للإيقاف)...",
    )
}

pub fn forward_running() -> &'static str {
    t(
        "Forwarding. Press Ctrl+C to stop.",
        "إعادة التوجيه تعمل. اضغط Ctrl+C للإيقاف.",
    )
}

pub fn lan_bind_warning(bind: std::net::SocketAddr) -> String {
    t(
        format!(
            "WARNING: listening on {bind}: other devices on your network can use this tunnel too."
        ),
        format!("تحذير: الاستماع على {bind}: يمكن للأجهزة الأخرى على شبكتك استخدام هذا النفق أيضًا."),
    )
}

pub fn lan_bind_needs_flag(bind: std::net::SocketAddr) -> String {
    t(
        format!(
            "{bind} is not a loopback address: other devices could use the tunnel. Add \
             --bind-lan to confirm."
        ),
        format!(
            "{bind} ليس عنوانًا محليًا (loopback): قد تستخدم أجهزة أخرى النفق. أضف --bind-lan \
             للتأكيد."
        ),
    )
}

pub fn forward_started(s: &TunnelStatus) -> String {
    t(
        format!(
            "Forwarding {} -> {} on the host (rule {})",
            s.local,
            s.target_display(),
            s.id
        ),
        format!(
            "إعادة توجيه {} -> {} على المضيف (القاعدة {})",
            s.local,
            s.target_display(),
            s.id
        ),
    )
}

pub fn forward_failed(spec: &ForwardSpec, e: &ViewerError) -> String {
    t(
        format!("Cannot forward {spec}: {e}"),
        format!("تعذّرت إعادة توجيه {spec}: {e}"),
    )
}

pub fn forward_closed(id: TunnelId, reason: &TunnelCloseReason) -> String {
    t(
        format!("Rule {id} closed by the host: {reason}"),
        format!("أغلق المضيف القاعدة {id}: {reason}"),
    )
}

pub fn forward_stats(list: &[TunnelStatus]) -> String {
    let mut out = String::new();
    for s in list {
        if !out.is_empty() {
            out.push('\n');
        }
        let (from, to) = (s.local, s.target_display());
        out.push_str(&t(
            format!(
                "  {from} -> {to}: {} open, {} total, {} failed, sent {} B, received {} B",
                s.active, s.connections, s.failed, s.bytes_sent, s.bytes_received
            ),
            format!(
                "  {from} -> {to}: {} مفتوح، {} إجمالًا، {} فاشل، أُرسل {} بايت، استُقبل {} بايت",
                s.active, s.connections, s.failed, s.bytes_sent, s.bytes_received
            ),
        ));
    }
    out
}

pub fn host_tunnels(list: &[tlp_desk_host::HostTunnelInfo]) -> String {
    if list.is_empty() {
        return t(
            "Port forwarding: no tunnel open.",
            "إعادة توجيه المنافذ: لا يوجد نفق مفتوح.",
        )
        .into();
    }
    let mut out = String::from(t("Port forwarding:", "إعادة توجيه المنافذ:"));
    for tn in list {
        out.push_str(&t(
            format!(
                "\n  tunnel {} -> {}: {} open, {} total, {} failed",
                tn.id, tn.target, tn.active, tn.connections, tn.failed
            ),
            format!(
                "\n  النفق {} -> {}: {} مفتوح، {} إجمالًا، {} فاشل",
                tn.id, tn.target, tn.active, tn.connections, tn.failed
            ),
        ));
    }
    out
}

pub fn host_tunnel_refused(target_host: Option<&str>, port: u16, reason: &str) -> String {
    let host = target_host.unwrap_or("localhost");
    t(
        format!("Refused port forwarding to {host:?}:{port} ({reason})"),
        format!("رُفضت إعادة التوجيه إلى {host:?}:{port} ({reason})"),
    )
}

// --- chat (TLP-chat) ---

/// A chat message from the other side (`from` is the peer's name).
pub fn chat_received(from: &str, text: &str) -> String {
    t(
        format!("[chat] {from}: {text}"),
        format!("[دردشة] {from}: {text}"),
    )
}

pub fn chat_sent() -> &'static str {
    t("Message sent.", "أُرسلت الرسالة.")
}

pub fn chat_not_allowed() -> &'static str {
    t(
        "The host does not allow chat in this session.",
        "لا يسمح الجهاز المضيف بالدردشة في هذه الجلسة.",
    )
}

/// Why the message was not sent (`detail` is the technical reason).
pub fn chat_rejected(detail: &str) -> String {
    t(
        format!("Message not sent: {detail}"),
        format!("لم تُرسل الرسالة: {detail}"),
    )
}

// --- clipboard (TLP-clipboard) ---

pub fn clip_sent(bytes: u64) -> String {
    t(
        format!("Clipboard sent ({bytes} bytes)."),
        format!("أُرسلت الحافظة ({bytes} بايت)."),
    )
}

pub fn clip_received(bytes: u64) -> String {
    t(
        format!("Clipboard received ({bytes} bytes)."),
        format!("استُلمت الحافظة ({bytes} بايت)."),
    )
}

pub fn clip_not_synced() -> &'static str {
    t(
        "The clipboard could not be synced.",
        "تعذّرت مزامنة الحافظة.",
    )
}

pub fn clip_too_large() -> &'static str {
    t(
        "The clipboard content is too large to sync.",
        "محتوى الحافظة أكبر من أن تتم مزامنته.",
    )
}

pub fn clip_not_allowed() -> &'static str {
    t(
        "The host does not allow clipboard sync for this session.",
        "لا يسمح المضيف بمزامنة الحافظة في هذه الجلسة.",
    )
}

pub fn clip_ended_early() -> &'static str {
    t(
        "The session ended before the clipboard was sent.",
        "انتهت الجلسة قبل إرسال الحافظة.",
    )
}

pub fn clip_timeout() -> &'static str {
    t(
        "Timed out waiting for the clipboard to be sent.",
        "انتهت المهلة قبل إرسال الحافظة.",
    )
}

pub fn clip_nothing() -> &'static str {
    t(
        "Nothing to send: give the text or --file.",
        "لا شيء للإرسال: اكتب النص أو استخدم --file.",
    )
}

// --- files: remote file system (TLP-files-fs) ---

/// One line of `ls`: `<DIR>` or the size in bytes, then the name.
pub fn fs_entry_row(e: &tlp_desk_proto::FsEntry) -> String {
    let kind = if e.is_dir {
        t("<DIR>".to_owned(), "<مجلد>".to_owned())
    } else {
        e.size.to_string()
    };
    let flags = match (e.hidden, e.readonly) {
        (true, true) => t(" (hidden, read-only)", " (مخفي، للقراءة فقط)"),
        (true, false) => t(" (hidden)", " (مخفي)"),
        (false, true) => t(" (read-only)", " (للقراءة فقط)"),
        (false, false) => "",
    };
    format!("{kind:>14}  {}{flags}", e.name)
}

/// One line of `ls` without a path: a drive or known folder.
pub fn fs_root_row(r: &tlp_desk_proto::FsRoot) -> String {
    use tlp_desk_proto::FsRootKind as K;
    let kind = match r.kind {
        K::Drive => t("drive", "قرص"),
        K::Home => t("home", "المجلد الشخصي"),
        K::Desktop => t("desktop", "سطح المكتب"),
        K::Documents => t("documents", "المستندات"),
        K::Downloads => t("downloads", "التنزيلات"),
        K::Pictures => t("pictures", "الصور"),
        K::Music => t("music", "الموسيقى"),
        K::Videos => t("videos", "الفيديو"),
        K::Other => t("shared folder", "مجلد مشترك"),
    };
    let space = match (r.free_bytes, r.total_bytes) {
        (Some(f), Some(tot)) => t(
            format!("  {} of {} GB free", f / 1_000_000_000, tot / 1_000_000_000),
            format!(
                "  {} من {} غيغابايت متاحة",
                f / 1_000_000_000,
                tot / 1_000_000_000
            ),
        ),
        _ => String::new(),
    };
    let label = if r.label.is_empty() {
        String::new()
    } else {
        format!(" \"{}\"", r.label)
    };
    format!("{}  [{kind}]{label}{space}", r.path)
}

pub fn fs_empty_folder() -> &'static str {
    t("The folder is empty.", "المجلد فارغ.")
}

pub fn fs_created(path: &str) -> String {
    t(format!("Created {path}"), format!("أُنشئ {path}"))
}

pub fn fs_deleted(path: &str) -> String {
    t(format!("Deleted {path}"), format!("حُذف {path}"))
}

pub fn fs_moved(from: &str, to: &str) -> String {
    t(
        format!("Moved {from} -> {to}"),
        format!("نُقل {from} <- {to}"),
    )
}

pub fn fs_error(e: &tlp_desk_files::fs::FsClientError) -> String {
    use tlp_desk_files::fs::FsClientError as C;
    use tlp_desk_proto::FsError as F;
    match e {
        C::Remote(F::NotFound) => t("Not found on the remote PC.", "غير موجود على الجهاز البعيد.").into(),
        C::Remote(F::PermissionDenied) => t(
            "The remote PC denied access to this path.",
            "رفض الجهاز البعيد الوصول إلى هذا المسار.",
        )
        .into(),
        C::Remote(F::AlreadyExists) => t("It already exists.", "موجود بالفعل.").into(),
        C::Remote(F::NotEmpty) => t(
            "The folder is not empty (use --recursive).",
            "المجلد ليس فارغاً (استخدم --recursive).",
        )
        .into(),
        C::Remote(F::InvalidPath) => t(
            "Invalid path: use an absolute path such as C:\\Users.",
            "مسار غير صالح: استخدم مساراً كاملاً مثل C:\\Users.",
        )
        .into(),
        C::Remote(F::NoSpace) => t("The remote disk is full.", "القرص البعيد ممتلئ.").into(),
        C::Remote(F::Busy) => t(
            "The file is in use on the remote PC.",
            "الملف قيد الاستخدام على الجهاز البعيد.",
        )
        .into(),
        C::Remote(other) => t(
            format!("The remote PC reported an error: {other:?}"),
            format!("أبلغ الجهاز البعيد عن خطأ: {other:?}"),
        ),
        C::NoPermission => t(
            "The host does not allow file access in this session (it must grant \"files\").",
            "لا يسمح الجهاز المضيف بالوصول إلى الملفات في هذه الجلسة (يجب أن يمنح صلاحية \"الملفات\").",
        )
        .into(),
        C::Timeout => t("The remote PC did not answer in time.", "لم يرد الجهاز البعيد في الوقت المحدد.").into(),
        C::Closed => t("The session ended.", "انتهت الجلسة.").into(),
        C::Send(d) => t(format!("Cannot send the request: {d}"), format!("تعذّر إرسال الطلب: {d}")),
        C::UnexpectedReply => t(
            "The remote PC sent an unexpected answer.",
            "أرسل الجهاز البعيد رداً غير متوقع.",
        )
        .into(),
    }
}

// --- files-transfer lane: send-file / get-file ---

/// `1:05` / `1:02:03` for a duration in seconds.
fn clock(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub fn transfer_started(label: &str, files: u32, total: u64, upload: bool) -> String {
    let size = bytes(total);
    if upload {
        t(
            format!("Sending \"{label}\": {files} file(s), {size}..."),
            format!("جارٍ إرسال \"{label}\": {files} ملف، {size}..."),
        )
    } else {
        t(
            format!("Receiving \"{label}\": {files} file(s), {size}..."),
            format!("جارٍ استلام \"{label}\": {files} ملف، {size}..."),
        )
    }
}

pub fn transfer_progress(done: u64, total: u64, rate: u64, files_done: u32, files: u32) -> String {
    let pct = done.saturating_mul(100).checked_div(total).unwrap_or(100);
    let eta = total
        .saturating_sub(done)
        .checked_div(rate)
        .map_or_else(|| "-".to_string(), clock);
    let (d, tt, r) = (bytes(done), bytes(total), bytes(rate));
    t(
        format!("{pct:>3}%  {d} / {tt}  {r}/s  ETA {eta}  files {files_done}/{files}"),
        format!("{pct:>3}%  {d} / {tt}  {r}/ث  المتبقي {eta}  الملفات {files_done}/{files}"),
    )
}

pub fn transfer_item_failed(rel_path: &str, error: &str) -> String {
    t(
        format!("failed: {rel_path} ({error})"),
        format!("فشل: {rel_path} ({error})"),
    )
}

pub fn transfer_done(files: u32, total: u64, secs: f64, failed: u32, dest: &str) -> String {
    let rate = bytes((total as f64 / secs.max(0.001)) as u64);
    let size = bytes(total);
    t(
        format!(
            "Done: {files} file(s), {size} in {secs:.1} s ({rate}/s), {failed} failed. Destination: {dest}"
        ),
        format!(
            "اكتمل: {files} ملف، {size} خلال {secs:.1} ث ({rate}/ث)، فشل {failed}. الوجهة: {dest}"
        ),
    )
}

pub fn transfer_stopped(state: &str) -> String {
    t(
        format!("Transfer stopped: {state}"),
        format!("توقف النقل: {state}"),
    )
}

pub fn transfer_items_failed(failed: u32) -> String {
    t(
        format!("{failed} file(s) could not be transferred"),
        format!("تعذّر نقل {failed} ملف"),
    )
}

pub fn serve_transfer(label: &str, upload: bool) -> String {
    if upload {
        t(
            format!("Receiving files from the viewer: \"{label}\""),
            format!("استلام ملفات من العارض: \"{label}\""),
        )
    } else {
        t(
            format!("Sending files to the viewer: \"{label}\""),
            format!("إرسال ملفات إلى العارض: \"{label}\""),
        )
    }
}

// --- end files-transfer lane ---

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_languages_differ_and_keep_values() {
        let en = with_lang(Lang::En, || searching(7));
        let ar = with_lang(Lang::Ar, || searching(7));
        assert_eq!(en, "Searching the LAN for 7 s...");
        assert!(ar.starts_with("جارٍ البحث"));
        assert!(ar.contains('7'), "numbers stay ASCII: {ar}");
        assert_ne!(
            with_lang(Lang::En, no_devices),
            with_lang(Lang::Ar, no_devices)
        );
    }

    #[test]
    fn arabic_names_round_trip_in_messages() {
        let name = "حاسوب المكتب ٢";
        let ar = with_lang(Lang::Ar, || connecting(name));
        assert!(ar.contains(name));
        let en = with_lang(Lang::En, || connecting(name));
        assert_eq!(en, format!("Connecting to {name}..."));
    }

    #[test]
    fn permission_names_are_stable() {
        assert_eq!(
            permission_names(Permissions::VIEW | Permissions::FILES),
            ["view", "files"]
        );
        assert_eq!(
            with_lang(Lang::En, || permissions(Permissions::NONE)),
            "none"
        );
        assert_eq!(
            with_lang(Lang::Ar, || permissions(Permissions::NONE)),
            "لا شيء"
        );
    }

    #[test]
    fn byte_and_uptime_formatting() {
        assert_eq!(with_lang(Lang::En, || bytes(512)), "512 B");
        assert_eq!(with_lang(Lang::En, || bytes(16 << 30)), "16.0 GiB");
        assert_eq!(with_lang(Lang::Ar, || bytes(1536)), "1.5 ك.ب");
        assert_eq!(with_lang(Lang::En, || bytes(u64::MAX)), "16384.0 PiB");
        assert_eq!(with_lang(Lang::En, || uptime(90_061)), "1 d 1 h 1 min");
        assert_eq!(with_lang(Lang::Ar, || uptime(3_660)), "1 س 1 د");
        assert_eq!(with_lang(Lang::En, || uptime(59)), "0 min");
    }

    #[test]
    fn device_info_block_in_both_languages() {
        let info = tlp_desk_viewer::DeviceInfo {
            os: "Windows 11 Pro".into(),
            version: "10.0.26100".into(),
            cpu: "Test CPU".into(),
            cores: 8,
            ram_bytes: 16 << 30,
            disks: vec![tlp_desk_viewer::DiskInfo {
                mount: "C:\\".into(),
                label: "النظام".into(),
                total_bytes: 512 << 30,
                free_bytes: 128 << 30,
            }],
            uptime_s: 3600,
            hostname: "حاسوب-المكتب".into(),
        };
        let en = with_lang(Lang::En, || device_info(&info));
        assert!(en.starts_with("Computer: حاسوب-المكتب\n"), "{en}");
        assert!(en.contains("Memory: 16.0 GiB"), "{en}");
        assert!(
            en.contains("C:\\ \"النظام\"  128.0 GiB free of 512.0 GiB"),
            "{en}"
        );
        let ar = with_lang(Lang::Ar, || device_info(&info));
        assert!(ar.starts_with("الحاسوب: حاسوب-المكتب\n"), "{ar}");
        assert!(ar.contains("الذاكرة: 16.0 غ.ب"), "{ar}");
    }

    #[test]
    fn override_is_per_thread() {
        with_lang(Lang::Ar, || {
            let other = std::thread::spawn(|| with_lang(Lang::En, stopping))
                .join()
                .unwrap();
            assert_eq!(other, "Stopping.");
            assert_eq!(stopping(), "جارٍ الإيقاف.");
        });
    }

    #[test]
    fn privacy_screen_is_bilingual_and_names_viewers() {
        // The same in both CLI languages: it is read by the person at the host.
        for lang in [Lang::En, Lang::Ar] {
            let t = with_lang(lang, || {
                privacy_screen(&[PrivacyViewer {
                    name: "مكتب-Office".into(),
                    device_id: "123 456 789".into(),
                }])
            });
            assert!(t.lines[0].large && t.lines[0].is_rtl());
            assert!(t.lines[1].large && !t.lines[1].is_rtl());
            assert!(t
                .lines
                .iter()
                .any(|l| l.text == "متصل: مكتب-Office (123 456 789)"));
            assert!(t
                .lines
                .iter()
                .any(|l| l.text == "Connected: مكتب-Office (123 456 789)"));
            assert_eq!(t.sanitized(), t, "fits the privacy screen limits");
        }
        assert_ne!(
            with_lang(Lang::En, || viewer_privacy(true)),
            with_lang(Lang::Ar, || viewer_privacy(true))
        );
    }
}
