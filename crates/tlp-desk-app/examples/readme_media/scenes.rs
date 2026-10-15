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
use tlp_desk_app::backend::fake::{fake_monitors, test_pattern_frame};
use tlp_desk_app::backend::{
    AttemptId, ConnectTarget, DeviceId, FrameSlot, RequestId, SessionId, SessionStats,
};
use tlp_desk_app::i18n::{DigitStyle, Lang, Tx, S};
use tlp_desk_app::ui::address_book::{Dialog, EditForm, HomeTab};
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
        name: "settings",
        about: "settings: language and digit style",
        setup: settings,
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
        bitrate_kbps: 12_400,
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
