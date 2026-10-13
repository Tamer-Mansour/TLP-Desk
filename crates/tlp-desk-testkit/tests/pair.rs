//! The test kit tests itself: one call gives a working host+viewer pair, and everything the
//! host created is released when the session ends.

use tlp_desk_host::{EndReason, HostEvent};
use tlp_desk_proto::{InputEvent, Message, Permissions};
use tlp_desk_testkit::fakes::{DEFAULT_H, DEFAULT_W};
use tlp_desk_testkit::pair::{TestPair, VIEWER_NAME};
use tlp_desk_testkit::probe::ProbeFeature;
use tlp_desk_testkit::wait;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pair_streams_injects_routes_and_releases() {
    let chat = ProbeFeature::matching("chat-probe", |m| matches!(m, Message::ChatMessage { .. }));
    let probe = chat.clone();
    let mut pair = TestPair::start_with(move |host, _| host.features.push(probe)).await;
    let conn = pair.connect(Permissions::ALL).await;
    assert_eq!(pair.host.sessions()[0].peer.name, VIEWER_NAME);

    let f = wait::next_frame(&conn.session).await;
    assert_eq!((f.width, f.height), (DEFAULT_W, DEFAULT_H));

    assert!(conn
        .session
        .send_input(InputEvent::MouseMove { x: 9, y: 9 }));
    let injected = pair.injected.clone();
    wait::until("the move to be injected", || !injected.is_empty()).await;
    assert_eq!(
        pair.injected.events()[0],
        InputEvent::MouseMove { x: 9, y: 9 }
    );

    let msg = Message::ChatMessage {
        id: 1,
        text: "مرحبا \u{200F}hello ١٢٣".into(),
        sent_unix_ms: 1,
    };
    assert!(conn.session.send_message(msg.clone()));
    wait::until("the chat message to reach the probe", || {
        chat.received() == vec![msg.clone()]
    })
    .await;
    assert_eq!(chat.permissions(), Some(Permissions::ALL));
    assert!(pair.alive.total() >= 2, "capturer and injector are alive");

    conn.session.disconnect();
    let ended = pair
        .host_event(|e| matches!(e, HostEvent::SessionEnded { .. }))
        .await;
    assert!(
        matches!(
            &ended,
            HostEvent::SessionEnded {
                reason: EndReason::Remote(_),
                ..
            }
        ),
        "{ended:?}"
    );
    pair.all_released().await;
    wait::until("the probe's inbox to close", || chat.running() == 0).await;
}

/// With the fake encoder the host streams without OpenH264; the viewer cannot decode the
/// frames, but the session runs and stops cleanly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fake_encoder_pair_runs_and_releases() {
    let mut pair = TestPair::start_with_fake_encoder(|_, _| {}).await;
    let alive = pair.alive.clone();
    let conn = pair.connect(Permissions::VIEW).await;
    wait::until("the fake encoder to start", || alive.encoders() == 1).await;
    pair.host.disconnect(conn.id);
    pair.all_released().await;
    wait::until("the fake encoder to be dropped", || alive.encoders() == 0).await;
}
