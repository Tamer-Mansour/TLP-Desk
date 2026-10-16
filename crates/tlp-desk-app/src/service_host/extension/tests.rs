use std::sync::atomic::{AtomicU32, Ordering};

use tlp_desk_host::service::ServiceExtension;
use tlp_desk_net::{ChatEntry, ChatAuthor, ChatEvent, FeatureEvent, CHAT_FEATURE};

use super::*;

fn ext(dir: &Path) -> (Arc<AppServiceExtension>, Arc<AtomicU32>) {
    let protected = Arc::new(AtomicU32::new(0));
    let p = protected.clone();
    let e = AppServiceExtension::new(
        dir.to_path_buf(),
        Arc::new(move |_| {
            p.fetch_add(1, Ordering::SeqCst);
        }),
    );
    (e, protected)
}

fn reply(e: &AppServiceExtension, cmd: &UiCommand) -> UiReply {
    wire::decode(&e.app_command(&wire::encode(cmd)).unwrap()).unwrap()
}

#[test]
fn settings_from_the_gui_are_validated_saved_and_protected() {
    let dir = tempfile::tempdir().unwrap();
    let (e, protected) = ext(dir.path());
    assert_eq!(e.settings(), HostSettings::default());
    let json = r#"{"accept_timeout_secs": 3, "allow_one_time_code": false,
                   "allow_list": ["10.0.0.0/8", "bad"], "ask_on_upload_conflict": true}"#;
    assert_eq!(
        reply(&e, &UiCommand::ApplySettings { json: json.into() }),
        UiReply::Ok
    );
    let s = e.settings();
    assert_eq!(s.accept_timeout_secs, 10, "clamped");
    assert!(!s.allow_one_time_code);
    assert_eq!(s.allow_list, vec!["10.0.0.0/8".to_string()]);
    assert_eq!(protected.load(Ordering::SeqCst), 1);
    // Saved: a restarted service starts with them.
    assert_eq!(settings::load(dir.path()), s);
    let (again, _) = ext(dir.path());
    assert_eq!(again.settings(), s);
    // Read back over the pipe.
    let UiReply::Settings(j) = reply(&e, &UiCommand::GetSettings) else {
        panic!("settings expected");
    };
    assert_eq!(settings::parse(&j).unwrap(), s);
    // Garbage is refused and changes nothing.
    assert!(matches!(
        reply(
            &e,
            &UiCommand::ApplySettings {
                json: "[1,2".into()
            }
        ),
        UiReply::Error(_)
    ));
    assert_eq!(e.settings(), s);
}

#[test]
fn unknown_or_malformed_app_commands_are_not_answered() {
    let dir = tempfile::tempdir().unwrap();
    let (e, _) = ext(dir.path());
    assert!(e.app_command(&[255, 255, 255]).is_none());
    assert!(matches!(
        reply(
            &e,
            &UiCommand::ResolveConflict {
                id: 77,
                answer: None
            }
        ),
        UiReply::Error(_)
    ));
    // Chat in a session that does not exist.
    assert!(matches!(
        reply(
            &e,
            &UiCommand::SendChat {
                session: 5,
                text: "مرحبا".into()
            }
        ),
        UiReply::Error(_)
    ));
    assert!(e.app_snapshot().is_empty());
}

#[test]
fn chat_and_privacy_feature_events_become_ui_events() {
    let entry = ChatEntry {
        id: 3,
        author: ChatAuthor::Peer,
        text: "مرحبا من سارة".into(),
        sent_unix_ms: 1,
        local_unix_ms: 2,
    };
    let ev = FeatureEvent::new(CHAT_FEATURE, 9, ChatEvent::Received(entry));
    match ui_event(9, &ev) {
        Some(UiEvent::Chat { session: 9, line }) => {
            assert_eq!(line.text, "مرحبا من سارة");
            assert!(!line.mine);
        }
        other => panic!("{other:?}"),
    }
    let ev = FeatureEvent::new(CHAT_FEATURE, 9, ChatEvent::Availability(false));
    assert_eq!(
        ui_event(9, &ev),
        Some(UiEvent::ChatAvailability {
            session: 9,
            allowed: false
        })
    );
    let ev = FeatureEvent::new(
        tlp_desk_host::PRIVACY_FEATURE,
        9,
        tlp_desk_host::PrivacyStatus {
            active: true,
            cause: tlp_desk_host::PrivacyCause::Requested,
        },
    );
    assert_eq!(
        ui_event(9, &ev),
        Some(UiEvent::Privacy {
            session: 9,
            active: true
        })
    );
    assert_eq!(
        ui_event(9, &FeatureEvent::new("files", 9, 1u8)),
        None
    );
}

#[test]
fn file_access_follows_the_console_user() {
    let dir = tempfile::tempdir().unwrap();
    let (e, _) = ext(dir.path());
    let home = dir.path().join("Users").join("تامر");
    std::fs::create_dir_all(home.join("Desktop")).unwrap();
    e.console_user(Some(ConsoleUser {
        session: 1,
        profile: home.clone(),
        desktop: home.join("Desktop"),
        downloads: home.join("Downloads"),
    }));
    // Logged off: nothing reachable again.
    e.console_user(None);
    // The engine refuses everything; nothing panics without a running service.
    assert!(e.app_snapshot().is_empty());
}
