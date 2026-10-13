//! Chat end to end: a real host and a real viewer on loopback QUIC, each with its own
//! `ChatFeature`, exchanging Arabic and mixed-direction messages; permission revocation, the
//! length cap and the peer rate limit.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tlp_desk_capture::{BgraFrame, CaptureError, CursorState, MonitorInfo, ScreenCapturer};
use tlp_desk_crypto::Identity;
use tlp_desk_host::{Host, HostConfig, HostEvent, HostEvents, SessionId, DEFAULT_PERMISSIONS};
use tlp_desk_input::{InputError, InputEvent, InputInjector, LocalActivity};
use tlp_desk_net::{
    ChatAuthor, ChatEvent, ChatFeature, ChatLimits, ChatRejected, Endpoint, EndpointConfig,
    Feature, FeatureCtx, FeatureSet, CHAT_FEATURE,
};
use tlp_desk_proto::{Message, Permissions};
use tlp_desk_viewer::{
    AuthMethod, ConnectTarget, KnownHostsStore, Viewer, ViewerConfig, ViewerEvent, ViewerEvents,
    ViewerSession,
};
use tokio::sync::mpsc;

const WAIT: Duration = Duration::from_secs(15);

/// A screen that never changes (no video needed for chat).
struct IdleCapturer(MonitorInfo);

impl ScreenCapturer for IdleCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<BgraFrame>, CaptureError> {
        std::thread::sleep(timeout.min(Duration::from_millis(50)));
        Ok(None)
    }

    fn cursor(&mut self) -> Result<CursorState, CaptureError> {
        Ok(CursorState {
            x: 0,
            y: 0,
            visible: false,
            shape: None,
        })
    }

    fn monitor(&self) -> &MonitorInfo {
        &self.0
    }
}

fn monitor() -> MonitorInfo {
    MonitorInfo {
        index: 0,
        name: "FAKE".into(),
        width: 320,
        height: 240,
        primary: true,
        x: 0,
        y: 0,
    }
}

struct NoInput;

impl InputInjector for NoInput {
    fn inject(&mut self, _: &InputEvent) -> Result<(), InputError> {
        Ok(())
    }
}

struct NoLocalInput;

impl LocalActivity for NoLocalInput {
    fn last_physical_input(&self) -> Option<std::time::Instant> {
        None
    }
}

/// Test-only feature that reports every `DeviceInfoRequest` the host core routed to features.
/// The core routes control messages in arrival order, so once the marker arrives every
/// message the viewer sent before it has passed the host's permission check.
struct Marker(mpsc::UnboundedSender<()>);

impl Feature for Marker {
    fn name(&self) -> &'static str {
        "test-marker"
    }

    fn handles(&self, msg: &Message) -> bool {
        matches!(msg, Message::DeviceInfoRequest)
    }

    fn start(&self, _ctx: FeatureCtx, mut inbox: mpsc::UnboundedReceiver<Message>) {
        let tx = self.0.clone();
        tokio::spawn(async move {
            while inbox.recv().await.is_some() {
                let _ = tx.send(());
            }
        });
    }
}

fn endpoint(listen: bool) -> Arc<Endpoint> {
    Arc::new(
        Endpoint::bind(
            Arc::new(Identity::generate().unwrap()),
            EndpointConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                listen,
                device_name: if listen {
                    "حاسوب المضيف"
                } else {
                    "viewer-pc"
                }
                .into(),
                ..EndpointConfig::default()
            },
        )
        .unwrap(),
    )
}

struct Chat {
    host: Host,
    host_events: HostEvents,
    host_chat: Arc<ChatFeature>,
    host_session: SessionId,
    session: ViewerSession,
    viewer_events: ViewerEvents,
    viewer_chat: Arc<ChatFeature>,
    markers: mpsc::UnboundedReceiver<()>,
}

async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, fut)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// Host and viewer connected with a one-time code; both register chat (`host_limits` on the
/// host side).
async fn connect(host_limits: ChatLimits) -> Chat {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_test_writer()
        .try_init();
    let host_chat = ChatFeature::with_limits(host_limits);
    let viewer_chat = ChatFeature::new();
    let (marker_tx, markers) = mpsc::unbounded_channel();
    let cfg = HostConfig {
        advertise: false,
        default_permissions: DEFAULT_PERMISSIONS | Permissions::CHAT,
        capturer_factory: Arc::new(|_| {
            Ok(Box::new(IdleCapturer(monitor())) as Box<dyn ScreenCapturer>)
        }),
        monitor_lister: Arc::new(|| Ok(vec![monitor()])),
        injector_factory: Arc::new(|_| Ok(Box::new(NoInput) as Box<dyn InputInjector>)),
        local_activity_factory: Arc::new(|| Ok(Arc::new(NoLocalInput) as Arc<dyn LocalActivity>)),
        remote_pointer_marker: false,
        features: FeatureSet::new()
            .with(host_chat.clone())
            .with(Arc::new(Marker(marker_tx))),
        ..HostConfig::default()
    };
    let host_ep = endpoint(true);
    let target = ConnectTarget::IdAt(host_ep.device_id(), host_ep.local_addr().unwrap());
    let (host, mut host_events) = Host::start(host_ep, cfg).await.unwrap();
    let viewer = Viewer::new(
        endpoint(false),
        ViewerConfig {
            known_hosts: KnownHostsStore::InMemory,
            features: FeatureSet::new().with(viewer_chat.clone()),
            ..ViewerConfig::default()
        },
    )
    .unwrap();
    let code = host.one_time_code().current();
    let (session, viewer_events) = within(
        "connect",
        viewer.connect(target, AuthMethod::OneTimeCode(code)),
    )
    .await
    .unwrap();
    let host_session = match host_event(&mut host_events, |e| {
        matches!(e, HostEvent::SessionStarted(_))
    })
    .await
    {
        HostEvent::SessionStarted(v) => v.id,
        _ => unreachable!(),
    };
    Chat {
        host,
        host_events,
        host_chat,
        host_session,
        session,
        viewer_events,
        viewer_chat,
        markers,
    }
}

async fn host_event(events: &mut HostEvents, pred: impl Fn(&HostEvent) -> bool) -> HostEvent {
    within("host event", async {
        loop {
            let ev = events.recv().await.expect("host events closed");
            if pred(&ev) {
                return ev;
            }
        }
    })
    .await
}

/// Next chat event on the host side.
async fn host_chat_event(c: &mut Chat) -> ChatEvent {
    let ev = host_event(
        &mut c.host_events,
        |e| matches!(e, HostEvent::Feature { event, .. } if event.feature == CHAT_FEATURE),
    )
    .await;
    match ev {
        HostEvent::Feature { id, event } => {
            assert_eq!(id, c.host_session);
            assert_eq!(event.session_key, c.host_session.0);
            event.payload::<ChatEvent>().cloned().expect("chat payload")
        }
        _ => unreachable!(),
    }
}

/// Next chat event on the viewer side.
async fn viewer_chat_event(c: &mut Chat) -> ChatEvent {
    let key = c.session.feature_key();
    within("viewer chat event", async {
        loop {
            match c.viewer_events.recv().await.expect("viewer events closed") {
                ViewerEvent::Feature(event) if event.feature == CHAT_FEATURE => {
                    assert_eq!(event.session_key, key);
                    return event.payload::<ChatEvent>().cloned().expect("chat payload");
                }
                ViewerEvent::Ended(r) => panic!("session ended: {r:?}"),
                _ => {}
            }
        }
    })
    .await
}

async fn host_received(c: &mut Chat) -> String {
    loop {
        if let ChatEvent::Received(e) = host_chat_event(c).await {
            assert_eq!(e.author, ChatAuthor::Peer);
            return e.text;
        }
    }
}

async fn viewer_received(c: &mut Chat) -> String {
    loop {
        if let ChatEvent::Received(e) = viewer_chat_event(c).await {
            assert_eq!(e.author, ChatAuthor::Peer);
            return e.text;
        }
    }
}

/// Waits until the host core routed everything the viewer sent so far.
async fn sync_host(c: &mut Chat) {
    assert!(c.session.send_message(Message::DeviceInfoRequest));
    within("marker", c.markers.recv()).await.expect("marker");
}

fn raw_chat(id: u64, text: &str) -> Message {
    Message::ChatMessage {
        id,
        text: text.into(),
        sent_unix_ms: 1_700_000_000_000,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn arabic_and_mixed_messages_both_ways() {
    let mut c = connect(ChatLimits::default()).await;
    assert_eq!(host_chat_event(&mut c).await, ChatEvent::Availability(true));
    assert_eq!(
        viewer_chat_event(&mut c).await,
        ChatEvent::Availability(true)
    );
    let key = c.session.feature_key();

    let viewer_lines = [
        "مرحباً، هل ترى الشاشة الآن؟",
        "افتح الملف report-2026.pdf من فضلك (الصفحة 3)",
        // RLM, ZWJ, ZWNJ and an emoji must arrive byte for byte.
        "\u{200F}نص\u{200D}مع\u{200C}علامات 👍 OK",
        "line one\nالسطر الثاني",
    ];
    for line in viewer_lines {
        let sent = c.viewer_chat.send(key, line).unwrap();
        assert_eq!(sent.author, ChatAuthor::Me);
        assert_eq!(host_received(&mut c).await, line);
    }
    let host_lines = ["نعم، أراها بوضوح", "Sure — سأفتحه الآن: 192.168.1.40"];
    for line in host_lines {
        c.host_chat.send(c.host_session.0, line).unwrap();
        assert_eq!(viewer_received(&mut c).await, line);
    }

    // Both histories hold the whole conversation in order, with the right authors.
    let host_history = c.host_chat.history(c.host_session.0);
    let viewer_history = c.viewer_chat.history(key);
    assert_eq!(host_history.len(), 6);
    assert_eq!(viewer_history.len(), 6);
    for (h, v) in host_history.iter().zip(&viewer_history) {
        assert_eq!(h.text, v.text);
        assert_ne!(h.author, v.author);
    }

    // History is per session and forgotten at the end.
    c.session.disconnect();
    host_event(&mut c.host_events, |e| {
        matches!(e, HostEvent::SessionEnded { .. })
    })
    .await;
    within("chat cleanup", async {
        while c.host_chat.is_allowed(c.host_session.0).is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(c.host_chat.history(c.host_session.0).is_empty());
    assert_eq!(
        c.host_chat.send(c.host_session.0, "مرحبا"),
        Err(ChatRejected::NoSession)
    );
    c.host.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoking_chat_blocks_both_sides_live() {
    let mut c = connect(ChatLimits::default()).await;
    let key = c.session.feature_key();
    assert_eq!(host_chat_event(&mut c).await, ChatEvent::Availability(true));
    assert_eq!(
        viewer_chat_event(&mut c).await,
        ChatEvent::Availability(true)
    );

    assert!(c.host.set_permissions(c.host_session, DEFAULT_PERMISSIONS));
    assert_eq!(
        host_chat_event(&mut c).await,
        ChatEvent::Availability(false)
    );
    assert_eq!(
        viewer_chat_event(&mut c).await,
        ChatEvent::Availability(false)
    );
    assert!(!c.session.permissions().contains(Permissions::CHAT));

    // Neither side may send through the feature...
    assert_eq!(
        c.viewer_chat.send(key, "هل ما زلت هنا؟"),
        Err(ChatRejected::NotAllowed)
    );
    assert_eq!(
        c.host_chat.send(c.host_session.0, "لا"),
        Err(ChatRejected::NotAllowed)
    );
    // ...and a viewer that bypasses its own check is stopped by the host.
    assert!(c.session.send_message(raw_chat(500, "رسالة ممنوعة")));
    sync_host(&mut c).await;

    let all = DEFAULT_PERMISSIONS | Permissions::CHAT;
    assert!(c.host.set_permissions(c.host_session, all));
    assert_eq!(host_chat_event(&mut c).await, ChatEvent::Availability(true));
    assert_eq!(
        viewer_chat_event(&mut c).await,
        ChatEvent::Availability(true)
    );
    c.viewer_chat.send(key, "عدت").unwrap();
    // The blocked message never shows up: the first one received is the new one.
    assert_eq!(host_received(&mut c).await, "عدت");
    c.host_chat.send(c.host_session.0, "أهلاً").unwrap();
    assert_eq!(viewer_received(&mut c).await, "أهلاً");
    assert!(c
        .host_chat
        .history(c.host_session.0)
        .iter()
        .all(|e| e.text != "رسالة ممنوعة"));
    c.host.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn length_cap_and_duplicates_are_enforced() {
    let mut c = connect(ChatLimits::default()).await;
    let key = c.session.feature_key();
    let max = c.viewer_chat.limits().max_chars;
    let long = "ش".repeat(max + 1);
    assert_eq!(
        c.viewer_chat.send(key, &long),
        Err(ChatRejected::TooLong {
            chars: max + 1,
            max
        })
    );
    // Exactly at the cap goes through.
    let at_cap = format!("{}x", "ش".repeat(max - 1));
    c.viewer_chat.send(key, &at_cap).unwrap();
    assert_eq!(host_received(&mut c).await, at_cap);

    // A peer that skips the check, repeats an id or sends blanks is filtered by the host.
    assert!(c.session.send_message(raw_chat(900, &long)));
    assert!(c.session.send_message(raw_chat(901, " \n ")));
    assert!(c.session.send_message(raw_chat(902, "أول")));
    assert!(c.session.send_message(raw_chat(902, "مكرر")));
    assert!(c.session.send_message(raw_chat(903, "ثاني")));
    assert_eq!(host_received(&mut c).await, "أول");
    assert_eq!(host_received(&mut c).await, "ثاني");
    c.host.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peer_flood_is_rate_limited() {
    let burst = 5;
    let mut c = connect(ChatLimits {
        peer_burst: burst,
        // Nothing refills during the test.
        refill: Duration::from_secs(3600),
        ..ChatLimits::default()
    })
    .await;
    for id in 0..40u64 {
        assert!(c.session.send_message(raw_chat(id, &format!("رسالة {id}"))));
    }
    sync_host(&mut c).await;
    for id in 0..u64::from(burst) {
        assert_eq!(host_received(&mut c).await, format!("رسالة {id}"));
    }
    // The rest was dropped: give the chat task time to process the queue, then check.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let history = c.host_chat.history(c.host_session.0);
    assert_eq!(history.len(), burst as usize, "{history:?}");
    assert!(
        c.session.end_reason().is_none(),
        "flood must not end the session"
    );

    // The local user's own rate limit (default burst) applies too.
    let key = c.session.feature_key();
    let mut sent = 0u32;
    let limited = loop {
        match c.viewer_chat.send(key, "ب") {
            Ok(_) => sent += 1,
            Err(e) => break e,
        }
        assert!(sent <= 100, "viewer was never rate limited");
    };
    assert!(
        matches!(limited, ChatRejected::RateLimited { .. }),
        "{limited:?}"
    );
    assert_eq!(sent, c.viewer_chat.limits().burst);
    c.host.shutdown();
}
