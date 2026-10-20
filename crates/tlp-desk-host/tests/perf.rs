//! Loopback tests of the performance features: quality presets applied live, adaptive bitrate
//! from viewer feedback, static-screen detection, latency echo and the all-monitors view.
//! Synthetic capturers and a recording encoder; nothing touches the real screen.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tlp_desk_capture::{
    fit_within, BgraFrame, CaptureError, CaptureOptions, CursorState, MonitorInfo, ScreenCapturer,
};
use tlp_desk_codec::{CodecError, EncodedFrame, EncoderConfig, VideoEncoder};
use tlp_desk_crypto::Identity;
use tlp_desk_host::{Host, HostConfig, HostEvent, HostEvents};
use tlp_desk_input::{InputError, InputEvent, InputInjector, LocalActivity, MonitorRect};
use tlp_desk_net::{Endpoint, EndpointConfig};
use tlp_desk_proto::{Message, QualityPreset, QualitySettings};
use tlp_desk_viewer::{
    AuthMethod, ConnectTarget, DecodedFrame, KnownHostsStore, PixelFormat, Viewer, ViewerConfig,
    ViewerEvents, ViewerSession,
};

const WAIT: Duration = Duration::from_secs(20);

fn monitor(index: u32, x: i32, y: i32, width: u32, height: u32) -> MonitorInfo {
    MonitorInfo {
        index,
        name: format!("FAKE-{index}"),
        width,
        height,
        primary: index == 0,
        x,
        y,
    }
}

/// What the capturer factory was asked for.
type Opened = Arc<Mutex<Vec<(u32, Option<(u32, u32)>)>>>;

/// A fake monitor that honours `CaptureOptions::max_size` like the real capturers. `moving`:
/// a bar moves every frame; otherwise every frame is identical (a static desktop that the OS
/// still reports as "new frames").
struct Fake {
    mon: MonitorInfo,
    size: (u32, u32),
    moving: bool,
    n: u32,
    shade: u8,
    next_at: Instant,
}

impl ScreenCapturer for Fake {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<BgraFrame>, CaptureError> {
        let now = Instant::now();
        if now < self.next_at {
            std::thread::sleep(timeout.min(self.next_at - now));
            if Instant::now() < self.next_at {
                return Ok(None);
            }
        }
        self.next_at = Instant::now() + Duration::from_millis(16);
        self.n += 1;
        let (w, h) = self.size;
        let mut f = BgraFrame::new_tight(w, h);
        let bar = if self.moving { (self.n * 8) % w } else { 0 };
        for y in 0..h {
            let row = f.row_mut(y);
            for x in 0..w {
                let i = x as usize * 4;
                let on = x >= bar && x < bar + 16;
                let v = if on {
                    230
                } else {
                    self.shade.wrapping_add((y % 64) as u8)
                };
                row[i..i + 4].copy_from_slice(&[v, v / 2, 255 - v, 255]);
            }
        }
        Ok(Some(f))
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
        &self.mon
    }

    fn frame_size(&self) -> (u32, u32) {
        self.size
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EncEvent {
    Created(EncoderConfig),
    Bitrate(u32),
    Encoded,
}

type EncLog = Arc<Mutex<Vec<EncEvent>>>;

/// The real encoder, recording what the host asks of it.
struct Recording {
    inner: Box<dyn VideoEncoder>,
    log: EncLog,
}

impl VideoEncoder for Recording {
    fn encode(&mut self, frame: &BgraFrame) -> Result<Option<EncodedFrame>, CodecError> {
        self.log.lock().unwrap().push(EncEvent::Encoded);
        self.inner.encode(frame)
    }
    fn request_keyframe(&mut self) {
        self.inner.request_keyframe();
    }
    fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError> {
        self.log.lock().unwrap().push(EncEvent::Bitrate(kbps));
        self.inner.set_bitrate(kbps)
    }
    fn config(&self) -> EncoderConfig {
        self.inner.config()
    }
}

struct NoInput;

impl InputInjector for NoInput {
    fn inject(&mut self, _: &InputEvent) -> Result<(), InputError> {
        Ok(())
    }
}

struct NoLocal;

impl LocalActivity for NoLocal {
    fn last_physical_input(&self) -> Option<Instant> {
        None
    }
}

struct Rig {
    _host: Host,
    _events: HostEvents,
    session: ViewerSession,
    vevents: ViewerEvents,
    opened: Opened,
    enc: EncLog,
    rects: Arc<Mutex<Vec<MonitorRect>>>,
    /// The host's monitor layout (edit it to simulate hot-plug).
    layout: Arc<Mutex<Vec<MonitorInfo>>>,
}

struct Setup {
    monitors: Vec<MonitorInfo>,
    moving: bool,
    tweak: Box<dyn FnOnce(&mut HostConfig)>,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            monitors: vec![monitor(0, 0, 0, 640, 400)],
            moving: true,
            tweak: Box::new(|_| {}),
        }
    }
}

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

async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, fut)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

async fn rig(setup: Setup) -> Rig {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_test_writer()
        .try_init();
    let opened: Opened = Arc::default();
    let enc: EncLog = Arc::default();
    let rects: Arc<Mutex<Vec<MonitorRect>>> = Arc::default();
    let (o, e, r) = (opened.clone(), enc.clone(), rects.clone());
    let layout = Arc::new(Mutex::new(setup.monitors.clone()));
    let (list, list2) = (layout.clone(), layout.clone());
    let moving = setup.moving;
    let mut cfg = HostConfig {
        advertise: false,
        remote_pointer_marker: false,
        local_activity_factory: Arc::new(|| Ok(Arc::new(NoLocal) as Arc<dyn LocalActivity>)),
        injector_factory: Arc::new(move |rect| {
            r.lock().unwrap().push(rect);
            Ok(Box::new(NoInput) as Box<dyn InputInjector>)
        }),
        monitor_lister: Arc::new(move || Ok(list.lock().unwrap().clone())),
        capturer_factory: Arc::new(move |i, opts: CaptureOptions| {
            o.lock().unwrap().push((i, opts.max_size));
            let mon = list2
                .lock()
                .unwrap()
                .iter()
                .find(|m| m.index == i)
                .cloned()
                .ok_or(CaptureError::NoSuchMonitor(i))?;
            let size = opts.max_size.map_or((mon.width, mon.height), |c| {
                fit_within((mon.width, mon.height), c)
            });
            Ok(Box::new(Fake {
                shade: (i as u8).wrapping_mul(90),
                mon,
                size,
                moving,
                n: 0,
                next_at: Instant::now(),
            }) as Box<dyn ScreenCapturer>)
        }),
        encoder_factory: Arc::new(move |cfg| {
            e.lock().unwrap().push(EncEvent::Created(cfg));
            Ok(Box::new(Recording {
                inner: tlp_desk_codec::new_encoder(cfg)?,
                log: e.clone(),
            }) as Box<dyn VideoEncoder>)
        }),
        ..HostConfig::default()
    };
    (setup.tweak)(&mut cfg);
    let host_ep = endpoint(true);
    let target = ConnectTarget::IdAt(host_ep.device_id(), host_ep.local_addr().unwrap());
    let (host, mut events) = Host::start(host_ep, cfg).await.unwrap();
    let viewer = Viewer::new(
        endpoint(false),
        ViewerConfig {
            known_hosts: KnownHostsStore::InMemory,
            pixel_format: PixelFormat::Rgba,
            ..ViewerConfig::default()
        },
    )
    .unwrap();
    let conn = tokio::spawn(async move { viewer.connect(target, AuthMethod::AcceptPrompt).await });
    let req = within("accept prompt", async {
        loop {
            if let Some(HostEvent::IncomingRequest(r)) = events.recv().await {
                return r.id;
            }
        }
    })
    .await;
    assert!(host.accept(req, None));
    let (session, vevents) = conn.await.unwrap().expect("connect");
    Rig {
        _host: host,
        _events: events,
        session,
        vevents,
        opened,
        enc,
        rects,
        layout,
    }
}

/// Next decoded frame of exactly `w` x `h`.
async fn frame_of_size(s: &ViewerSession, w: u32, h: u32) -> DecodedFrame {
    within(&format!("a {w}x{h} frame"), async {
        loop {
            if let Some(f) = s.take_frame() {
                if (f.width, f.height) == (w, h) {
                    return f;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
}

fn quality(preset: QualityPreset) -> QualitySettings {
    QualitySettings {
        preset,
        max_width: 0,
        max_height: 0,
        fps: 0,
        bitrate_kbps: None,
    }
}

fn created(log: &EncLog) -> Vec<EncoderConfig> {
    log.lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            EncEvent::Created(c) => Some(*c),
            _ => None,
        })
        .collect()
}

/// SetQuality changes the capture cap, the frame rate and the bitrate of a running session;
/// the host reopens the capturer with the new cap and rebuilds the encoder.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn set_quality_applies_cap_fps_and_bitrate_live() {
    let r = rig(Setup {
        tweak: Box::new(|c| c.max_capture_size = (320, 200)),
        ..Setup::default()
    })
    .await;
    // Host default (Balanced): the configured cap.
    frame_of_size(&r.session, 320, 200).await;
    assert_eq!(r.opened.lock().unwrap()[0], (0, Some((320, 200))));

    // Custom: explicit cap, 20 fps, fixed 1500 kbps.
    assert!(r.session.send_message(Message::SetQuality(QualitySettings {
        preset: QualityPreset::Custom,
        max_width: 480,
        max_height: 300,
        fps: 20,
        bitrate_kbps: Some(1_500),
    })));
    frame_of_size(&r.session, 480, 300).await;
    let cfg = *created(&r.enc).last().unwrap();
    assert_eq!(
        (cfg.width, cfg.height, cfg.fps, cfg.bitrate_kbps),
        (480, 300, 20, 1_500)
    );

    // Speed: 1280x720 cap (the 640x400 monitor fits: native), adaptive bitrate.
    assert!(r
        .session
        .send_message(Message::SetQuality(quality(QualityPreset::Speed))));
    frame_of_size(&r.session, 640, 400).await;
    assert!(r.opened.lock().unwrap().contains(&(0, Some((1280, 720)))));
    let cfg = *created(&r.enc).last().unwrap();
    assert_eq!((cfg.width, cfg.height, cfg.fps), (640, 400, 60));
    // 60 % of the default bitrate for 640x400@60.
    let default = EncoderConfig::for_screen(640, 400, 60).bitrate_kbps;
    assert_eq!(cfg.bitrate_kbps, (default * 60 / 100).max(300));
    // The session keeps streaming the newest frames afterwards.
    let a = frame_of_size(&r.session, 640, 400).await;
    let b = frame_of_size(&r.session, 640, 400).await;
    assert!(b.seq > a.seq);
    // Input still maps to the whole physical monitor, whatever the frame size.
    assert!(r
        .session
        .send_input(InputEvent::MouseMove { x: 100, y: 100 }));
    let rect = within("the injector", async {
        loop {
            if let Some(rect) = r.rects.lock().unwrap().last().copied() {
                return rect;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert_eq!(rect, MonitorRect::new(0, 0, 640, 400));
    drop(r.vevents);
}

fn encoded(log: &EncLog) -> usize {
    log.lock()
        .unwrap()
        .iter()
        .filter(|e| **e == EncEvent::Encoded)
        .count()
}

/// The capturer keeps delivering frames (as WGC does for many reasons), but they are all
/// identical: after the first frame nothing is encoded or sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn static_screen_is_not_reencoded() {
    let r = rig(Setup {
        moving: false,
        ..Setup::default()
    })
    .await;
    frame_of_size(&r.session, 640, 400).await;
    // Let the first frames settle, then watch a full second of a static desktop
    // (the fake delivers ~60 identical frames per second).
    tokio::time::sleep(Duration::from_millis(300)).await;
    let enc_before = encoded(&r.enc);
    // Frames encoded before the snapshot may still be decoding (slow under a loaded CI box):
    // drain until the stream has been quiet for a while, so only new frames count below.
    within("the stream to go quiet", async {
        let mut quiet_since = tokio::time::Instant::now();
        while quiet_since.elapsed() < Duration::from_millis(300) {
            if r.session.take_frame().is_some() {
                quiet_since = tokio::time::Instant::now();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert_eq!(
        encoded(&r.enc),
        enc_before,
        "a static frame was encoded while draining"
    );
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let enc_after = encoded(&r.enc);
    assert!(
        enc_before <= 3,
        "{enc_before} frames encoded for a static screen"
    );
    assert_eq!(enc_after, enc_before, "static frames were encoded");
    assert!(r.session.take_frame().is_none(), "static frames were sent");
}

/// RGBA pixel of a decoded frame.
fn rgba(f: &DecodedFrame, x: u32, y: u32) -> [u8; 4] {
    let i = (y as usize * f.width as usize + x as usize) * 4;
    f.pixels[i..i + 4].try_into().unwrap()
}

/// "All monitors" with a secondary monitor left of and above the primary (negative origin):
/// one composed picture of the virtual-desktop bounding box, input mapped across it, and
/// back to one monitor with `SelectMonitor`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn all_monitors_compose_the_virtual_desktop_and_map_input() {
    use tlp_desk_input::normalize_pixel;
    use tlp_desk_viewer::ViewerEvent;

    let mut r = rig(Setup {
        monitors: vec![monitor(0, 0, 0, 640, 400), monitor(1, -480, -100, 480, 300)],
        moving: false,
        ..Setup::default()
    })
    .await;
    frame_of_size(&r.session, 640, 400).await;
    assert!(r.session.send_message(Message::SelectAllMonitors));
    // Bounding box: x -480..640, y -100..400 -> 1120x500 (fits the default cap).
    let f = frame_of_size(&r.session, 1120, 500).await;
    let active = within("the monitor list", async {
        loop {
            match r.vevents.recv().await {
                Some(ViewerEvent::MonitorsChanged { active, monitors }) if active == u32::MAX => {
                    return (active, monitors.len());
                }
                Some(_) => {}
                None => panic!("session ended"),
            }
        }
    })
    .await;
    assert_eq!(active, (u32::MAX, 2));
    // Each monitor sits at its place: the secondary at the top left (shade 90), the primary
    // 100 px lower on the right (shade 0); below the secondary there is no screen (black).
    // The fake paints blue = shade + (row % 64); allow for lossy H.264.
    let near = |got: u8, want: u8| got.abs_diff(want) <= 24;
    assert!(
        near(rgba(&f, 100, 250)[2], 90 + (250 % 64) as u8),
        "{:?}",
        rgba(&f, 100, 250)
    );
    assert!(near(rgba(&f, 800, 150)[2], 50), "{:?}", rgba(&f, 800, 150));
    let gap = rgba(&f, 100, 450);
    assert!(gap[0] < 24 && gap[1] < 24 && gap[2] < 24, "{gap:?}");

    // Input maps across the bounding box, negative origin included.
    let rect = within("the all-monitors injector", async {
        loop {
            let last = r.rects.lock().unwrap().last().copied();
            if let Some(rect) = last.filter(|r| r.width == 1120) {
                return rect;
            }
            r.session.send_input(InputEvent::MouseMove { x: 0, y: 0 });
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert_eq!(rect, MonitorRect::new(-480, -100, 1120, 500));
    assert_eq!(rect.normalized_to_pixel(0, 0), (-480, -100));
    assert_eq!(rect.normalized_to_pixel(u16::MAX, u16::MAX), (639, 399));
    // The pixel at (100, 250) of the picture is (-380, 150) on the desktop: on monitor 1.
    let (nx, ny) = (normalize_pixel(100, 1120), normalize_pixel(250, 500));
    let (px, py) = rect.normalized_to_pixel(nx, ny);
    assert_eq!((px, py), (-380, 150));
    assert!(MonitorRect::new(-480, -100, 480, 300).contains(px, py));
    // And (800, 150) is (320, 50): on the primary.
    let p = rect.normalized_to_pixel(normalize_pixel(800, 1120), normalize_pixel(150, 500));
    assert_eq!(p, (320, 50));

    // Hot-plug: a third monitor right of the primary widens the composed picture
    // (x -480..1040 = 1520 wide).
    r.layout.lock().unwrap().push(monitor(2, 640, 0, 400, 300));
    frame_of_size(&r.session, 1520, 500).await;

    // Back to one monitor.
    assert!(r.session.select_monitor(0));
    frame_of_size(&r.session, 640, 400).await;
}

/// Viewer feedback that reports lost frames makes the host lower the encoder bitrate.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn viewer_feedback_with_losses_lowers_the_bitrate() {
    let r = rig(Setup::default()).await;
    frame_of_size(&r.session, 640, 400).await;
    let start = created(&r.enc)[0].bitrate_kbps;
    let lowered = within("a lower bitrate", async {
        loop {
            r.session.send_message(Message::ViewerFeedback {
                decode_us: 2_000,
                frames_dropped: 5,
                rtt_us: 500,
            });
            let low = r.enc.lock().unwrap().iter().find_map(|e| match e {
                EncEvent::Bitrate(k) if *k < start => Some(*k),
                _ => None,
            });
            if let Some(k) = low {
                return k;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(lowered <= start * 6 / 10 + 1, "{lowered} vs {start}");
    // Malformed / hostile values do not hurt: the session keeps streaming.
    r.session.send_message(Message::ViewerFeedback {
        decode_us: u32::MAX,
        frames_dropped: u32::MAX,
        rtt_us: u32::MAX,
    });
    let a = frame_of_size(&r.session, 640, 400).await;
    let b = frame_of_size(&r.session, 640, 400).await;
    assert!(b.seq > a.seq);
}

/// A viewer that fits the screen into a window smaller than the screen sends its picture
/// area with the preset: the host scales the capture straight to that size (one scaling pass,
/// shown 1:1 by the viewer) and the bitrate follows the smaller frame. Withdrawing the hint
/// (actual-size view) streams the preset's own size again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn viewport_hint_scales_the_stream_to_the_viewer_window() {
    let r = rig(Setup::default()).await;
    // The fake monitor is 640x400; Quality streams it natively.
    assert!(r
        .session
        .send_message(Message::SetQuality(quality(QualityPreset::Quality))));
    frame_of_size(&r.session, 640, 400).await;
    let native = *created(&r.enc).last().unwrap();

    // A 400x300 picture area (a small window): 640x400 fits as 400x250.
    assert!(r.session.send_message(Message::SetQuality(QualitySettings {
        max_width: 400,
        max_height: 300,
        ..quality(QualityPreset::Quality)
    })));
    frame_of_size(&r.session, 400, 250).await;
    assert!(r.opened.lock().unwrap().contains(&(0, Some((400, 300)))));
    let small = *created(&r.enc).last().unwrap();
    assert_eq!((small.width, small.height), (400, 250));
    assert!(
        small.bitrate_kbps < native.bitrate_kbps,
        "{} !< {}",
        small.bitrate_kbps,
        native.bitrate_kbps
    );

    // Actual size again: no hint, the preset's cap.
    assert!(r
        .session
        .send_message(Message::SetQuality(quality(QualityPreset::Quality))));
    frame_of_size(&r.session, 640, 400).await;
    drop(r.vevents);
}
