//! The scene registry: one entry per README screenshot.
//!
//! **Adding a scene** (e.g. when a feature lane lands file transfer, chat or the address book):
//! write a `fn(&mut App, Lang)` that puts the app into the state to show, using the fixtures
//! in `backend.rs`, and add one [`Scene`] to [`SCENES`]. Then run
//! `scripts/update-readme-media.ps1` and commit the new `docs/media/<name>-*.png` files.
//! Scene setup must be deterministic: no wall clock, no randomness, no files or network.

use std::net::SocketAddr;
use std::time::Duration;

use tlp_desk_app::app::{App, KeyWarning, Pending, Screen};
use tlp_desk_app::backend::chat::ChatLine;
use tlp_desk_app::backend::fake::{fake_monitors, test_pattern_frame};
use tlp_desk_app::backend::{
    AttemptId, ConnectTarget, DeviceId, FrameSlot, RequestId, SessionId, SessionStats,
};
use tlp_desk_app::i18n::{DigitStyle, Lang, Tx, S};
use tlp_desk_app::ui::address_book::{Dialog, EditForm, HomeTab};
use tlp_desk_app::ui::chat::ChatState;
use tlp_desk_app::ui::session::ViewerSession;
use tlp_desk_proto::Permissions;

use crate::backend::{devices, incoming_peer, pick, saved_devices, session_peer, FIXED_NOW};

/// One screenshot: `docs/media/<name>-<lang>-<theme>.png`.
pub struct Scene {
    /// File name stem; lowercase, `-` separated.
    pub name: &'static str,
    /// What the scene shows (printed by `--list`).
    pub about: &'static str,
    /// Put a freshly created app (home screen, no devices yet) into the scene's state.
    pub setup: fn(&mut App, Lang),
}

/// Every README screenshot, in gallery order. Feature lanes append their scene here.
pub const SCENES: &[Scene] = &[
    Scene {
        name: "home",
        about: "home screen: device ID, one-time code, connect box, nearby devices",
        setup: home,
    },
    Scene {
        name: "devices",
        about: "devices found on the LAN, online and offline",
        setup: device_list,
    },
    Scene {
        name: "connect-code",
        about: "connect box filled with a device ID and the host's one-time code",
        setup: connect_with_code,
    },
    Scene {
        name: "connecting",
        about: "connecting, waiting for the host user to accept",
        setup: connecting,
    },
    Scene {
        name: "incoming",
        about: "host side: incoming request with Accept / Decline",
        setup: incoming,
    },
    Scene {
        name: "host-banner",
        about: "host side: the always-visible 'who is connected' banner",
        setup: host_banner,
    },
    Scene {
        name: "session",
        about: "viewer session with toolbar and stats overlay",
        setup: session,
    },
    Scene {
        name: "monitors",
        about: "monitor picker on a two-screen host, second screen selected",
        setup: monitors,
    },
    Scene {
        name: "file-browser",
        about: "two-pane file browser: this PC and the remote PC, Arabic and English names",
        setup: file_browser,
    },
    Scene {
        name: "settings",
        about: "settings: language and digit style",
        setup: settings,
    },
    Scene {
        name: "settings-incoming",
        about: "settings: what connected devices may do, accept timeout, one-time code",
        setup: settings_incoming,
    },
    Scene {
        name: "settings-unattended",
        about: "settings: permanent password for unattended access",
        setup: settings_unattended,
    },
    Scene {
        name: "settings-network",
        about: "settings: allowed networks (CIDR list), picture quality, start-up and tray",
        setup: settings_network,
    },
    Scene {
        name: "connection-log",
        about: "connection log: Arabic and English peers, refused attempts, search and filter",
        setup: connection_log,
    },
    Scene {
        name: "key-changed",
        about: "loud warning when a host's key changed",
        setup: key_changed,
    },
    Scene {
        name: "address-book",
        about: "address book: favorites, groups, online status, Connect and Wake up",
        setup: address_book,
    },
    Scene {
        name: "address-book-edit",
        about: "address book: editing a saved device (alias, group, tags, notes, MAC)",
        setup: address_book_edit,
    },
    Scene {
        name: "wake-on-lan",
        about: "address book: Wake-on-LAN magic packet sent to an offline PC",
        setup: wake_on_lan,
    },
    Scene {
        name: "chat",
        about: "viewer session with the chat panel: Arabic and English messages",
        setup: chat,
    },
    Scene {
        name: "host-chat",
        about: "host side: chat window that opened when the viewer wrote",
        setup: host_chat,
    },
    Scene {
        name: "recording",
        about: "viewer session while recording: red indicator with the elapsed time",
        setup: recording,
    },
    Scene {
        name: "whiteboard",
        about: "viewer session in drawing mode: tools bar and a few strokes on the remote screen",
        setup: whiteboard,
    },
    Scene {
        name: "tunnels",
        about: "viewer session with the port forwarding window: two rules with connection counts",
        setup: tunnels,
    },
    Scene {
        name: "device-info",
        about: "viewer session with the device info window: system, memory, uptime and disk bars",
        setup: device_info,
    },
    Scene {
        name: "transfer-queue",
        about: "viewer session with the transfer queue: running, paused and finished transfers",
        setup: transfer_queue,
    },
    Scene {
        name: "transfer-conflict",
        about: "the 'files already exist' dialog: existing and new version, choice per file",
        setup: transfer_conflict,
    },
    Scene {
        name: "host-transfers",
        about: "host side: banner button and the File transfers window with who sends what",
        setup: host_transfers,
    },
    Scene {
        name: "accept-popup",
        about: "the Accept window: countdown ring, permission chips (Files switched off), trust",
        setup: accept_popup,
    },
];
/// Find a scene by name.
pub fn find(name: &str) -> Option<&'static Scene> {
    SCENES.iter().find(|s| s.name == name)
}

/// Switch the digit style without saving settings (scenes never touch the real profile).
fn use_digits(app: &mut App, digits: DigitStyle) {
    app.settings.digits = digits;
    app.tx = Tx::new(app.settings.lang, digits);
}

fn home(app: &mut App, lang: Lang) {
    app.devices = Some(devices(lang));
}

fn device_list(app: &mut App, lang: Lang) {
    home(app, lang);
    if lang == Lang::Ar {
        // Show off the Arabic-Indic digit option on one of the Arabic shots.
        use_digits(app, DigitStyle::ArabicIndic);
    }
}

fn connect_with_code(app: &mut App, lang: Lang) {
    home(app, lang);
    app.address = "715 204 386".into();
    app.code = "275 931".into();
}

fn connecting(app: &mut App, lang: Lang) {
    connect_with_code(app, lang);
    app.code.clear();
    app.pending = Some(Pending {
        attempt: AttemptId(1),
        target: session_peer(lang).name,
        awaiting_accept: true,
        dial: ConnectTarget::Id(DeviceId(715_204_386)),
        code: None,
    });
}

fn incoming(app: &mut App, lang: Lang) {
    home(app, lang);
    app.incoming.push_back((RequestId(1), incoming_peer(lang)));
}

fn host_banner(app: &mut App, lang: Lang) {
    home(app, lang);
    app.host_sessions.push((SessionId(2), incoming_peer(lang)));
}

/// A viewer session on monitor `active` with a fixed frame, stats and duration.
pub fn start_session(app: &mut App, lang: Lang, active: u32) {
    let frames = FrameSlot::new();
    frames.publish(test_pattern_frame(active, 3.2, (30_000, 26_000)));
    let mut view = ViewerSession::new(SessionId(1), session_peer(lang), Permissions::ALL, frames);
    view.stats = Some(SessionStats {
        fps: 60.0,
        latency_ms: 17.0,
        latency_synced: true,
        bitrate_kbps: 12_400,
        qp: 0,
    });
    view.set_fixed_elapsed(Some(Duration::from_secs(4 * 60 + 27)));
    view.set_monitors(fake_monitors(), active);
    app.session = Some(view);
}

fn session(app: &mut App, lang: Lang) {
    home(app, lang);
    start_session(app, lang, 0);
}

fn monitors(app: &mut App, lang: Lang) {
    home(app, lang);
    start_session(app, lang, 1);
    if lang == Lang::Ar {
        // Screen buttons ١ / ٢ as described in the Arabic user guide.
        use_digits(app, DigitStyle::ArabicIndic);
    }
}

/// A fixed directory entry (dates in 2025, so nothing depends on the clock).
fn fs_entry(name: &str, is_dir: bool, size: u64, day: i64) -> tlp_desk_proto::FsEntry {
    tlp_desk_proto::FsEntry {
        name: name.to_owned(),
        is_dir,
        size,
        modified_unix_ms: (1_735_689_600 + day * 86_400 + 9 * 3_600) * 1000,
        readonly: false,
        hidden: false,
    }
}

fn file_browser(app: &mut App, lang: Lang) {
    home(app, lang);
    start_session(app, lang, 0);
    let Some(view) = app.session.as_mut() else {
        return;
    };
    let local = vec![
        fs_entry("Projects", true, 0, 40),
        fs_entry("الصور", true, 0, 12),
        fs_entry("invoice-2025.pdf", false, 482_113, 33),
        fs_entry("تقرير الربع الأول.docx", false, 96_204, 28),
        fs_entry("notes.txt", false, 2_048, 5),
    ];
    let remote = vec![
        fs_entry("Backups", true, 0, 60),
        fs_entry("المستندات", true, 0, 21),
        fs_entry("اقرأني.txt", false, 612, 14),
        fs_entry("photos.zip", false, 48_220_331, 30),
        fs_entry("setup.exe", false, 12_883_968, 19),
    ];
    view.files.show_fixture(
        (r"C:\Users\me\Documents", local, "invoice-2025.pdf"),
        (r"D:\Shared", remote, "photos.zip"),
    );
}

fn settings(app: &mut App, lang: Lang) {
    home(app, lang);
    app.screen = Screen::Settings;
    if lang == Lang::Ar {
        use_digits(app, DigitStyle::ArabicIndic);
    }
}

fn address_book(app: &mut App, lang: Lang) {
    home(app, lang);
    app.address_book.fixed_now = Some(FIXED_NOW);
    app.address_book.book = saved_devices(lang);
    app.address_book.tab = HomeTab::AddressBook;
    let backend = app.backend.clone();
    let list = devices(lang);
    let tx = app.tx;
    app.address_book.on_devices(&list, backend.as_ref(), tx);
}

fn address_book_edit(app: &mut App, lang: Lang) {
    address_book(app, lang);
    if let Some(e) = app.address_book.book.get(DeviceId(950_442_118)) {
        app.address_book.dialog = Some(Dialog::Edit(EditForm::edit(e)));
    }
}

fn wake_on_lan(app: &mut App, lang: Lang) {
    address_book(app, lang);
    let name = pick(lang, "Lab laptop", "حاسوب المختبر المحمول");
    let text = app.tx.f(S::AbWakeSent, &[("name", &name)]);
    app.address_book.show_toast(text, false);
}

fn settings_incoming(app: &mut App, lang: Lang) {
    settings(app, lang);
    app.settings_ui.scroll_offset = Some(430.0);
}

fn settings_unattended(app: &mut App, lang: Lang) {
    settings(app, lang);
    app.settings_ui.scroll_offset = Some(800.0);
}

fn settings_network(app: &mut App, lang: Lang) {
    settings(app, lang);
    app.settings.host.allow_list = vec!["192.168.1.0/24".into(), "10.8.0.0/16".into()];
    app.settings_ui.scroll_offset = Some(1390.0);
}

fn connection_log(app: &mut App, lang: Lang) {
    use tlp_desk_app::connection_log::{Direction, LogAuth, LogEnd, LogEntry};
    use tlp_desk_app::settings::PermissionSet;
    use tlp_desk_app::ui::connection_log::ConnLogUi;
    home(app, lang);
    let base: u64 = 1_760_000_000_000;
    let entry = |hours: u64,
                 name: &str,
                 id: u32,
                 addr: &str,
                 direction: Direction,
                 auth: LogAuth,
                 minutes: u64,
                 end: LogEnd| LogEntry {
        started_ms: base + hours * 3_600_000,
        duration_ms: minutes * 60_000,
        direction,
        device_id: id,
        name: name.into(),
        address: addr.into(),
        auth,
        permissions: PermissionSet(
            Permissions::VIEW | Permissions::CONTROL | Permissions::KEYBOARD | Permissions::FILES,
        ),
        end,
        ..LogEntry::default()
    };
    let entries = vec![
        entry(
            0,
            "Office-PC",
            482_913_057,
            "192.168.1.20",
            Direction::Incoming,
            LogAuth::AcceptPrompt,
            42,
            LogEnd::ClosedRemotely,
        ),
        entry(
            5,
            "حاسوب سارة",
            731_204_586,
            "192.168.1.31",
            Direction::Incoming,
            LogAuth::OneTimeCode,
            7,
            LogEnd::ClosedHere,
        ),
        entry(
            9,
            "Laptop-Ali",
            118_004_372,
            "192.168.1.77",
            Direction::Incoming,
            LogAuth::OneTimeCode,
            0,
            LogEnd::WrongCredentials,
        ),
        entry(
            26,
            "خادم المحاسبة",
            905_551_220,
            "192.168.1.5",
            Direction::Outgoing,
            LogAuth::Password,
            125,
            LogEnd::ClosedHere,
        ),
        entry(
            30,
            "Reception",
            264_380_911,
            "192.168.1.44",
            Direction::Incoming,
            LogAuth::AcceptPrompt,
            0,
            LogEnd::Declined,
        ),
    ];
    app.conn_log = ConnLogUi::with_fixed(entries, time::UtcOffset::UTC);
    app.screen = Screen::ConnectionLog;
    if lang == Lang::Ar {
        use_digits(app, DigitStyle::ArabicIndic);
    }
}

fn key_changed(app: &mut App, lang: Lang) {
    home(app, lang);
    app.key_warning = Some(KeyWarning {
        attempt: AttemptId(3),
        target: "192.168.1.66".into(),
        dial: ConnectTarget::Address(SocketAddr::from((
            [192, 168, 1, 66],
            ConnectTarget::DEFAULT_PORT,
        ))),
        code: None,
        previous: "3f9a 1c07 e2b4 58d1".into(),
        presented: "b70e 44a2 9c13 0f6e".into(),
    });
}

/// A short conversation mixing Arabic and English in both directions (fixed times). On the
/// host side (`host`) the authors are swapped.
fn conversation(host: bool) -> ChatState {
    let mut chat = ChatState::new(true);
    chat.set_open(true);
    let lines = [
        (
            false,
            "مرحباً! هل ترى ملف report-2026.pdf على الشاشة؟",
            (10, 41),
        ),
        (true, "Yes, opening it now.", (10, 42)),
        (false, "Thanks 👍 page 3 please", (10, 42)),
        (true, "تم، الصفحة 3 جاهزة الآن.", (10, 43)),
    ];
    for (id, (mine, text, hm)) in (1..).zip(lines) {
        let line = ChatLine {
            id,
            mine: mine != host,
            text: text.into(),
            at_unix_ms: 0,
        };
        chat.push_at(line, hm);
    }
    chat
}

fn chat(app: &mut App, lang: Lang) {
    session(app, lang);
    if let Some(s) = app.session.as_mut() {
        s.chat = conversation(false);
        s.chat.draft = match lang {
            Lang::En => "One more question…".into(),
            Lang::Ar => "سؤال أخير…".into(),
        };
    }
}

fn recording(app: &mut App, lang: Lang) {
    session(app, lang);
    if let Some(s) = app.session.as_mut() {
        // 12:34 into the recording (the clock only moves by milliseconds while rendering).
        let since = std::time::Instant::now()
            .checked_sub(Duration::from_secs(12 * 60 + 34))
            .unwrap_or_else(std::time::Instant::now);
        s.recording = tlp_desk_app::ui::recordings::RecordState::On { since };
    }
}

fn whiteboard(app: &mut App, lang: Lang) {
    use tlp_desk_input::whiteboard::{Rgba, Stroke, WhiteboardOp};
    session(app, lang);
    if let Some(s) = app.session.as_mut() {
        s.whiteboard.set_active(true);
        let now = std::time::Instant::now();
        // A red circle around something, and a blue arrow pointing at it.
        let circle: Vec<(u16, u16)> = (0..=36u32)
            .map(|i| {
                let a = f64::from(i) * std::f64::consts::TAU / 36.0;
                (
                    (30_000.0 + 7_000.0 * a.cos()) as u16,
                    (26_000.0 + 9_000.0 * a.sin()) as u16,
                )
            })
            .collect();
        let strokes = [
            (1, circle, Rgba::new(0xE5, 0x39, 0x35, 0xFF), 90),
            (
                2,
                vec![(48_000, 12_000), (40_500, 21_000)],
                Rgba::new(0x1E, 0x88, 0xE5, 0xFF),
                90,
            ),
            (
                3,
                vec![(40_500, 21_000), (43_500, 20_300)],
                Rgba::new(0x1E, 0x88, 0xE5, 0xFF),
                90,
            ),
            (
                4,
                vec![(40_500, 21_000), (41_300, 17_800)],
                Rgba::new(0x1E, 0x88, 0xE5, 0xFF),
                90,
            ),
        ];
        for (id, points, color, width) in strokes {
            let stroke = Stroke {
                id,
                points,
                color,
                width,
            };
            s.whiteboard.remote(&WhiteboardOp::Stroke(stroke), now);
        }
    }
}

fn tunnels(app: &mut App, lang: Lang) {
    use tlp_desk_app::backend::tunnels::TunnelRow;
    session(app, lang);
    if let Some(s) = app.session.as_mut() {
        s.tunnels.open = true;
        s.tunnels.rules = vec![
            TunnelRow {
                id: 1,
                local: std::net::SocketAddr::from(([127, 0, 0, 1], 8080)),
                target: "localhost:80".to_owned(),
                active: 3,
                connections: 42,
                failed: 0,
                bytes_sent: 18_400,
                bytes_received: 6_300_000,
            },
            TunnelRow {
                id: 2,
                local: std::net::SocketAddr::from(([127, 0, 0, 1], 5433)),
                target: "192.168.1.20:5432".to_owned(),
                active: 1,
                connections: 5,
                failed: 1,
                bytes_sent: 92_000,
                bytes_received: 410_000,
            },
        ];
    }
}

fn device_info(app: &mut App, lang: Lang) {
    session(app, lang);
    if let Some(s) = app.session.as_mut() {
        s.device_info.open = true;
        s.device_info
            .set_info(tlp_desk_app::backend::device_info::fake_info());
    }
}

fn host_chat(app: &mut App, lang: Lang) {
    host_banner(app, lang);
    app.host_chats.insert(SessionId(2), conversation(true));
}

fn transfer_view(
    id: u64,
    dir: tlp_desk_app::backend::files::TransferDir,
    label: &str,
    status: tlp_desk_app::backend::files::TransferStatus,
    (done, total): (u64, u64),
    files: (u32, u32),
    local: bool,
) -> tlp_desk_app::backend::files::TransferView {
    use tlp_desk_app::backend::files::{TransferId, TransferStatus, TransferView};
    let running = status == TransferStatus::Running;
    TransferView {
        id: TransferId(id),
        dir,
        label: label.to_owned(),
        status,
        bytes_done: done,
        total_bytes: total,
        files_done: files.0,
        total_files: files.1,
        failed_files: 0,
        bytes_per_sec: if running { 38_000_000 } else { 0 },
        local_dest: Some(r"C:\Users\me\Downloads".into()),
        local,
        started_unix_ms: 1_760_000_000_000 + id * 1000,
    }
}

fn transfer_queue(app: &mut App, lang: Lang) {
    use tlp_desk_app::backend::files::{TransferDir::*, TransferStatus::*};
    session(app, lang);
    if let Some(s) = app.session.as_mut() {
        let mb = 1_000_000;
        for v in [
            transfer_view(
                1,
                Send,
                "تقرير المشروع",
                Completed,
                (96 * mb, 96 * mb),
                (12, 12),
                true,
            ),
            transfer_view(
                2,
                Receive,
                "photos.zip",
                Paused,
                (310 * mb, 480 * mb),
                (1, 1),
                true,
            ),
            transfer_view(
                3,
                Send,
                "مجلد الصور 2025",
                Running,
                (640 * mb, 1_200 * mb),
                (212, 400),
                true,
            ),
        ] {
            s.transfers.update(v);
        }
        s.transfers.toasts.clear();
    }
}

fn transfer_conflict(app: &mut App, lang: Lang) {
    use tlp_desk_app::backend::files::{ConflictPrompt, FileConflict, TransferId};
    session(app, lang);
    let now_ms: u64 = 1_760_000_000_000;
    let hours = |h: u64| i64::try_from(now_ms - h * 3_600_000).unwrap_or(0);
    let file = |index, name: &str, old: (u64, u64), new: (u64, u64), partial| FileConflict {
        index,
        rel_path: name.to_owned(),
        existing_size: old.0,
        existing_modified_unix_ms: hours(old.1),
        incoming_size: new.0,
        incoming_modified_unix_ms: hours(new.1),
        partial_bytes: partial,
    };
    app.conflicts.fixed_now_ms = Some(now_ms);
    app.conflicts.push(
        ConflictPrompt {
            id: TransferId(7),
            local: true,
            label: "المستندات".to_owned(),
            dest_dir: r"C:\Users\me\Downloads".to_owned(),
            total: 3,
            files: vec![
                file(0, "تقرير الربع الأول.docx", (96_204, 72), (98_880, 5), None),
                file(
                    1,
                    "photos.zip",
                    (48_220_331, 24),
                    (48_220_331, 30),
                    Some(33_554_432),
                ),
                file(2, "notes.txt", (2_048, 5), (1_024, 5), None),
            ],
        },
        "",
    );
}

fn host_transfers(app: &mut App, lang: Lang) {
    use tlp_desk_app::backend::files::{TransferDir::*, TransferStatus::*};
    host_banner(app, lang);
    let peer = incoming_peer(lang);
    let who = format!("{} ({})", peer.name, peer.device_id);
    let mb = 1_000_000;
    for v in [
        transfer_view(
            1,
            Receive,
            "عرض المبيعات.pptx",
            Running,
            (22 * mb, 60 * mb),
            (0, 1),
            false,
        ),
        transfer_view(
            2,
            Send,
            "backup-notes",
            Completed,
            (8 * mb, 8 * mb),
            (4, 4),
            false,
        ),
    ] {
        app.host_transfers.update(v, &who);
    }
}

fn accept_popup(app: &mut App, lang: Lang) {
    use tlp_desk_app::prompt::PromptRequest;
    use tlp_desk_app::ui::accept_window::AcceptCard;
    home(app, lang);
    let peer = tlp_desk_app::backend::PeerInfo {
        name: pick(lang, "Reception-Desk-2", "مكتب-الاستقبال-٢"),
        ..incoming_peer(lang)
    };
    let mut card = AcceptCard::new(PromptRequest {
        peer_name: peer.name.clone(),
        device_id: peer.device_id.0,
        address: peer.address.to_string(),
        permissions: app.settings.host.default_permissions,
        timeout_secs: app.settings.host.accept_timeout_secs,
        lang,
        ..PromptRequest::default()
    });
    card.permissions = card.permissions.difference(Permissions::FILES);
    card.trust = true;
    // Twelve seconds after it appeared: the countdown ring has started to empty.
    card.set_elapsed(0.0, 12.0);
    app.incoming.push_back((RequestId(1), peer));
    app.accept_card = Some((RequestId(1), card));
}
