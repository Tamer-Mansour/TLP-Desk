//! A viewer that is authorized but misbehaves: sends messages it has no permission for,
//! messages only a host may send, unknown monitor indices, floods and huge payloads, and
//! connects/disconnects in a loop. The host must gate, ignore or survive every case, and
//! release everything when sessions end. Built on `tlp-desk-testkit`.

use std::sync::Arc;

use tlp_desk_host::{EndReason, HostEvent};
use tlp_desk_proto::{
    AuthResult, ClipboardData, Codec, FsOp, Hello, InputEvent, Message, MonitorDesc, MouseButton,
    Permissions, QualityPreset, QualitySettings, Role, TransferDirection, TransferItem,
    TransferOffer, VideoFrame,
};
use tlp_desk_testkit::pair::TestPair;
use tlp_desk_testkit::probe::ProbeFeature;
use tlp_desk_testkit::wait;
use tlp_desk_viewer::{EndReason as ViewerEnd, ViewerEvent, ViewerSession};

/// A message every session may send (permission-free, not claimed by the core): once the probe has it, everything sent
/// before it on the control stream has been through the host's permission gate.
fn sentinel(n: u64) -> Message {
    Message::TunnelOpened { id: n }
}

async fn start_with_probe() -> (TestPair, Arc<ProbeFeature>) {
    let probe = ProbeFeature::all("probe");
    let p = probe.clone();
    let pair = TestPair::start_with(move |host, _| host.features.push(p)).await;
    (pair, probe)
}

/// Sends `msgs` then a sentinel, and waits until the probe has the sentinel.
async fn send_then_sync(session: &ViewerSession, probe: &ProbeFeature, msgs: Vec<Message>, n: u64) {
    for m in msgs {
        assert!(session.send_message(m));
    }
    assert!(session.send_message(sentinel(n)));
    wait::until("the sentinel to pass the gate", || {
        probe.received().contains(&sentinel(n))
    })
    .await;
}

/// One message per permission-gated feature (Arabic text and paths on purpose).
fn gated_messages() -> Vec<(Message, Permissions)> {
    let item = TransferItem {
        rel_path: "مجلد/ملف تجريبي.txt".into(),
        size: 3,
        modified_unix_ms: 0,
        is_dir: false,
    };
    vec![
        (
            Message::ChatMessage {
                id: 1,
                text: "مرحبا".into(),
                sent_unix_ms: 0,
            },
            Permissions::CHAT,
        ),
        (
            Message::FsRequest {
                req_id: 1,
                op: FsOp::ListDir {
                    path: r"C:\Users\مستخدم".into(),
                },
            },
            Permissions::FILES,
        ),
        (
            Message::TransferOffer(TransferOffer {
                id: 2,
                direction: TransferDirection::Download,
                source_dir: r"C:\".into(),
                dest_dir: r"D:\".into(),
                items: vec![item.clone()],
                total_bytes: 3,
                resume: vec![],
            }),
            Permissions::FILES,
        ),
        (
            Message::ClipboardData(ClipboardData::Text("نص".into())),
            Permissions::CLIPBOARD,
        ),
        (
            Message::ClipboardHtml {
                html: "<b>نص</b>".into(),
                alt_text: "نص".into(),
            },
            Permissions::CLIPBOARD,
        ),
        (
            Message::ClipboardFiles {
                transfer_id: 3,
                items: vec![item],
            },
            Permissions::CLIPBOARD | Permissions::FILES,
        ),
        (
            Message::AudioControl {
                enabled: true,
                sample_rate: 48_000,
                channels: 2,
            },
            Permissions::AUDIO,
        ),
        (Message::SendCtrlAltDel, Permissions::SYSTEM),
        (Message::LockWorkstation, Permissions::SYSTEM),
        (
            Message::RestartRemote { reconnect: true },
            Permissions::RESTART,
        ),
        (
            Message::PrivacyMode { enabled: true },
            Permissions::PRIVACY_MODE,
        ),
        (
            Message::TunnelOpen {
                id: 4,
                target_port: 3389,
                target_host: Some("10.0.0.5".into()),
            },
            Permissions::TUNNEL,
        ),
        (
            Message::WhiteboardStroke {
                id: 5,
                points: vec![(0, 0), (u16::MAX, u16::MAX)],
                color_rgba: 0xFF00_00FF,
                width: 10,
            },
            Permissions::WHITEBOARD,
        ),
        (Message::WhiteboardClear, Permissions::WHITEBOARD),
    ]
}

/// With only `VIEW`, no feature message gets past the host's gate; granting a permission
/// live lets exactly that feature's messages through, and revoking it blocks them again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn feature_messages_need_their_permission_live() {
    let (mut pair, probe) = start_with_probe().await;
    let conn = pair.connect(Permissions::VIEW).await;
    let all: Vec<Message> = gated_messages().into_iter().map(|(m, _)| m).collect();
    send_then_sync(&conn.session, &probe, all, 1).await;
    assert_eq!(
        probe.received(),
        vec![sentinel(1)],
        "a gated message leaked"
    );

    for (n, (msg, needed)) in (2u64..).zip(gated_messages()) {
        probe.clear();
        assert!(pair
            .host
            .set_permissions(conn.id, Permissions::VIEW | needed));
        wait::until("features to see the new permissions", || {
            probe.permissions() == Some(Permissions::VIEW | needed)
        })
        .await;
        send_then_sync(&conn.session, &probe, vec![msg.clone()], n).await;
        assert_eq!(
            probe.received(),
            vec![msg.clone(), sentinel(n)],
            "{needed:?}"
        );

        // Revoke again: blocked.
        probe.clear();
        assert!(pair.host.set_permissions(conn.id, Permissions::VIEW));
        wait::until("features to see the revocation", || {
            probe.permissions() == Some(Permissions::VIEW)
        })
        .await;
        send_then_sync(&conn.session, &probe, vec![msg], n + 1000).await;
        assert_eq!(
            probe.received(),
            vec![sentinel(n + 1000)],
            "{needed:?} revoked"
        );
    }
}

/// Mouse needs `CONTROL`, keys need `KEYBOARD`; nothing else reaches the injector.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn input_needs_control_and_keyboard() {
    let (mut pair, probe) = start_with_probe().await;
    let conn = pair.connect(Permissions::VIEW).await;
    let mv = InputEvent::MouseMove { x: 10, y: 20 };
    let click = InputEvent::MouseButton {
        button: MouseButton::Left,
        down: true,
    };
    let key = InputEvent::Key {
        scancode: 0x1E,
        extended: false,
        down: true,
    };
    let blocked = vec![
        Message::InputEvent(mv.clone()),
        Message::InputEvent(click.clone()),
        Message::InputEvent(InputEvent::MouseWheel { dx: 0, dy: -120 }),
        Message::InputEvent(key.clone()),
        Message::InputEvent(InputEvent::ReleaseAll),
    ];
    send_then_sync(&conn.session, &probe, blocked, 1).await;

    // Mouse only.
    pair.host
        .set_permissions(conn.id, Permissions::VIEW | Permissions::CONTROL);
    let mut events = conn.events;
    wait::viewer_event(
        &mut events,
        |e| matches!(e, ViewerEvent::PermissionsChanged(p) if p.contains(Permissions::CONTROL)),
    )
    .await;
    // Input is dropped until the pipeline published the monitor (first frame).
    wait::next_frame(&conn.session).await;
    send_then_sync(
        &conn.session,
        &probe,
        vec![
            Message::InputEvent(key.clone()),
            Message::InputEvent(mv.clone()),
        ],
        2,
    )
    .await;
    let injected = pair.injected.clone();
    wait::until("the move to be injected", || !injected.is_empty()).await;
    assert_eq!(pair.injected.events(), vec![mv.clone()]);
    assert!(probe
        .received()
        .iter()
        .all(|m| !matches!(m, Message::InputEvent(_))));
}

/// Messages only a host sends (a viewer "granting itself" permissions, fake auth results,
/// video on the control stream, ...) change nothing: the session keeps its permissions and
/// gated messages stay blocked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_only_messages_from_the_viewer_change_nothing() {
    let (mut pair, probe) = start_with_probe().await;
    let conn = pair.connect(Permissions::VIEW).await;
    let forged = vec![
        Message::Permissions(Permissions::ALL),
        Message::AuthResult(AuthResult::Accepted {
            permissions: Permissions::ALL,
        }),
        Message::Hello(Hello {
            protocol_version: 4,
            device_id: 123_456_789,
            device_name: "مزيف".into(),
            role: Role::Host,
        }),
        Message::VideoFrame(VideoFrame {
            seq: 1,
            timestamp_us: 0,
            width: 1,
            height: 1,
            codec: Codec::H264,
            keyframe: true,
            data: vec![0; 64],
        }),
        Message::MonitorList {
            monitors: vec![MonitorDesc {
                index: 9,
                name: "x".into(),
                width: 1,
                height: 1,
                primary: true,
                x: 0,
                y: 0,
            }],
            active: 9,
        },
        Message::HostInputPriority { local_active: true },
        Message::DesktopState {
            secure_desktop: true,
            locked: true,
        },
        Message::LatencyEcho {
            t_viewer_us: 1,
            t_host_capture_us: 2,
        },
        Message::TunnelOpened { id: 1 },
        Message::ReconnectTicket {
            token: vec![1; 32],
            expires_unix_ms: u64::MAX,
        },
        Message::ChatMessage {
            id: 9,
            text: "بعد التزوير".into(),
            sent_unix_ms: 0,
        },
    ];
    send_then_sync(&conn.session, &probe, forged, 1).await;
    assert_eq!(pair.host.sessions()[0].permissions, Permissions::VIEW);
    assert_eq!(probe.permissions(), Some(Permissions::VIEW));
    assert!(
        !probe
            .received()
            .iter()
            .any(|m| matches!(m, Message::ChatMessage { .. })),
        "chat passed after a forged permission grant"
    );
    // Still streaming.
    wait::next_frame(&conn.session).await;
    assert!(conn.session.end_reason().is_none());
}

/// Unknown monitor indices are ignored; a valid one still switches.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_monitor_indices_are_ignored() {
    let (mut pair, probe) = start_with_probe().await;
    let conn = pair.connect(Permissions::VIEW).await;
    let mut events = conn.events;
    wait::viewer_event(&mut events, |e| {
        matches!(e, ViewerEvent::MonitorsChanged { .. })
    })
    .await;
    let bad = [2, 7, u32::MAX - 1]
        .into_iter()
        .map(|index| Message::SelectMonitor { index })
        .collect();
    send_then_sync(&conn.session, &probe, bad, 1).await;
    assert_eq!(conn.session.monitors().1, 0);
    assert!(conn.session.select_monitor(1));
    wait::viewer_event(&mut events, |e| {
        matches!(e, ViewerEvent::MonitorsChanged { active: 1, .. })
    })
    .await;
    // Quality requests with absurd values are accepted on the wire without harm.
    send_then_sync(
        &conn.session,
        &probe,
        vec![Message::SetQuality(QualitySettings {
            preset: QualityPreset::Custom,
            max_width: u32::MAX,
            max_height: 0,
            fps: u32::MAX,
            bitrate_kbps: Some(0),
        })],
        2,
    )
    .await;
    wait::next_frame(&conn.session).await;
}

/// A flood of small messages and one near-limit payload do not end the session or starve
/// the video.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn floods_and_huge_payloads_do_not_end_the_session() {
    let (mut pair, probe) = start_with_probe().await;
    let conn = pair.connect(Permissions::VIEW).await;
    let mut flood: Vec<Message> = (0..5_000)
        .map(|i| Message::ChatMessage {
            id: i,
            text: "سيل".into(),
            sent_unix_ms: 0,
        })
        .collect();
    // ~15 MiB of Arabic clipboard text without CLIPBOARD permission.
    flood.push(Message::ClipboardData(ClipboardData::Text(
        "ن".repeat(7 * 1024 * 1024),
    )));
    send_then_sync(&conn.session, &probe, flood, 1).await;
    assert_eq!(probe.received(), vec![sentinel(1)]);
    assert!(conn.session.end_reason().is_none());
    wait::next_frame(&conn.session).await;
}

/// Removing `VIEW` ends the session on both sides and releases everything.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoking_view_ends_the_session_cleanly() {
    let (mut pair, probe) = start_with_probe().await;
    let conn = pair.connect(Permissions::ALL).await;
    wait::next_frame(&conn.session).await;
    assert!(pair.host.set_permissions(conn.id, Permissions::NONE));
    let ended = pair
        .host_event(|e| matches!(e, HostEvent::SessionEnded { .. }))
        .await;
    assert!(
        matches!(
            ended,
            HostEvent::SessionEnded {
                reason: EndReason::PermissionRevoked,
                ..
            }
        ),
        "{ended:?}"
    );
    let reason = wait::within("the viewer to end", conn.session.ended()).await;
    assert_eq!(
        reason,
        ViewerEnd::Remote(tlp_desk_proto::ByeReason::PermissionRevoked)
    );
    pair.all_released().await;
    wait::until("the feature to stop", || probe.running() == 0).await;
}

/// Connect/disconnect in a loop, ending each session a different way: no session, thread or
/// feature task is left behind, and a key held by the viewer is released every time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconnect_loop_leaves_nothing_behind() {
    let (mut pair, probe) = start_with_probe().await;
    let shift = InputEvent::Key {
        scancode: 0x2A,
        extended: false,
        down: true,
    };
    for round in 0..9u32 {
        let conn = pair.connect(Permissions::ALL).await;
        // Input is dropped until the pipeline published the monitor (first frame).
        wait::next_frame(&conn.session).await;
        pair.injected.clear();
        assert!(conn.session.send_input(shift.clone()));
        let injected = pair.injected.clone();
        wait::until("Shift to be pressed", || injected.events().contains(&shift)).await;
        match round % 3 {
            0 => conn.session.disconnect(),
            1 => assert!(pair.host.disconnect(conn.id)),
            _ => drop(conn),
        }
        pair.host_event(|e| matches!(e, HostEvent::SessionEnded { .. }))
            .await;
        pair.all_released().await;
        assert_eq!(
            pair.injected.events().last(),
            Some(&InputEvent::ReleaseAll),
            "round {round}"
        );
    }
    assert_eq!(probe.started(), 9);
    wait::until("every feature inbox to close", || probe.running() == 0).await;
}
