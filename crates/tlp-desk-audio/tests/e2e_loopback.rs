//! End to end over real QUIC on 127.0.0.1: a host and a viewer session with the audio
//! features, a fake capture device (synthetic tone) and a fake speaker that records what it
//! plays. No sound card is touched.

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tlp_desk_audio::device::fake::{FakeCapture, FakePlayback, FakeSignal};
use tlp_desk_audio::{AudioEvent, AudioHost, AudioViewer, ViewerAudioState, FEATURE_NAME};
use tlp_desk_crypto::Identity;
use tlp_desk_net::{
    AuthMethod, Endpoint, EndpointConfig, FeatureDispatch, FeatureEvent, FeatureEventSink,
    FeatureSet, Session, StreamKind,
};
use tlp_desk_proto::{Message, Permissions};
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout, Instant};

const T: Duration = Duration::from_secs(10);
const VIEWER_KEY: u64 = 7;

fn endpoint(listen: bool) -> Arc<Endpoint> {
    Arc::new(
        Endpoint::bind(
            Arc::new(Identity::generate().unwrap()),
            EndpointConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                listen,
                device_name: if listen { "host-pc" } else { "viewer-pc" }.into(),
                ..EndpointConfig::default()
            },
        )
        .unwrap(),
    )
}

/// The host side of one session, routed like `tlp-desk-host` does: permission check, then
/// the feature; live permission changes reach the features and the viewer.
struct HostSide {
    session: Arc<Session>,
    dispatch: Arc<FeatureDispatch>,
    perms: Arc<AtomicU16>,
}

impl HostSide {
    async fn set_permissions(&self, p: Permissions) {
        self.perms.store(p.0, Ordering::Release);
        self.dispatch.set_permissions(p);
        self.session.send(&Message::Permissions(p)).await.unwrap();
    }
}

struct Harness {
    host: HostSide,
    viewer_session: Arc<Session>,
    viewer_events: mpsc::UnboundedReceiver<FeatureEvent>,
    _viewer_dispatch: Option<Arc<FeatureDispatch>>,
    _endpoints: (Arc<Endpoint>, Arc<Endpoint>),
}

fn sink() -> (FeatureEventSink, mpsc::UnboundedReceiver<FeatureEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (
        Arc::new(move |e| {
            let _ = tx.send(e);
        }),
        rx,
    )
}

async fn harness(perms: Permissions, host: FeatureSet, viewer: FeatureSet) -> Harness {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_test_writer()
        .try_init();
    let host_ep = endpoint(true);
    let viewer_ep = endpoint(false);
    let addr = host_ep.local_addr().unwrap();
    let h = host_ep.clone();
    let accept = tokio::spawn(async move { h.accept().await.unwrap().accept(perms).await });
    let v = timeout(
        T,
        viewer_ep.connect(addr, Some(host_ep.device_id()), AuthMethod::AcceptPrompt),
    )
    .await
    .unwrap()
    .unwrap();
    let h = timeout(T, accept).await.unwrap().unwrap().unwrap();
    let (host_session, viewer_session) = (Arc::new(h), Arc::new(v));

    let (host_sink, _host_events) = sink();
    let host_dispatch = Arc::new(FeatureDispatch::start(
        &host,
        host_session.clone(),
        1,
        perms,
        host_sink,
    ));
    let current = Arc::new(AtomicU16::new(perms.0));
    {
        let (session, dispatch, current) =
            (host_session.clone(), host_dispatch.clone(), current.clone());
        tokio::spawn(async move {
            while let Ok(msg) = session.recv().await {
                let have = Permissions(current.load(Ordering::Acquire));
                if have.contains(msg.required_permissions()) {
                    let _ = dispatch.dispatch(msg);
                }
            }
        });
    }

    let (viewer_sink, viewer_events) = sink();
    let viewer_dispatch = if viewer.is_empty() {
        None
    } else {
        let d = Arc::new(FeatureDispatch::start(
            &viewer,
            viewer_session.clone(),
            VIEWER_KEY,
            perms,
            viewer_sink,
        ));
        let (session, dispatch) = (viewer_session.clone(), d.clone());
        tokio::spawn(async move {
            while let Ok(msg) = session.recv().await {
                match msg {
                    Message::Permissions(p) => dispatch.set_permissions(p),
                    other => {
                        let _ = dispatch.dispatch(other);
                    }
                }
            }
        });
        Some(d)
    };
    Harness {
        host: HostSide {
            session: host_session,
            dispatch: host_dispatch,
            perms: current,
        },
        viewer_session,
        viewer_events,
        _viewer_dispatch: viewer_dispatch,
        _endpoints: (host_ep, viewer_ep),
    }
}

async fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + T;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        sleep(Duration::from_millis(20)).await;
    }
}

fn tone() -> FakeSignal {
    FakeSignal::Tone {
        freq: 1000.0,
        amplitude: 0.5,
    }
}

/// Upward zero crossings per second on channel 0.
fn frequency(samples: &[f32], channels: usize, rate: f32) -> f32 {
    let ch0: Vec<f32> = samples.iter().step_by(channels).copied().collect();
    let n = ch0.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
    n as f32 * rate / ch0.len() as f32
}

fn last_state(events: &mut mpsc::UnboundedReceiver<FeatureEvent>) -> Option<ViewerAudioState> {
    let mut last = None;
    while let Ok(e) = events.try_recv() {
        assert_eq!(e.feature, FEATURE_NAME);
        assert_eq!(e.session_key, VIEWER_KEY);
        if let Some(AudioEvent::Viewer(s)) = e.payload::<AudioEvent>() {
            last = Some(*s);
        }
    }
    last
}

#[tokio::test(flavor = "multi_thread")]
async fn tone_plays_on_the_viewer_and_revoking_audio_stops_capture() {
    // 44.1 kHz mono capture exercises the resampler on the host; 48 kHz stereo playback.
    let capture = FakeCapture::new(44_100, 1, tone());
    let speaker = FakePlayback::new(48_000, 2);
    let perms = Permissions::VIEW | Permissions::AUDIO;
    let mut h = harness(
        perms,
        FeatureSet::new().with(Arc::new(AudioHost::new(Arc::new(capture.clone())))),
        FeatureSet::new().with(Arc::new(AudioViewer::new(Arc::new(speaker.clone()), true))),
    )
    .await;

    // Half a second of real (non-silent) audio arrives.
    wait_for("audible playback", || {
        speaker.recorded().iter().filter(|s| s.abs() > 0.05).count() > 48_000
    })
    .await;
    let played = speaker.recorded();
    let tail = &played[played.len() - 48_000..];
    let f = frequency(tail, 2, 48_000.0);
    assert!((f - 1000.0).abs() < 20.0, "played {f} Hz");
    let peak = tail.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!((0.35..=0.65).contains(&peak), "peak {peak}");
    assert_eq!(capture.starts(), 1);
    let state = last_state(&mut h.viewer_events).unwrap();
    assert!(
        state.permitted && state.enabled && state.playing,
        "{state:?}"
    );

    // The host user revokes AUDIO: capture and playback stop.
    h.host.set_permissions(Permissions::VIEW).await;
    wait_for("capture to stop", || !capture.is_running()).await;
    wait_for("playback to stop", || !speaker.is_running()).await;
    sleep(Duration::from_millis(100)).await;
    let state = last_state(&mut h.viewer_events).unwrap();
    assert!(!state.permitted && !state.playing, "{state:?}");

    // Granting it again resumes (the viewer still wants audio).
    h.host.set_permissions(perms).await;
    wait_for("capture to restart", || capture.is_running()).await;
    wait_for("playback to restart", || speaker.is_running()).await;
    assert_eq!(capture.starts(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_captured_unless_permitted_and_requested() {
    let capture = FakeCapture::new(48_000, 2, tone());
    let speaker = FakePlayback::new(48_000, 2);
    let viewer = Arc::new(AudioViewer::new(Arc::new(speaker.clone()), false));
    let h = harness(
        Permissions::VIEW | Permissions::AUDIO,
        FeatureSet::new().with(Arc::new(AudioHost::new(Arc::new(capture.clone())))),
        FeatureSet::new().with(viewer.clone()),
    )
    .await;
    // Permitted but not requested: no capture at all (zero CPU on the host).
    sleep(Duration::from_millis(500)).await;
    assert_eq!(capture.starts(), 0);
    assert_eq!(speaker.starts(), 0);

    // The toolbar toggle turns it on, and off again.
    assert!(viewer.set_enabled(VIEWER_KEY, true));
    wait_for("capture", || capture.is_running()).await;
    wait_for("playback", || speaker.is_running()).await;
    assert!(viewer.set_volume(VIEWER_KEY, 0.0));
    assert!(viewer.set_enabled(VIEWER_KEY, false));
    wait_for("capture to stop", || !capture.is_running()).await;
    wait_for("playback to stop", || !speaker.is_running()).await;
    assert!(!viewer.set_enabled(999, true), "unknown session");

    // Without AUDIO the viewer's request never reaches the feature.
    h.host.set_permissions(Permissions::VIEW).await;
    sleep(Duration::from_millis(100)).await;
    viewer.set_enabled(VIEWER_KEY, true);
    h.viewer_session
        .send(&Message::AudioControl {
            enabled: true,
            sample_rate: 48_000,
            channels: 2,
        })
        .await
        .unwrap();
    sleep(Duration::from_millis(500)).await;
    assert!(!capture.is_running());
    assert_eq!(capture.starts(), 1);
}

/// Reads one audio stream to its end; returns the number of packets.
async fn count_packets(mut recv: tlp_desk_net::RecvStream) -> usize {
    let mut n = 0;
    let mut len = [0u8; 2];
    while recv.read_exact(&mut len).await.is_ok() {
        let l = usize::from(u16::from_le_bytes(len));
        assert!((1..=1500).contains(&l), "packet length {l}");
        let mut pkt = vec![0u8; l];
        recv.read_exact(&mut pkt).await.unwrap();
        n += 1;
    }
    n
}

#[tokio::test(flavor = "multi_thread")]
async fn silence_sends_nothing_and_sound_starts_a_new_generation() {
    let capture = FakeCapture::new(48_000, 2, FakeSignal::Silence);
    // No viewer feature: the test plays the viewer and reads the raw streams.
    let h = harness(
        Permissions::VIEW | Permissions::AUDIO,
        FeatureSet::new().with(Arc::new(AudioHost::new(Arc::new(capture.clone())))),
        FeatureSet::new(),
    )
    .await;
    let v = h.viewer_session.clone();
    v.send(&Message::AudioControl {
        enabled: true,
        sample_rate: 48_000,
        channels: 2,
    })
    .await
    .unwrap();
    let reply = timeout(T, v.recv()).await.unwrap().unwrap();
    assert_eq!(
        reply,
        Message::AudioControl {
            enabled: true,
            sample_rate: 48_000,
            channels: 2
        }
    );
    wait_for("capture", || capture.is_running()).await;

    // Silence: no stream at all.
    assert!(
        timeout(
            Duration::from_millis(600),
            v.accept_side_stream(StreamKind::Audio)
        )
        .await
        .is_err(),
        "a stream was opened for silence"
    );

    // Sound: generation 0 starts; silence again ends it after the hangover.
    capture.set_signal(tone());
    let s0 = timeout(T, v.accept_side_stream(StreamKind::Audio))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s0.id, 0);
    sleep(Duration::from_millis(300)).await;
    capture.set_signal(FakeSignal::Silence);
    let n0 = timeout(T, count_packets(s0.recv)).await.unwrap();
    // ~30 frames of tone plus the 200 ms hangover, at 10 ms per packet.
    assert!((20..=80).contains(&n0), "{n0} packets");

    // Sound again: a new generation.
    capture.set_signal(tone());
    let s1 = timeout(T, v.accept_side_stream(StreamKind::Audio))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s1.id, 1);

    // Stop: the host confirms.
    v.send(&Message::AudioControl {
        enabled: false,
        sample_rate: 48_000,
        channels: 2,
    })
    .await
    .unwrap();
    let reply = timeout(T, v.recv()).await.unwrap().unwrap();
    assert!(matches!(
        reply,
        Message::AudioControl { enabled: false, .. }
    ));
    wait_for("capture to stop", || !capture.is_running()).await;
}
