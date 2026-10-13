//! End-to-end port forwarding over loopback: a real host and viewer in one process (QUIC on
//! 127.0.0.1, synthetic screen), with tiny TCP servers on 127.0.0.1 as the "remote service".
//! Run with `--nocapture` to see the measured throughput.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tlp_desk_capture::{BgraFrame, CaptureError, CursorState, MonitorInfo, ScreenCapturer};
use tlp_desk_crypto::Identity;
use tlp_desk_host::{
    FeatureSet, Host, HostConfig, HostEvent, HostEvents, HostTunnelEvent, HostTunnels, SessionId,
    TunnelHostConfig, DEFAULT_PERMISSIONS, TUNNEL_FEATURE,
};
use tlp_desk_input::{InputError, InputEvent, InputInjector, LocalActivity, MonitorRect};
use tlp_desk_net::{Endpoint, EndpointConfig};
use tlp_desk_proto::Permissions;
use tlp_desk_viewer::{
    AuthMethod, ConnectTarget, ForwardSpec, KnownHostsStore, PixelFormat, TunnelCloseReason,
    TunnelEvent, TunnelStatus, TunnelViewerConfig, Viewer, ViewerConfig, ViewerError, ViewerEvent,
    ViewerEvents, ViewerSession, ViewerTunnels,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const WAIT: Duration = Duration::from_secs(20);

/// A small static screen at a low rate: the tests are about tunnels, not video.
struct StillCapturer {
    mon: MonitorInfo,
    next_at: Instant,
}

fn monitor() -> MonitorInfo {
    MonitorInfo {
        index: 0,
        name: "FAKE".into(),
        width: 64,
        height: 64,
        primary: true,
        x: 0,
        y: 0,
    }
}

impl ScreenCapturer for StillCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<BgraFrame>, CaptureError> {
        let now = Instant::now();
        if now < self.next_at {
            std::thread::sleep(timeout.min(self.next_at - now));
            return Ok(None);
        }
        self.next_at = now + Duration::from_millis(200);
        Ok(Some(BgraFrame::new_tight(64, 64)))
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

struct Rig {
    host: Host,
    host_events: HostEvents,
    host_session: SessionId,
    host_tunnels: Arc<HostTunnels>,
    session: ViewerSession,
    events: ViewerEvents,
    tunnels: Arc<ViewerTunnels>,
    _viewer_ep: Arc<Endpoint>,
}

impl Rig {
    fn key(&self) -> u64 {
        self.session.feature_key()
    }

    async fn forward(&self, target_host: Option<&str>, port: u16) -> TunnelStatus {
        self.tunnels
            .add(
                self.key(),
                ForwardSpec::local(0, target_host.map(str::to_owned), port),
            )
            .await
            .expect("forwarding rule")
    }

    /// Next tunnel event from the viewer side.
    async fn tunnel_event(&mut self) -> TunnelEvent {
        within("viewer tunnel event", async {
            loop {
                match self.events.recv().await.expect("viewer events closed") {
                    ViewerEvent::Feature(e) if e.feature == TUNNEL_FEATURE => {
                        return e.payload::<TunnelEvent>().expect("payload").clone();
                    }
                    _ => {}
                }
            }
        })
        .await
    }

    /// Next host tunnel event matching `pred`.
    async fn host_tunnel_event(
        &mut self,
        pred: impl Fn(&HostTunnelEvent) -> bool,
    ) -> HostTunnelEvent {
        within("host tunnel event", async {
            loop {
                if let HostEvent::Feature { event, .. } =
                    self.host_events.recv().await.expect("host events closed")
                {
                    if let Some(p) = event.payload::<HostTunnelEvent>() {
                        if pred(p) {
                            return p.clone();
                        }
                    }
                }
            }
        })
        .await
    }
}

async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, fut)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// Host + viewer with the tunnel feature on both sides; `perms` is what the host grants.
async fn rig_with(perms: Permissions, host_cfg: TunnelHostConfig) -> Rig {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_test_writer()
        .try_init();
    let host_tunnels = HostTunnels::new(host_cfg);
    let cfg = HostConfig {
        advertise: false,
        fps: 5,
        default_permissions: perms,
        capturer_factory: Arc::new(|_| {
            Ok(Box::new(StillCapturer {
                mon: monitor(),
                next_at: Instant::now(),
            }) as Box<dyn ScreenCapturer>)
        }),
        monitor_lister: Arc::new(|| Ok(vec![monitor()])),
        injector_factory: Arc::new(
            |_: MonitorRect| Ok(Box::new(NoInput) as Box<dyn InputInjector>),
        ),
        local_activity_factory: Arc::new(|| Ok(Arc::new(NoLocal) as Arc<dyn LocalActivity>)),
        remote_pointer_marker: false,
        features: FeatureSet::new().with(host_tunnels.clone()),
        ..HostConfig::default()
    };
    let host_ep = endpoint(true);
    let target = ConnectTarget::IdAt(host_ep.device_id(), host_ep.local_addr().unwrap());
    let (host, mut host_events) = Host::start(host_ep, cfg).await.unwrap();
    let tunnels = ViewerTunnels::new(TunnelViewerConfig::default());
    let viewer_ep = endpoint(false);
    let viewer = Viewer::new(
        viewer_ep.clone(),
        ViewerConfig {
            known_hosts: KnownHostsStore::InMemory,
            pixel_format: PixelFormat::Rgba,
            features: FeatureSet::new().with(tunnels.clone()),
            ..ViewerConfig::default()
        },
    )
    .unwrap();
    let code = host.one_time_code().current();
    let (session, events) = within(
        "connect",
        viewer.connect(target, AuthMethod::OneTimeCode(code)),
    )
    .await
    .expect("connect");
    let host_session = within("host session", async {
        loop {
            if let HostEvent::SessionStarted(v) = host_events.recv().await.unwrap() {
                return v.id;
            }
        }
    })
    .await;
    Rig {
        host,
        host_events,
        host_session,
        host_tunnels,
        session,
        events,
        tunnels,
        _viewer_ep: viewer_ep,
    }
}

async fn rig() -> Rig {
    rig_with(
        DEFAULT_PERMISSIONS | Permissions::TUNNEL,
        TunnelHostConfig::default(),
    )
    .await
}

/// Echo server on 127.0.0.1: copies everything back, then half-closes after the client did.
async fn echo_server() -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
                let _ = w.shutdown().await;
            });
        }
    });
    addr
}

/// Minimal HTTP/1.0 server answering every request with an Arabic body.
async fn http_server(body: &'static str) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut req = Vec::new();
                let mut buf = [0u8; 1024];
                while !req.ends_with(b"\r\n\r\n") {
                    match s.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => req.extend_from_slice(&buf[..n]),
                    }
                }
                let head = format!(
                    "HTTP/1.0 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes()).await;
                let _ = s.write_all(body.as_bytes()).await;
            });
        }
    });
    addr
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_request_reaches_the_remote_service() {
    let body = "مرحباً من الجهاز البعيد — hello from the remote PC";
    let web = http_server(body).await;
    let rig = rig().await;
    let rule = rig.forward(Some("127.0.0.1"), web.port()).await;
    assert!(rule.local.ip().is_loopback(), "listens on loopback only");
    assert_ne!(rule.local.port(), 0);

    let mut c = TcpStream::connect(rule.local).await.unwrap();
    c.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut resp = Vec::new();
    within("http response", c.read_to_end(&mut resp))
        .await
        .unwrap();
    let text = String::from_utf8(resp).unwrap();
    assert!(text.starts_with("HTTP/1.0 200 OK"), "{text}");
    assert!(
        text.ends_with(body),
        "Arabic body must arrive byte-exact: {text}"
    );

    // The host lists the tunnel; the viewer counts the connection.
    let on_host = rig.host_tunnels.tunnels(rig.host_session);
    assert_eq!(on_host.len(), 1);
    assert_eq!(on_host[0].target, web);
    assert_eq!(on_host[0].connections, 1);
    let list = rig.tunnels.list(rig.key());
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].connections, 1);
    assert!(list[0].bytes_received > body.len() as u64);
    assert!(rig.tunnels.stop(rig.key(), rule.id));
    assert!(rig.tunnels.list(rig.key()).is_empty());
    // The listener is gone.
    within("listener closed", async {
        while TcpStream::connect(rule.local).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_concurrent_connections() {
    let echo = echo_server().await;
    let rig = rig().await;
    let rule = rig.forward(None, echo.port()).await; // None = the host itself
    let n = 48;
    let mut tasks = Vec::new();
    for i in 0..n {
        let local = rule.local;
        tasks.push(tokio::spawn(async move {
            let data = pattern(64 * 1024 + i * 17, i as u8);
            let mut c = TcpStream::connect(local).await.unwrap();
            let (mut r, mut w) = c.split();
            let send = async {
                w.write_all(&data).await.unwrap();
                w.shutdown().await.unwrap();
            };
            let mut back = Vec::new();
            let recv = r.read_to_end(&mut back);
            let ((), got) = tokio::join!(send, recv);
            got.unwrap();
            assert_eq!(back, data, "connection {i} corrupted");
        }));
    }
    within("all connections", async {
        for t in tasks {
            t.await.unwrap();
        }
    })
    .await;
    let s = &rig.tunnels.list(rig.key())[0];
    assert_eq!(s.connections, n as u64);
    assert_eq!(s.failed, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn large_transfer_throughput_and_half_close() {
    // Upload sink: reads until the client half-closes, then answers with the byte count and
    // a checksum (only possible if the half-close crossed the tunnel and the reply direction
    // stayed open).
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let sink = l.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 256 * 1024];
                let (mut total, mut sum) = (0u64, 0u64);
                loop {
                    match s.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            total += n as u64;
                            sum = buf[..n]
                                .iter()
                                .fold(sum, |a, b| a.wrapping_mul(31).wrapping_add(u64::from(*b)));
                        }
                    }
                }
                let _ = s.write_all(format!("{total} {sum}").as_bytes()).await;
            });
        }
    });
    let rig = rig().await;
    let rule = rig.forward(Some("localhost"), sink.port()).await;

    const SIZE: usize = 48 * 1024 * 1024;
    let data = pattern(SIZE, 7);
    let want_sum = data
        .iter()
        .fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(u64::from(*b)));
    let mut c = TcpStream::connect(rule.local).await.unwrap();
    let t0 = Instant::now();
    within("upload", async {
        for chunk in data.chunks(1024 * 1024) {
            c.write_all(chunk).await.unwrap();
        }
        c.shutdown().await.unwrap();
    })
    .await;
    let mut reply = String::new();
    within("upload reply", c.read_to_string(&mut reply))
        .await
        .unwrap();
    let secs = t0.elapsed().as_secs_f64();
    assert_eq!(reply, format!("{SIZE} {want_sum}"));
    let mbps = SIZE as f64 / (1024.0 * 1024.0) / secs;
    println!(
        "tunnel upload: {} MiB in {secs:.2} s = {mbps:.1} MiB/s",
        SIZE / (1024 * 1024)
    );

    // Download direction: the service sends a large body and closes.
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let src = l.local_addr().unwrap();
    let body = Arc::new(pattern(SIZE, 99));
    let served = body.clone();
    tokio::spawn(async move {
        if let Ok((mut s, _)) = l.accept().await {
            let _ = s.write_all(&served).await;
        }
    });
    let rule2 = rig.forward(None, src.port()).await;
    let mut c = TcpStream::connect(rule2.local).await.unwrap();
    let t0 = Instant::now();
    let mut got = Vec::with_capacity(SIZE);
    within("download", c.read_to_end(&mut got)).await.unwrap();
    let secs = t0.elapsed().as_secs_f64();
    assert!(got == *body, "download corrupted ({} bytes)", got.len());
    println!(
        "tunnel download: {} MiB in {secs:.2} s = {:.1} MiB/s",
        SIZE / (1024 * 1024),
        SIZE as f64 / (1024.0 * 1024.0) / secs
    );
    let s = rig
        .tunnels
        .list(rig.key())
        .into_iter()
        .find(|s| s.id == rule.id)
        .unwrap();
    assert_eq!(s.bytes_sent, SIZE as u64);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_tunnel_permission_forwarding_is_refused() {
    let echo = echo_server().await;
    let mut rig = rig_with(DEFAULT_PERMISSIONS, TunnelHostConfig::default()).await;
    let err = rig
        .tunnels
        .add(rig.key(), ForwardSpec::local(0, None, echo.port()))
        .await
        .unwrap_err();
    assert!(matches!(err, ViewerError::TunnelNotAllowed), "{err}");

    // A viewer that skips its own check still gets nothing: the host drops TunnelOpen
    // without TUNNEL (no answer), so no tunnel ever opens.
    assert!(rig
        .session
        .send_message(tlp_desk_proto::Message::TunnelOpen {
            id: 77,
            target_port: echo.port(),
            target_host: None,
        }));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(rig.host.sessions()[0].permissions == DEFAULT_PERMISSIONS);
    let no_event = tokio::time::timeout(Duration::from_millis(300), rig.tunnel_event()).await;
    assert!(no_event.is_err(), "no tunnel event expected: {no_event:?}");

    // Granting it live makes forwarding work.
    assert!(rig
        .host
        .set_permissions(rig.host_session, DEFAULT_PERMISSIONS | Permissions::TUNNEL));
    within("permission granted", async {
        while !rig.session.permissions().contains(Permissions::TUNNEL) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    // The feature sees the change right after the session does.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let rule = rig.forward(None, echo.port()).await;
    let mut c = TcpStream::connect(rule.local).await.unwrap();
    c.write_all("سلام".as_bytes()).await.unwrap();
    let mut b = [0u8; 8];
    within("echo", c.read_exact(&mut b)).await.unwrap();
    assert_eq!(&b, "سلام".as_bytes());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoking_tunnel_closes_everything() {
    let echo = echo_server().await;
    let mut rig = rig().await;
    let rule = rig.forward(None, echo.port()).await;
    let mut c = TcpStream::connect(rule.local).await.unwrap();
    c.write_all(b"before").await.unwrap();
    let mut b = [0u8; 6];
    within("echo", c.read_exact(&mut b)).await.unwrap();
    rig.host_tunnel_event(
        |e| matches!(e, HostTunnelEvent::Changed(l) if l.first().is_some_and(|t| t.active == 1)),
    )
    .await;

    assert!(rig
        .host
        .set_permissions(rig.host_session, DEFAULT_PERMISSIONS));
    let closed = within("closed event", async {
        loop {
            if let TunnelEvent::Closed { id, reason } = rig.tunnel_event().await {
                return (id, reason);
            }
        }
    })
    .await;
    assert_eq!(closed, (rule.id, TunnelCloseReason::PermissionRevoked));
    rig.host_tunnel_event(|e| matches!(e, HostTunnelEvent::Changed(l) if l.is_empty()))
        .await;

    // The open connection is cut and the local port no longer accepts.
    let mut rest = Vec::new();
    let r = within("connection cut", c.read_to_end(&mut rest)).await;
    assert!(r.is_err() || rest.is_empty(), "{r:?} {rest:?}");
    within("listener closed", async {
        while TcpStream::connect(rule.local).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(rig.tunnels.list(rig.key()).is_empty());
    assert!(matches!(
        rig.tunnels
            .add(rig.key(), ForwardSpec::local(0, None, echo.port()))
            .await,
        Err(ViewerError::TunnelNotAllowed)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn internet_and_named_targets_are_refused() {
    let mut rig = rig().await;
    for (host, want) in [
        ("8.8.8.8", TunnelCloseReason::TargetNotAllowed),
        ("2001:4860:4860::8888", TunnelCloseReason::TargetNotAllowed),
        ("0.0.0.0", TunnelCloseReason::TargetNotAllowed),
    ] {
        let err = rig
            .tunnels
            .add(rig.key(), ForwardSpec::local(0, Some(host.into()), 80))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ViewerError::TunnelRefused(r) if *r == want),
            "{host}: {err}"
        );
        let ev = rig
            .host_tunnel_event(|e| matches!(e, HostTunnelEvent::Refused { .. }))
            .await;
        assert!(
            matches!(&ev, HostTunnelEvent::Refused { target_host: Some(h), reason: "target-not-allowed", .. } if h == host),
            "{ev:?}"
        );
    }
    // A host name is never resolved (no DNS query may leave the LAN).
    let err = rig
        .tunnels
        .add(
            rig.key(),
            ForwardSpec::local(0, Some("example.com".into()), 80),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            &err,
            ViewerError::TunnelRefused(TunnelCloseReason::BadTarget)
        ),
        "{err}"
    );
    assert!(rig.tunnels.list(rig.key()).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unreachable_service_and_limits() {
    // A port nobody listens on: the local connection is closed and counted as failed.
    let dead = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap()
    };
    let rig = rig_with(
        DEFAULT_PERMISSIONS | Permissions::TUNNEL,
        TunnelHostConfig {
            max_tunnels: 2,
            ..TunnelHostConfig::default()
        },
    )
    .await;
    let rule = rig.forward(None, dead.port()).await;
    let mut c = TcpStream::connect(rule.local).await.unwrap();
    let _ = c.write_all(b"x").await;
    let mut rest = Vec::new();
    let r = within("closed", c.read_to_end(&mut rest)).await;
    assert!(r.is_err() || rest.is_empty());
    within("failure counted", async {
        while rig.tunnels.list(rig.key())[0].failed == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;

    // Host limit: two tunnels per session in this rig.
    rig.forward(None, dead.port()).await;
    let err = rig
        .tunnels
        .add(rig.key(), ForwardSpec::local(0, None, dead.port()))
        .await
        .unwrap_err();
    assert!(
        matches!(
            &err,
            ViewerError::TunnelRefused(TunnelCloseReason::TooManyTunnels)
        ),
        "{err}"
    );
}
