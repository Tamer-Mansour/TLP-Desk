use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tlp_desk_input::whiteboard::lock_board;
use tlp_desk_input::ShieldSignal;
use tlp_desk_proto::TransferItem;

use super::super::agent::{serve_connection, AgentPlatform};
use super::super::desktop::DesktopSource;
use super::super::ipc::{
    self, AgentChannel, AgentEvent, AgentHello, AgentWelcome, AudioPacket, ClipData, FeatureReply,
    FeatureRequest, WireClip, WireError, WireRect, WireScreenLine, WireStage, WireStroke,
    IPC_VERSION,
};
use super::super::testutil::duplex;
use super::super::testutil::features::{lock, wait_until, FakeFeatures};
use super::*;

/// An agent platform with only user-session features (no capture or input needed here).
struct Platform {
    features: Option<Arc<FeatureHost>>,
}

struct NoDesktop;

impl DesktopSource for NoDesktop {
    fn input_desktop(&mut self) -> Result<String, HostError> {
        Ok("Default".into())
    }
    fn attach_thread(&mut self) -> Result<String, HostError> {
        Ok("Default".into())
    }
}

impl AgentPlatform for Platform {
    fn session_id(&self) -> u32 {
        2
    }
    fn open_capturer(
        &self,
        _monitor: u32,
        _max_size: Option<(u32, u32)>,
        _secure_desktop: bool,
    ) -> Result<Box<dyn tlp_desk_capture::ScreenCapturer>, tlp_desk_capture::CaptureError> {
        Err(tlp_desk_capture::CaptureError::Unsupported)
    }
    fn list_monitors(
        &self,
    ) -> Result<Vec<tlp_desk_capture::MonitorInfo>, tlp_desk_capture::CaptureError> {
        Ok(Vec::new())
    }
    fn open_injector(
        &self,
        _rect: tlp_desk_input::MonitorRect,
    ) -> Result<Box<dyn tlp_desk_input::InputInjector>, InputError> {
        Err(InputError::Unsupported)
    }
    fn lock_workstation(&self) -> Result<(), HostError> {
        Ok(())
    }
    fn desktop_source(&self) -> Box<dyn DesktopSource> {
        Box::new(NoDesktop)
    }
    fn feature_host(&self) -> Option<&Arc<FeatureHost>> {
        self.features.as_ref()
    }
}

/// A connection on `channel` to an agent thread serving `platform`.
fn open(
    platform: Arc<Platform>,
    channel: AgentChannel,
) -> (super::super::testutil::PipeEnd, AgentWelcome) {
    let (mut a, mut b) = duplex();
    std::thread::spawn(move || {
        let stop = AtomicBool::new(false);
        let _ = serve_connection(&mut b, &*platform, &stop);
    });
    ipc::write_msg(
        &mut a,
        &AgentHello {
            version: IPC_VERSION,
            channel,
        },
    )
    .unwrap();
    let welcome = ipc::read_msg(&mut a).unwrap();
    (a, welcome)
}

fn setup() -> (FakeFeatures, Arc<FeatureHost>, Arc<Platform>) {
    let fake = FakeFeatures::default();
    let host = FeatureHost::new(Arc::new(fake.clone()));
    let platform = Arc::new(Platform {
        features: Some(host.clone()),
    });
    (fake, host, platform)
}

fn ask(conn: &mut super::super::testutil::PipeEnd, req: &FeatureRequest) -> FeatureReply {
    ipc::write_msg(conn, req).unwrap();
    ipc::read_msg(conn).unwrap()
}

fn next_event(rx: &std::sync::mpsc::Receiver<AgentEvent>) -> AgentEvent {
    rx.recv_timeout(Duration::from_secs(10)).expect("an event")
}

#[test]
fn clipboard_crosses_the_feature_channel_byte_exact() {
    let (fake, host, platform) = setup();
    let events = host.subscribe();
    let (mut c, welcome) = open(platform, AgentChannel::Feature);
    assert!(matches!(welcome, AgentWelcome::Ok { session_id: 2, .. }));

    // Empty clipboard.
    assert_eq!(
        ask(&mut c, &FeatureRequest::ClipboardRead),
        FeatureReply::Clip(None)
    );

    // The service writes Arabic text (RLM, ZWJ, CRLF kept).
    let text = ClipData::Text("مرحبا\u{200f}\r\nبالعالم\u{200d}ـ".into());
    ipc::write_msg_with_payload(
        &mut c,
        &FeatureRequest::ClipboardWrite(text.header()),
        &text.payload(),
    )
    .unwrap();
    assert_eq!(
        ipc::read_msg::<_, FeatureReply>(&mut c).unwrap(),
        FeatureReply::ClipboardSeq(1)
    );
    assert_eq!(*lock(&fake.0.clip), Some(text.clone()));

    // The local user copies HTML: the service hears the change and reads it.
    let html = ClipData::Html {
        html: "<p dir=\"rtl\">نص <b>غامق</b></p>".into(),
        alt_text: "نص غامق".into(),
    };
    fake.0.user_copies(html.clone());
    assert!(matches!(
        next_event(&events),
        AgentEvent::ClipboardChanged { .. }
    ));
    ipc::write_msg(&mut c, &FeatureRequest::ClipboardRead).unwrap();
    let FeatureReply::Clip(Some(h)) = ipc::read_msg(&mut c).unwrap() else {
        panic!("expected content");
    };
    assert_eq!(ipc::read_clip_payload(&mut c, h).unwrap(), html);

    // Copied files in Explorer (Arabic path).
    *lock(&fake.0.files) = Some(vec![r"C:\Users\تامر\المستندات\تقرير.docx".into()]);
    assert_eq!(
        ask(&mut c, &FeatureRequest::ClipboardFiles),
        FeatureReply::Files(Some(vec![r"C:\Users\تامر\المستندات\تقرير.docx".into()]))
    );
}

#[test]
fn malformed_clipboard_write_is_refused_and_the_connection_survives() {
    let (fake, _host, platform) = setup();
    let (mut c, _) = open(platform, AgentChannel::Feature);
    // Announces 2 bytes of text but sends invalid UTF-8.
    ipc::write_msg_with_payload(
        &mut c,
        &FeatureRequest::ClipboardWrite(WireClip::Text { len: 2 }),
        &[0xff, 0xfe],
    )
    .unwrap();
    assert!(matches!(
        ipc::read_msg::<_, FeatureReply>(&mut c).unwrap(),
        FeatureReply::Error(WireError::Other(_))
    ));
    assert!(lock(&fake.0.clip).is_none());
    // Still in step.
    assert_eq!(
        ask(&mut c, &FeatureRequest::ClipboardRead),
        FeatureReply::Clip(None)
    );
}

#[test]
fn overlay_marker_and_shield_follow_the_service() {
    let (fake, host, platform) = setup();
    let events = host.subscribe();
    let (mut c, _) = open(platform, AgentChannel::Feature);

    // Whiteboard overlay.
    assert_eq!(
        ask(&mut c, &FeatureRequest::OverlayOpen { id: 7 }),
        FeatureReply::Ok
    );
    let rect = WireRect {
        x: -1920,
        y: 0,
        width: 1920,
        height: 1080,
    };
    assert_eq!(
        ask(&mut c, &FeatureRequest::OverlayTarget { id: 7, rect }),
        FeatureReply::Ok
    );
    let stroke = |id, points: Vec<(u16, u16)>| WireStroke {
        id,
        color: 0x00ff_00ff,
        width: 25,
        points,
    };
    ask(
        &mut c,
        &FeatureRequest::OverlayStrokes {
            id: 7,
            reset: false,
            strokes: vec![stroke(1, vec![(0, 0), (10, 10)])],
        },
    );
    ask(
        &mut c,
        &FeatureRequest::OverlayStrokes {
            id: 7,
            reset: false,
            strokes: vec![stroke(1, vec![(20, 20)]), stroke(2, vec![(5, 5)])],
        },
    );
    let overlay = fake.0.overlay();
    {
        let log = lock(&overlay);
        assert_eq!(log.target.map(|r| (r.x, r.width)), Some((-1920, 1920)));
        assert_eq!(log.changed, 2);
        let b = lock_board(&log.board);
        assert_eq!(b.strokes().len(), 2);
        assert_eq!(b.strokes()[0].points, vec![(0, 0), (10, 10), (20, 20)]);
    }
    // A reset replaces everything.
    ask(
        &mut c,
        &FeatureRequest::OverlayStrokes {
            id: 7,
            reset: true,
            strokes: vec![stroke(3, vec![(1, 1)])],
        },
    );
    assert_eq!(lock_board(&lock(&overlay).board).strokes().len(), 1);
    // The host user clicks the clear badge.
    let on_clear = lock(&overlay).on_clear.clone().unwrap();
    on_clear();
    assert_eq!(next_event(&events), AgentEvent::OverlayClear { id: 7 });
    ask(&mut c, &FeatureRequest::OverlayClose { id: 7 });
    assert!(lock(&overlay).dropped);
    // Unknown ids are ignored.
    assert_eq!(
        ask(&mut c, &FeatureRequest::OverlayClose { id: 99 }),
        FeatureReply::Ok
    );

    // Pointer marker with an Arabic label.
    ask(
        &mut c,
        &FeatureRequest::MarkerOpen {
            id: 8,
            label: "حاسوب سارة".into(),
        },
    );
    ask(
        &mut c,
        &FeatureRequest::MarkerVisible {
            id: 8,
            visible: true,
        },
    );
    ask(
        &mut c,
        &FeatureRequest::MarkerLabel {
            id: 8,
            label: "سارة، Laptop-١".into(),
        },
    );
    let marker = fake.0.marker();
    assert!(lock(&marker).visible);
    assert_eq!(lock(&marker).label, "سارة، Laptop-١");
    ask(&mut c, &FeatureRequest::MarkerClose { id: 8 });
    assert!(lock(&marker).dropped);

    // Privacy shield: text is sanitized (bidi overrides removed), signals become events.
    let lines = vec![WireScreenLine {
        text: "\u{202e}هذا الجهاز قيد الاستخدام".into(),
        large: true,
    }];
    assert_eq!(
        ask(&mut c, &FeatureRequest::ShieldEngage { id: 9, lines }),
        FeatureReply::Ok
    );
    let shield = fake.0.shield();
    assert!(lock(&shield).engaged);
    assert_eq!(lock(&shield).text.lines[0].text, "هذا الجهاز قيد الاستخدام");
    let signal = lock(&shield).on_signal.clone().unwrap();
    signal(ShieldSignal::EmergencyExit);
    assert_eq!(
        next_event(&events),
        AgentEvent::Shield {
            id: 9,
            signal: super::super::ipc::WireShieldSignal::EmergencyExit
        }
    );
    ask(&mut c, &FeatureRequest::ShieldRelease { id: 9 });
    assert!(!lock(&shield).engaged);
    assert_eq!(lock(&shield).releases, 1);
}

#[test]
fn local_input_is_reported_while_the_hooks_run() {
    let (fake, host, platform) = setup();
    let events = host.subscribe();
    let (mut c, _) = open(platform, AgentChannel::Feature);
    assert_eq!(
        ask(&mut c, &FeatureRequest::ActivityStart),
        FeatureReply::Ok
    );
    // Idempotent.
    assert_eq!(
        ask(&mut c, &FeatureRequest::ActivityStart),
        FeatureReply::Ok
    );
    assert_eq!(fake.0.activity_starts.load(Ordering::SeqCst), 1);
    let stamp = lock(&fake.0.activity).clone().unwrap();
    stamp.record_keyboard(false, Instant::now() + Duration::from_millis(1));
    assert_eq!(
        next_event(&events),
        AgentEvent::LocalInput {
            keyboard: true,
            pointer: false
        }
    );
    // Injected input never counts.
    stamp.record_pointer(true, Instant::now() + Duration::from_millis(2));
    stamp.record_pointer(false, Instant::now() + Duration::from_millis(3));
    assert_eq!(
        next_event(&events),
        AgentEvent::LocalInput {
            keyboard: false,
            pointer: true
        }
    );
    ask(&mut c, &FeatureRequest::ActivityStop);
    // After a stop, input is no longer reported.
    stamp.record_keyboard(false, Instant::now() + Duration::from_millis(4));
    assert!(events.recv_timeout(ACTIVITY_POLL * 5).is_err());
}

#[test]
fn virtual_files_offer_reports_explorer_reads() {
    let (fake, host, platform) = setup();
    let events = host.subscribe();
    let (mut c, _) = open(platform, AgentChannel::Feature);
    let items = vec![TransferItem {
        rel_path: "صور/عطلة ٢٠٢٦.jpg".into(),
        size: 42,
        modified_unix_ms: 0,
        is_dir: false,
    }];
    let dir = std::env::temp_dir().join("tlp-desk-test-لصق");
    // A relative staging folder is refused.
    assert!(matches!(
        ask(
            &mut c,
            &FeatureRequest::ShellOffer {
                offer: 1,
                dir: "relative".into(),
                items: items.clone()
            }
        ),
        FeatureReply::Error(_)
    ));
    assert_eq!(
        ask(
            &mut c,
            &FeatureRequest::ShellOffer {
                offer: 1,
                dir: dir.to_string_lossy().into_owned(),
                items: items.clone()
            }
        ),
        FeatureReply::Ok
    );
    let (offer, got_dir, got_items, stage) = lock(&fake.0.offers)[0].clone();
    assert_eq!((offer, got_dir, got_items), (1, dir, items));
    assert_eq!(stage.state(), WireStage::Idle);
    // Explorer pastes: one begin event however often it asks.
    stage.begin();
    stage.begin();
    assert_eq!(next_event(&events), AgentEvent::StageBegin { offer: 1 });
    ask(
        &mut c,
        &FeatureRequest::ShellStage {
            offer: 1,
            state: WireStage::Running,
        },
    );
    assert_eq!(stage.state(), WireStage::Running);
    stage.cancel();
    assert_eq!(next_event(&events), AgentEvent::StageCancel { offer: 1 });
    ask(&mut c, &FeatureRequest::ShellClear { offer: 1 });
    assert_eq!(*lock(&fake.0.cleared), vec![1]);
    // Clearing twice does not clear the user's clipboard again.
    ask(&mut c, &FeatureRequest::ShellClear { offer: 1 });
    assert_eq!(*lock(&fake.0.cleared), vec![1]);
}

#[test]
fn object_counts_are_capped() {
    let (_fake, _host, platform) = setup();
    let (mut c, _) = open(platform, AgentChannel::Feature);
    for id in 0..MAX_FEATURE_OBJECTS as u64 {
        assert_eq!(
            ask(&mut c, &FeatureRequest::OverlayOpen { id }),
            FeatureReply::Ok
        );
    }
    assert!(matches!(
        ask(&mut c, &FeatureRequest::OverlayOpen { id: 1000 }),
        FeatureReply::Error(_)
    ));
}

#[test]
fn audio_streams_samples_and_keepalives_until_closed() {
    let (fake, _host, platform) = setup();
    lock(&fake.0.audio).push((48_000, 2, vec![0.5, -0.25, 1.0, 0.0]));
    let (mut c, welcome) = open(platform, AgentChannel::Audio);
    assert!(matches!(welcome, AgentWelcome::Ok { .. }));
    let hdr: AudioPacket = ipc::read_msg(&mut c).unwrap();
    assert_eq!((hdr.rate, hdr.channels, hdr.payload_len), (48_000, 2, 16));
    let mut buf = Vec::new();
    ipc::read_payload_into(&mut c, hdr.payload_len, &mut buf).unwrap();
    assert_eq!(&buf[..4], &0.5f32.to_le_bytes());
    // Nothing plays: keep-alive.
    let hdr: AudioPacket = ipc::read_msg(&mut c).unwrap();
    assert_eq!(hdr.payload_len, 0);
    // The service hangs up: capture stops.
    drop(c);
    wait_until("audio capture closed", || {
        fake.0.audio_closed.load(Ordering::SeqCst) == 1
    });
}

#[test]
fn an_agent_without_features_refuses_the_new_channels() {
    let platform = Arc::new(Platform { features: None });
    for channel in [AgentChannel::Feature, AgentChannel::Audio] {
        let (_c, welcome) = open(platform.clone(), channel);
        assert!(matches!(
            welcome,
            AgentWelcome::Refused {
                error: WireError::Unsupported,
                ..
            }
        ));
    }
}

#[test]
fn feature_events_share_the_events_channel_with_desktop_switches() {
    let (fake, _host, platform) = setup();
    let (mut c, _) = open(platform, AgentChannel::Events);
    fake.0.user_copies(ClipData::Text("نسخ".into()));
    // The first event may be the initial desktop report; the clipboard change follows.
    let mut got = Vec::new();
    while got.len() < 4 {
        let ev: AgentEvent = ipc::read_msg(&mut c).unwrap();
        let done = matches!(ev, AgentEvent::ClipboardChanged { .. });
        got.push(ev);
        if done {
            break;
        }
    }
    assert!(matches!(
        got.last(),
        Some(AgentEvent::ClipboardChanged { seq: 1 })
    ));
}
