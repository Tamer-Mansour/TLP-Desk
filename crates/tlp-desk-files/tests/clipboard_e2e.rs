//! End-to-end clipboard sync over a real loopback QUIC session, with in-memory clipboards on
//! both sides (these tests never touch the user's real clipboard).

use std::sync::Arc;
use std::time::Duration;

use tlp_desk_crypto::Identity;
use tlp_desk_files::{
    ClipContent, ClipImage, ClipKind, ClipboardBackend, ClipboardConfig, ClipboardEvent,
    ClipboardFeature, Direction, MemoryClipboard, INLINE_MAX,
};
use tlp_desk_net::{
    AuthMethod, Endpoint, EndpointConfig, FeatureDispatch, FeatureEvent, FeatureSet, Session,
    StreamKind, STREAM_REFUSED,
};
use tlp_desk_proto::{encode_frame, ClipboardData, Message, Permissions, Role};
use tokio::sync::mpsc;
use tokio::time::timeout;

const T: Duration = Duration::from_secs(10);
const QUIET: Duration = Duration::from_millis(400);

/// Arabic with harakat, RLM/LRM, ZWJ/ZWNJ, tatweel, CRLF and LF, emoji (ZWJ family + flag).
const ARABIC: &str = "مَرْحَبًا بِالْعَالَم\u{200F}!\u{200E} abc\r\nسطر ثانٍ\nلا\u{200C}مي\u{200D}ـه 👨\u{200D}👩\u{200D}👧 🇸🇦 ١٢٣";

fn cfg() -> ClipboardConfig {
    ClipboardConfig {
        debounce: Duration::from_millis(40),
        ..ClipboardConfig::default()
    }
}

fn ep_cfg(listen: bool) -> EndpointConfig {
    EndpointConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        device_name: if listen { "host-pc" } else { "viewer-pc" }.into(),
        listen,
        ..EndpointConfig::default()
    }
}

/// One side of a session with its own clipboard and feature.
struct Side {
    session: Arc<Session>,
    clip: Arc<MemoryClipboard>,
    feature: Arc<ClipboardFeature>,
    dispatch: Arc<FeatureDispatch>,
    events: mpsc::UnboundedReceiver<FeatureEvent>,
    key: u64,
    _endpoint: Option<Arc<Endpoint>>,
}

impl Side {
    fn start(session: Session, clip: MemoryClipboard, cfg: ClipboardConfig, key: u64) -> Self {
        let session = Arc::new(session);
        let clip = Arc::new(clip);
        let feature = ClipboardFeature::new(clip.clone() as Arc<dyn ClipboardBackend>, cfg);
        let (tx, events) = mpsc::unbounded_channel();
        let sink: tlp_desk_net::FeatureEventSink = Arc::new(move |e| {
            let _ = tx.send(e);
        });
        let set = FeatureSet::new().with(feature.clone());
        let perms = session.initial_permissions();
        let dispatch = Arc::new(FeatureDispatch::start(
            &set,
            session.clone(),
            key,
            perms,
            sink,
        ));
        // The control loop the host/viewer crates run: route what the core does not handle.
        let (s, d) = (session.clone(), dispatch.clone());
        tokio::spawn(async move {
            while let Ok(m) = s.recv().await {
                let _ = d.dispatch(m);
            }
        });
        Self {
            session,
            clip,
            feature,
            dispatch,
            events,
            key,
            _endpoint: None,
        }
    }

    fn role(&self) -> Role {
        self.session.role()
    }

    async fn next_event(&mut self) -> ClipboardEvent {
        let e = timeout(T, self.events.recv())
            .await
            .expect("event in time")
            .expect("event stream open");
        assert_eq!(e.feature, "clipboard");
        assert_eq!(e.session_key, self.key);
        e.payload::<ClipboardEvent>()
            .expect("clipboard event")
            .clone()
    }

    /// Waits until the clipboard holds `want`.
    async fn wait_for(&self, want: &ClipContent) {
        timeout(T, async {
            while self.clip.content().as_ref() != Some(want) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("clipboard content in time");
    }
}

/// Connects a host and a viewer granting `perms`; returns (host, viewer) with clipboards
/// (`viewer_clip` may already hold content for the push-on-start test).
async fn pair(perms: Permissions, viewer_clip: MemoryClipboard) -> (Side, Side) {
    let host_ep =
        Arc::new(Endpoint::bind(Arc::new(Identity::generate().unwrap()), ep_cfg(true)).unwrap());
    let viewer_ep =
        Arc::new(Endpoint::bind(Arc::new(Identity::generate().unwrap()), ep_cfg(false)).unwrap());
    let addr = host_ep.local_addr().unwrap();
    let h = {
        let host_ep = host_ep.clone();
        tokio::spawn(async move { host_ep.accept().await.unwrap().accept(perms).await.unwrap() })
    };
    let v = timeout(
        T,
        viewer_ep.connect(addr, Some(host_ep.device_id()), AuthMethod::AcceptPrompt),
    )
    .await
    .unwrap()
    .unwrap();
    let h = timeout(T, h).await.unwrap().unwrap();
    // The endpoints live as long as the sides.
    let mut host = Side::start(h, MemoryClipboard::new(), cfg(), 1);
    let mut viewer = Side::start(v, viewer_clip, cfg(), 1);
    host._endpoint = Some(host_ep);
    viewer._endpoint = Some(viewer_ep);
    (host, viewer)
}

fn all() -> Permissions {
    Permissions::VIEW | Permissions::CONTROL | Permissions::CLIPBOARD
}

fn noise(w: u32, h: u32) -> ClipImage {
    // Deterministic xorshift noise: compresses badly, so the PNG exceeds INLINE_MAX.
    let mut x: u32 = 0x1234_5678;
    let rgba = (0..w * h * 4)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 24) as u8
        })
        .collect();
    ClipImage {
        width: w,
        height: h,
        rgba,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn arabic_text_reaches_the_host_byte_exact_and_is_not_echoed() {
    let (mut host, mut viewer) = pair(all(), MemoryClipboard::new()).await;
    let content = ClipContent::Text(ARABIC.into());
    viewer.clip.copy(content.clone());
    host.wait_for(&content).await;
    let Some(ClipContent::Text(got)) = host.clip.content() else {
        panic!("text expected")
    };
    assert_eq!(
        got.as_bytes(),
        ARABIC.as_bytes(),
        "every code point preserved"
    );
    let bytes = ARABIC.len() as u64;
    assert_eq!(
        viewer.next_event().await,
        ClipboardEvent::Synced {
            direction: Direction::Sent,
            kind: ClipKind::Text,
            bytes
        }
    );
    assert_eq!(
        host.next_event().await,
        ClipboardEvent::Synced {
            direction: Direction::Received,
            kind: ClipKind::Text,
            bytes
        }
    );
    // No echo: the host's write must not come back to the viewer.
    tokio::time::sleep(QUIET).await;
    assert_eq!(viewer.clip.write_count(), 0, "echoed back to the viewer");
    assert_eq!(host.clip.write_count(), 1);
    assert!(viewer.events.try_recv().is_err() && host.events.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn html_flows_from_host_to_viewer() {
    let (host, viewer) = pair(all(), MemoryClipboard::new()).await;
    let html = ClipContent::Html {
        html: "<p dir=\"rtl\">مرحبا <b>بالعالم</b>\u{200F}</p>".into(),
        alt_text: "مرحبا بالعالم\u{200F}\r\n".into(),
    };
    host.clip.copy(html.clone());
    viewer.wait_for(&html).await;
    tokio::time::sleep(QUIET).await;
    assert_eq!(host.clip.write_count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn large_image_goes_on_a_side_stream_and_round_trips() {
    let (host, mut viewer) = pair(all(), MemoryClipboard::new()).await;
    let img = noise(800, 600); // 1.9 MB RGBA, PNG > 1 MiB
    let raw_len = {
        let msg = Message::ClipboardData(ClipboardData::Image {
            width: img.width,
            height: img.height,
            rgba: img.rgba.clone(),
        });
        encode_frame(&msg).unwrap().len()
    };
    // Noise barely compresses, so the PNG is about as large as the raw frame.
    assert!(raw_len > INLINE_MAX);
    let content = ClipContent::Image(img);
    host.clip.copy(content.clone());
    viewer.wait_for(&content).await;
    assert!(matches!(
        viewer.next_event().await,
        ClipboardEvent::Synced {
            direction: Direction::Received,
            kind: ClipKind::Image,
            ..
        }
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn rapid_changes_are_debounced_to_the_last_one() {
    let (host, viewer) = pair(all(), MemoryClipboard::new()).await;
    for i in 0..30 {
        viewer.clip.copy(ClipContent::Text(format!("نسخة رقم {i}")));
    }
    let last = ClipContent::Text("نسخة رقم 29".into());
    host.wait_for(&last).await;
    tokio::time::sleep(QUIET).await;
    assert!(
        host.clip.write_count() <= 2,
        "{} writes for one burst",
        host.clip.write_count()
    );
    assert_eq!(host.clip.content(), Some(last));
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_clipboard_stops_sync_both_ways_live() {
    let (host, viewer) = pair(all(), MemoryClipboard::new()).await;
    let first = ClipContent::Text("قبل السحب".into());
    viewer.clip.copy(first.clone());
    host.wait_for(&first).await;

    // The host user revokes CLIPBOARD; both sides learn it (the viewer via `Permissions`).
    let revoked = Permissions::VIEW | Permissions::CONTROL;
    host.dispatch.set_permissions(revoked);
    viewer.dispatch.set_permissions(revoked);

    viewer.clip.copy(ClipContent::Text("بعد السحب".into()));
    host.clip.copy(ClipContent::Text("من المضيف".into()));
    tokio::time::sleep(QUIET).await;
    assert_eq!(
        host.clip.content(),
        Some(ClipContent::Text("من المضيف".into()))
    );
    assert_eq!(host.clip.write_count(), 1, "nothing new reached the host");
    assert_eq!(viewer.clip.write_count(), 0, "nothing reached the viewer");

    // A hostile viewer sending anyway: the control message is dropped by the feature...
    viewer
        .session
        .send(&Message::ClipboardData(ClipboardData::Text("تجاوز".into())))
        .await
        .unwrap();
    // ...and its side stream is refused.
    let frame = encode_frame(&Message::ClipboardData(ClipboardData::Text("x".into()))).unwrap();
    let mut s = viewer
        .session
        .open_uni(StreamKind::Clipboard, 99)
        .await
        .unwrap();
    s.write_all(&frame).await.unwrap();
    // Not finished: the host must answer with STOP_SENDING, not just drain it.
    let stopped = timeout(T, s.stopped()).await.unwrap();
    assert_eq!(stopped.ok().flatten(), Some(STREAM_REFUSED.into()));
    tokio::time::sleep(QUIET).await;
    assert_eq!(host.clip.write_count(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_toggle_stops_and_resumes_with_a_push() {
    let (host, viewer) = pair(all(), MemoryClipboard::new()).await;
    // Let the (empty) push-on-start finish first.
    tokio::time::sleep(QUIET).await;
    assert_eq!(viewer.role(), Role::Viewer);
    assert_eq!(
        viewer.feature.sync_enabled(Role::Viewer, viewer.key),
        Some(true)
    );
    assert!(viewer
        .feature
        .set_sync_enabled(Role::Viewer, viewer.key, false));
    assert!(!viewer.feature.set_sync_enabled(Role::Host, 777, false));
    let off = ClipContent::Text("أثناء الإيقاف".into());
    viewer.clip.copy(off.clone());
    host.clip.copy(ClipContent::Text("لن يصل".into()));
    tokio::time::sleep(QUIET).await;
    assert_eq!(host.clip.write_count(), 0);
    assert_eq!(viewer.clip.write_count(), 0, "incoming dropped while off");
    // Switching back on pushes the viewer's current clipboard.
    viewer
        .feature
        .set_sync_enabled(Role::Viewer, viewer.key, true);
    host.wait_for(&off).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn viewer_pushes_its_clipboard_when_the_session_starts() {
    let pre = ClipContent::Text("نسخ قبل الاتصال".into());
    let (host, _viewer) = pair(all(), MemoryClipboard::with(pre.clone())).await;
    host.wait_for(&pre).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_text_is_refused_with_an_event() {
    let (host, mut viewer) = pair(all(), MemoryClipboard::new()).await;
    // 18 MiB of UTF-8: over the 16 MiB text cap.
    let big = "ض".repeat(9 << 20);
    viewer.clip.copy(ClipContent::Text(big));
    assert_eq!(
        viewer.next_event().await,
        ClipboardEvent::Refused {
            direction: Direction::Sent,
            kind: ClipKind::Text,
            reason: tlp_desk_files::RefusedReason::TooLarge
        }
    );
    tokio::time::sleep(QUIET).await;
    assert_eq!(host.clip.write_count(), 0);
}
