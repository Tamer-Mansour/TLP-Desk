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
use tlp_desk_host::{CancelReason, ConnectedViewer, EndReason as HostEnd, IncomingRequest};
use tlp_desk_net::{AuthKind, DiscoveredDevice, PeerInfo};
use tlp_desk_proto::{Permissions, RejectReason};
use tlp_desk_viewer::{EndReason as ViewerEnd, MonitorDesc, ViewerStats};

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
    t(format!("error: {err:#}"), format!("خطأ: {err:#}"))
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
    t(
        format!(
            "[{:>3}s] {:>5.1} fps  {:>8.0} kbps  rtt {:>5.1} ms  latency ~{:>5.1} ms  decode {:>5.1} ms  {}x{}  frames {}/{}",
            t_elapsed.as_secs(), s.fps, s.bitrate_kbps, s.rtt_ms, s.latency_ms, s.decode_ms,
            s.width, s.height, s.frames_decoded, s.frames_received
        ),
        format!(
            "[{:>3} ث] {:>5.1} إطار/ث  {:>8.0} ك.بت/ث  ذهاب وإياب {:>5.1} م.ث  تأخير ~{:>5.1} م.ث  فك {:>5.1} م.ث  {}x{}  إطارات {}/{}",
            t_elapsed.as_secs(), s.fps, s.bitrate_kbps, s.rtt_ms, s.latency_ms, s.decode_ms,
            s.width, s.height, s.frames_decoded, s.frames_received
        ),
    )
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

// --- files-transfer lane: send-file / get-file ---

/// Bytes with a binary unit and ASCII digits (`1.5 GiB`), in both languages.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    let unit = UNITS.get(u).copied().unwrap_or("B");
    if u == 0 {
        format!("{n} {unit}")
    } else {
        format!("{v:.1} {unit}")
    }
}

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
    fn override_is_per_thread() {
        with_lang(Lang::Ar, || {
            let other = std::thread::spawn(|| with_lang(Lang::En, stopping))
                .join()
                .unwrap();
            assert_eq!(other, "Stopping.");
            assert_eq!(stopping(), "جارٍ الإيقاف.");
        });
    }
}
