use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use tlp_desk_input::whiteboard::{Rgba, Stroke};
use tlp_desk_input::ScreenLine;

use super::super::agent::{serve_connection, AgentPlatform};
use super::super::agent_features::FeatureHost;
use super::super::desktop::DesktopSource;
use super::super::remote::AgentConnector;
use super::super::testutil::duplex;
use super::super::testutil::features::{wait_until, FakeFeatures};
use super::*;

struct NoDesktop;

impl DesktopSource for NoDesktop {
    fn input_desktop(&mut self) -> Result<String, HostError> {
        Ok("Default".into())
    }
    fn attach_thread(&mut self) -> Result<String, HostError> {
        Ok("Default".into())
    }
}

struct Platform(Arc<FeatureHost>);

impl AgentPlatform for Platform {
    fn session_id(&self) -> u32 {
        1
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
        _rect: MonitorRect,
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
        Some(&self.0)
    }
}

/// One fake agent process: every connection is served on its own thread.
struct Agent {
    fake: FakeFeatures,
    host: Arc<FeatureHost>,
    stop: Arc<AtomicBool>,
}

impl Agent {
    fn new() -> Arc<Self> {
        let fake = FakeFeatures::default();
        let host = FeatureHost::new(Arc::new(fake.clone()));
        Arc::new(Self {
            fake,
            host,
            stop: Arc::default(),
        })
    }
}

impl AgentConnector for Agent {
    fn connect(&self) -> Result<Box<dyn AgentConn>, HostError> {
        let (a, mut b) = duplex();
        let platform = Platform(self.host.clone());
        let stop = self.stop.clone();
        std::thread::spawn(move || {
            let _ = serve_connection(&mut b, &platform, &stop);
        });
        Ok(Box::new(a))
    }
}

/// What the service's events reader does: agent events into the hub.
fn pump_events(link: &Arc<AgentLink>, hub: &Arc<AgentFeatureHub>) {
    let (mut conn, _) = link.open(AgentChannel::Events).unwrap();
    let hub = hub.clone();
    std::thread::spawn(move || {
        while let Ok(ev) = ipc::read_msg::<_, AgentEvent>(&mut conn) {
            hub.on_event(ev);
        }
    });
}

fn setup() -> (Arc<AgentLink>, Arc<AgentFeatureHub>, Arc<Agent>) {
    let agent = Agent::new();
    let link = AgentLink::new();
    link.set(Some(agent.clone()));
    let hub = AgentFeatureHub::new(link.clone());
    pump_events(&link, &hub);
    (link, hub, agent)
}

fn stroke(id: u64, points: &[(u16, u16)]) -> Stroke {
    Stroke {
        id,
        points: points.to_vec(),
        color: Rgba(0xff00_00ff),
        width: 20,
    }
}

#[test]
fn clipboard_proxy_reads_writes_and_hears_changes() {
    let (_link, hub, agent) = setup();
    let mut changes = hub.clipboard_changes();
    assert_eq!(hub.clipboard_read().unwrap(), None);
    let image = ClipData::Image {
        width: 2,
        height: 2,
        rgba: (0..16).collect(),
    };
    let seq = hub.clipboard_write(&image).unwrap();
    assert_eq!(seq, 1);
    assert_eq!(hub.clipboard_read().unwrap(), Some(image));
    agent.fake.0.user_copies(ClipData::Text("نسخ ولصق".into()));
    wait_until("clipboard change", || {
        changes.has_changed().unwrap_or(false)
    });
    assert_eq!(*changes.borrow_and_update(), 2);
    assert_eq!(
        hub.clipboard_read().unwrap(),
        Some(ClipData::Text("نسخ ولصق".into()))
    );
    assert_eq!(hub.clipboard_files(), None);
}

#[test]
fn clipboard_without_an_agent_is_an_error_not_a_hang() {
    let link = AgentLink::new();
    let hub = AgentFeatureHub::new(link);
    assert!(matches!(hub.clipboard_read(), Err(HostError::NoAgent)));
    assert!(hub.clipboard_write(&ClipData::Text("x".into())).is_err());
    assert_eq!(hub.clipboard_files(), None);
}

#[test]
fn board_delta_sends_only_what_is_new_and_resets_on_removal() {
    let mut sent = Sent::default();
    let mut strokes = vec![stroke(1, &[(0, 0), (1, 1)])];
    let (reset, reqs) = board_delta(&strokes, &mut sent);
    assert!(!reset);
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0][0].points, vec![(0, 0), (1, 1)]);
    // Points appended to stroke 1 and a new stroke 2.
    strokes[0].points.push((2, 2));
    strokes.push(stroke(2, &[(9, 9)]));
    let (reset, reqs) = board_delta(&strokes, &mut sent);
    assert!(!reset);
    let all: Vec<_> = reqs.concat();
    assert_eq!(all.len(), 2);
    assert_eq!((all[0].id, all[0].points.clone()), (1, vec![(2, 2)]));
    assert_eq!((all[1].id, all[1].points.clone()), (2, vec![(9, 9)]));
    // Nothing new: nothing sent.
    let (reset, reqs) = board_delta(&strokes, &mut sent);
    assert!(!reset && reqs.is_empty());
    // Stroke 1 erased: everything is sent again after a reset.
    strokes.remove(0);
    let (reset, reqs) = board_delta(&strokes, &mut sent);
    assert!(reset);
    assert_eq!(reqs.concat().len(), 1);
    // A big board is split into requests of bounded size.
    let many: Vec<Stroke> = (0..40)
        .map(|i| stroke(100 + i, &vec![(1, 1); 1000]))
        .collect();
    let (_, reqs) = board_delta(&many, &mut Sent::default());
    assert!(reqs.len() >= 5);
    for r in &reqs {
        let n: usize = r.iter().map(|s| s.points.len()).sum();
        assert!(n <= OVERLAY_POINTS_PER_REQUEST);
    }
}

#[test]
fn overlay_proxy_mirrors_the_board_into_the_agent() {
    let (link, hub, agent) = setup();
    let cleared = Arc::new(AtomicU32::new(0));
    let c2 = cleared.clone();
    let factory = hub.overlay_factory();
    let board = SharedBoard::default();
    let mut overlay = factory(
        board.clone(),
        Arc::new(move || {
            c2.fetch_add(1, Ordering::SeqCst);
        }),
    )
    .unwrap();
    overlay.set_target(MonitorRect::new(0, 0, 2560, 1440));
    lock_board(&board).add(&stroke(1, &[(0, 0), (100, 100)]));
    overlay.changed();
    let agent_board = || {
        let log = agent.fake.0.overlay();
        let b = lock_board(&lock(&log).board).strokes().to_vec();
        b
    };
    wait_until("stroke in the agent", || {
        !agent.fake.0.overlays.lock().unwrap().is_empty() && agent_board().len() == 1
    });
    assert_eq!(
        lock(&agent.fake.0.overlay()).target.map(|r| r.width),
        Some(2560)
    );
    // Streaming more points of the same stroke.
    lock_board(&board).add(&stroke(1, &[(200, 200)]));
    overlay.changed();
    wait_until("appended points", || {
        agent_board().first().map(|s| s.points.len()) == Some(3)
    });
    // The host user clicks the clear badge in the agent: the session hears it.
    let on_clear = lock(&agent.fake.0.overlay()).on_clear.clone().unwrap();
    on_clear();
    wait_until("clear request", || cleared.load(Ordering::SeqCst) == 1);
    // The session clears its board.
    lock_board(&board).clear();
    overlay.changed();
    wait_until("cleared in the agent", || agent_board().is_empty());

    // The agent restarts (user switch): the overlay is re-created with its strokes.
    lock_board(&board).add(&stroke(5, &[(1, 2), (3, 4)]));
    let agent2 = Agent::new();
    link.set(Some(agent2.clone()));
    hub.agent_changed();
    overlay.changed();
    wait_until("overlay re-created in the new agent", || {
        let o = agent2.fake.0.overlays.lock().unwrap().last().cloned();
        o.is_some_and(|o| {
            let l = lock(&o);
            l.target.is_some() && lock_board(&l.board).strokes().len() == 1
        })
    });
    drop(overlay);
    wait_until("overlay closed", || lock(&agent2.fake.0.overlay()).dropped);
}

#[test]
fn marker_proxy_shows_hides_relabels_and_survives_a_restart() {
    let (link, hub, agent) = setup();
    let factory = hub.pointer_marker_factory();
    let mut marker = factory("حاسوب سارة").unwrap();
    marker.set_visible(true);
    marker.set_label("سارة، أحمد");
    wait_until("marker shown", || {
        agent
            .fake
            .0
            .markers
            .lock()
            .unwrap()
            .last()
            .is_some_and(|m| {
                let m = lock(m);
                m.visible && m.label == "سارة، أحمد"
            })
    });
    marker.set_visible(false);
    wait_until("marker hidden", || !lock(&agent.fake.0.marker()).visible);
    // New agent: the next change re-creates the marker there, visible again.
    let agent2 = Agent::new();
    link.set(Some(agent2.clone()));
    marker.set_visible(true);
    wait_until("marker in the new agent", || {
        agent2
            .fake
            .0
            .markers
            .lock()
            .unwrap()
            .last()
            .is_some_and(|m| {
                let m = lock(m);
                m.visible && m.label == "سارة، أحمد"
            })
    });
    drop(marker);
    wait_until("marker closed", || lock(&agent2.fake.0.marker()).dropped);
}

#[test]
fn local_activity_runs_while_a_handle_lives_and_reports_input() {
    let (_link, hub, agent) = setup();
    let factory = hub.local_activity_factory();
    let a = factory().unwrap();
    let b = factory().unwrap();
    wait_until("hooks started", || {
        agent.fake.0.activity_starts.load(Ordering::SeqCst) == 1
    });
    assert!(a.last_physical_input().is_none());
    let stamp = agent.fake.0.activity.lock().unwrap().clone().unwrap();
    stamp.record_keyboard(false, Instant::now() + Duration::from_millis(1));
    wait_until("keyboard reported", || {
        a.last_physical_keyboard().is_some() && b.last_physical_keyboard().is_some()
    });
    assert!(a.last_physical_pointer().is_none());
    assert!(a.is_local_active(Duration::from_secs(5)));
    drop(a);
    assert_eq!(lock(&hub.state).activity_users, 1);
    // The last drop stops the hooks; a new handle starts them again.
    drop(b);
    assert_eq!(lock(&hub.state).activity_users, 0);
    let _c = factory().unwrap();
    wait_until("hooks restarted", || {
        agent.fake.0.activity_starts.load(Ordering::SeqCst) == 2
    });
}

#[test]
fn shield_proxy_engages_in_the_agent_and_relays_signals() {
    let (_link, hub, agent) = setup();
    let signals = Arc::new(Mutex::new(Vec::new()));
    let s2 = signals.clone();
    let factory = hub.shield_factory();
    let mut shield = factory(Arc::new(move |s| s2.lock().unwrap().push(s))).unwrap();
    let text = PrivacyScreenText {
        lines: vec![ScreenLine::title("هذا الجهاز قيد الاستخدام عن بُعد")],
    };
    shield.engage(&text).unwrap();
    assert!(shield.is_engaged());
    assert!(lock(&agent.fake.0.shield()).engaged);
    assert_eq!(lock(&agent.fake.0.shield()).text, text);
    // The host user holds the emergency chord in the agent.
    let on_signal = lock(&agent.fake.0.shield()).on_signal.clone().unwrap();
    on_signal(ShieldSignal::EmergencyExit);
    wait_until("signal relayed", || !signals.lock().unwrap().is_empty());
    assert_eq!(*signals.lock().unwrap(), vec![ShieldSignal::EmergencyExit]);
    shield.release();
    assert!(!shield.is_engaged());
    assert!(!lock(&agent.fake.0.shield()).engaged);

    // Engaged when the agent goes away: privacy mode is told the shield failed.
    shield.engage(&text).unwrap();
    hub.agent_changed();
    assert_eq!(signals.lock().unwrap().last(), Some(&ShieldSignal::Failed));
}

#[test]
fn shield_without_an_agent_fails_to_engage() {
    let hub = AgentFeatureHub::new(AgentLink::new());
    let mut shield = hub.shield_factory()(Arc::new(|_| {})).unwrap();
    assert!(shield.engage(&PrivacyScreenText::default()).is_err());
    assert!(!shield.is_engaged());
}

#[test]
fn shell_offer_round_trip_drives_the_stage_handler() {
    let (_link, hub, agent) = setup();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let offer = hub.next_id();
    let dir = std::env::temp_dir().join("tlp-desk-paste-اختبار");
    let items = vec![TransferItem {
        rel_path: "ملف.txt".into(),
        size: 3,
        modified_unix_ms: 0,
        is_dir: false,
    }];
    hub.shell_offer(
        offer,
        dir.to_string_lossy().into_owned(),
        items,
        Arc::new(move |begin| s2.lock().unwrap().push(begin)),
    );
    wait_until("offer on the clipboard", || {
        !agent.fake.0.offers.lock().unwrap().is_empty()
    });
    let stage = agent.fake.0.offers.lock().unwrap()[0].3.clone();
    stage.begin();
    wait_until("begin relayed", || *seen.lock().unwrap() == vec![true]);
    hub.shell_stage(offer, WireStage::Finished);
    wait_until("state pushed", || stage.state() == WireStage::Finished);
    stage.cancel();
    wait_until("cancel relayed", || {
        *seen.lock().unwrap() == vec![true, false]
    });
    hub.shell_clear(offer);
    wait_until("offer cleared", || {
        *agent.fake.0.cleared.lock().unwrap() == vec![offer]
    });
}

#[test]
fn audio_proxy_reads_samples_and_skips_keepalives() {
    let (_link, hub, agent) = setup();
    agent
        .fake
        .0
        .audio
        .lock()
        .unwrap()
        .push((44_100, 1, vec![0.25, -0.5]));
    let mut audio = hub.open_audio().unwrap();
    let first = audio.next_packet().unwrap();
    assert_eq!(first, Some((44_100, 1, vec![0.25, -0.5])));
    // Silence: keep-alives.
    assert_eq!(audio.next_packet().unwrap(), None);
    drop(audio);
    wait_until("capture stopped", || {
        agent.fake.0.audio_closed.load(Ordering::SeqCst) == 1
    });
}
