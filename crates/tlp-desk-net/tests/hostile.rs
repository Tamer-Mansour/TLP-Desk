//! Hostile-peer tests: a peer that completed TLS (so it has a genuine key) but then speaks the
//! protocol wrong, too much, too slowly or too often. Every case must end in a clean
//! rejection or close: no panic, no stuck task, no leaked connection, and honest peers keep
//! working throughout.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tlp_desk_crypto::{Identity, PasswordVerifier};
use tlp_desk_net::{
    AuthKind, AuthMethod, Endpoint, EndpointConfig, NetError, Pin, RecvStream, SendStream, Session,
    StreamHeader, StreamKind, STREAM_REFUSED,
};
use tlp_desk_proto::{
    encode_frame, AuthRequest, AuthResult, Codec, FrameDecoder, Hello, Message, Permissions,
    RejectReason, Role, VideoFrame, PROTOCOL_VERSION,
};
use tokio::time::timeout;

const T: Duration = Duration::from_secs(10);

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tlp_desk_net=warn".into()),
        )
        .with_test_writer()
        .try_init();
}

fn cfg(listen: bool) -> EndpointConfig {
    EndpointConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        device_name: if listen { "host-pc" } else { "viewer-pc" }.into(),
        listen,
        ..EndpointConfig::default()
    }
}

struct Rig {
    host: Arc<Endpoint>,
    viewer: Arc<Endpoint>,
    viewer_id: Arc<Identity>,
    addr: SocketAddr,
}

fn rig_with(host_cfg: EndpointConfig) -> Rig {
    init_tracing();
    let host = Arc::new(Endpoint::bind(Arc::new(Identity::generate().unwrap()), host_cfg).unwrap());
    let viewer_id = Arc::new(Identity::generate().unwrap());
    let viewer = Arc::new(Endpoint::bind(viewer_id.clone(), cfg(false)).unwrap());
    let addr = host.local_addr().unwrap();
    Rig {
        host,
        viewer,
        viewer_id,
        addr,
    }
}

fn rig() -> Rig {
    rig_with(cfg(true))
}

impl Rig {
    /// A TLS-authenticated connection that has not said anything yet.
    async fn raw(&self) -> quinn::Connection {
        timeout(
            T,
            self.viewer
                .dial_raw(self.addr, Pin::DeviceId(self.host.device_id())),
        )
        .await
        .unwrap()
        .unwrap()
    }

    /// Raw connection + control stream after a correct `Hello` exchange.
    async fn raw_after_hello(&self) -> (quinn::Connection, SendStream, Reader) {
        let conn = self.raw().await;
        let (mut send, recv) = conn.open_bi().await.unwrap();
        let mut rx = Reader::new(recv);
        write(&mut send, &self.hello()).await;
        let host_hello = rx.next().await.expect("host Hello");
        assert!(matches!(host_hello, Message::Hello(_)), "{host_hello:?}");
        (conn, send, rx)
    }

    fn hello(&self) -> Message {
        Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: self.viewer_id.device_id().as_u32(),
            device_name: "viewer-pc".into(),
            role: Role::Viewer,
        })
    }

    /// An honest viewer connects with the accept prompt and the host accepts.
    async fn honest_session(&self) -> (Session, Session) {
        let host = self.host.clone();
        let h = tokio::spawn(async move {
            loop {
                let pending = host.accept().await.expect("pending session");
                if pending.peer().name == "viewer-pc" {
                    return pending.accept(Permissions::VIEW).await.unwrap();
                }
                pending.reject(RejectReason::NotAllowed).await;
            }
        });
        let v = timeout(
            T,
            self.viewer.connect(
                self.addr,
                Some(self.host.device_id()),
                AuthMethod::AcceptPrompt,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        (timeout(T, h).await.unwrap().unwrap(), v)
    }
}

/// Framed reader over a raw receive stream.
struct Reader {
    recv: RecvStream,
    dec: FrameDecoder,
}

impl Reader {
    fn new(recv: RecvStream) -> Self {
        Self {
            recv,
            dec: FrameDecoder::new(),
        }
    }

    /// Next message, or `None` once the stream or connection ended.
    async fn next(&mut self) -> Option<Message> {
        let mut buf = [0u8; 4096];
        loop {
            if let Some(m) = self.dec.next_message().unwrap() {
                return Some(m);
            }
            match timeout(T, self.recv.read(&mut buf)).await.unwrap() {
                Ok(Some(n)) => self.dec.push(&buf[..n]),
                _ => return None,
            }
        }
    }
}

async fn write(send: &mut SendStream, msg: &Message) {
    send.write_all(&encode_frame(msg).unwrap()).await.unwrap();
}

/// Waits for the host to close `conn`; returns the application close code, if any.
async fn closed_by_host(conn: &quinn::Connection) -> Option<u64> {
    match timeout(T, conn.closed()).await.expect("host never closed") {
        quinn::ConnectionError::ApplicationClosed(c) => Some(c.error_code.into_inner()),
        _ => None,
    }
}

/// The host application never sees a pending session.
async fn no_pending(host: &Endpoint) {
    assert!(
        timeout(Duration::from_millis(300), host.accept())
            .await
            .is_err(),
        "a hostile peer reached the host application"
    );
}

/// Waits until the endpoint has no open QUIC connection left.
async fn drained(ep: &Endpoint, what: &str, within: Duration) {
    let deadline = Instant::now() + within;
    while ep.open_connections() > 0 {
        assert!(
            Instant::now() < deadline,
            "{what}: {} connections still open",
            ep.open_connections()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

const CLOSE_PROTOCOL: u64 = 2;

// ------------------------------------------------------------------------------------------
// Handshake
// ------------------------------------------------------------------------------------------

/// Bytes that are not a valid message where `Hello` belongs: the host drops the connection.
#[tokio::test(flavor = "multi_thread")]
async fn garbage_instead_of_hello_closes_the_connection() {
    let r = rig();
    let conn = r.raw().await;
    let (mut send, _recv) = conn.open_bi().await.unwrap();
    let mut junk = 12u32.to_le_bytes().to_vec();
    junk.extend_from_slice(&[0xFF; 12]);
    send.write_all(&junk).await.unwrap();
    closed_by_host(&conn).await;
    no_pending(&r.host).await;
}

/// A length prefix of 4 GiB is refused at once, without waiting for (or buffering) a body.
#[tokio::test(flavor = "multi_thread")]
async fn oversized_prefix_before_hello_closes_the_connection() {
    let r = rig();
    let conn = r.raw().await;
    let (mut send, _recv) = conn.open_bi().await.unwrap();
    send.write_all(&u32::MAX.to_le_bytes()).await.unwrap();
    let start = Instant::now();
    closed_by_host(&conn).await;
    assert!(start.elapsed() < Duration::from_secs(5));
    no_pending(&r.host).await;
}

/// Before authentication a frame may not be large: a 200 KiB `Hello` is refused instead of
/// being buffered and shown to the host user.
#[tokio::test(flavor = "multi_thread")]
async fn huge_hello_before_auth_is_refused() {
    let r = rig();
    let conn = r.raw().await;
    let (mut send, _recv) = conn.open_bi().await.unwrap();
    let hello = Message::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
        device_id: r.viewer_id.device_id().as_u32(),
        device_name: "ح".repeat(100 * 1024),
        role: Role::Viewer,
    });
    // The host may close while we are still writing.
    let _ = send.write_all(&encode_frame(&hello).unwrap()).await;
    let _ = send
        .write_all(&encode_frame(&Message::AuthRequest(AuthRequest::AcceptPrompt)).unwrap())
        .await;
    let start = Instant::now();
    assert_eq!(closed_by_host(&conn).await, Some(CLOSE_PROTOCOL));
    // Refused on the length prefix, not by the 10 s handshake timeout.
    assert!(start.elapsed() < Duration::from_secs(5));
    no_pending(&r.host).await;
}

/// The self-reported name is cut to a sane length and stripped of control characters before
/// the host application sees it; Arabic letters and bidi marks survive.
#[tokio::test(flavor = "multi_thread")]
async fn peer_names_are_bounded_and_control_free() {
    let r = rig();
    let host = r.host.clone();
    let seen = tokio::spawn(async move {
        let p = host.accept().await.unwrap();
        let name = p.peer().name.clone();
        p.reject(RejectReason::UserDeclined).await;
        name
    });
    let conn = r.raw().await;
    let (mut send, recv) = conn.open_bi().await.unwrap();
    let mut rx = Reader::new(recv);
    let long = format!("مكتب\u{200F}-1\n\r\0\u{7}{}", "ب".repeat(500));
    write(
        &mut send,
        &Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: r.viewer_id.device_id().as_u32(),
            device_name: long,
            role: Role::Viewer,
        }),
    )
    .await;
    assert!(matches!(rx.next().await, Some(Message::Hello(_))));
    write(&mut send, &Message::AuthRequest(AuthRequest::AcceptPrompt)).await;
    let name = timeout(T, seen).await.unwrap().unwrap();
    assert_eq!(name.chars().count(), tlp_desk_net::MAX_PEER_NAME_CHARS);
    assert!(name.starts_with("مكتب\u{200F}-1ببب"), "{name:?}");
    assert!(!name.chars().any(char::is_control), "{name:?}");
}

/// `AuthRequest` before `Hello` is a protocol error.
#[tokio::test(flavor = "multi_thread")]
async fn auth_request_before_hello_is_a_protocol_error() {
    let r = rig();
    let conn = r.raw().await;
    let (mut send, _recv) = conn.open_bi().await.unwrap();
    write(&mut send, &Message::AuthRequest(AuthRequest::AcceptPrompt)).await;
    assert_eq!(closed_by_host(&conn).await, Some(CLOSE_PROTOCOL));
    no_pending(&r.host).await;
}

/// After `Hello`, anything but an `AuthRequest` is a protocol error (here: a chat message and
/// a video frame on the control stream).
#[tokio::test(flavor = "multi_thread")]
async fn wrong_message_after_hello_is_a_protocol_error() {
    let r = rig();
    for msg in [
        Message::ChatMessage {
            id: 1,
            text: "مرحبا".into(),
            sent_unix_ms: 0,
        },
        Message::VideoFrame(VideoFrame {
            seq: 0,
            timestamp_us: 0,
            width: 1,
            height: 1,
            codec: Codec::H264,
            keyframe: true,
            data: vec![0; 16],
        }),
        r.hello(),
    ] {
        let (conn, mut send, _rx) = r.raw_after_hello().await;
        write(&mut send, &msg).await;
        assert_eq!(closed_by_host(&conn).await, Some(CLOSE_PROTOCOL), "{msg:?}");
    }
    no_pending(&r.host).await;
}

/// A peer claiming to be a host is not let in.
#[tokio::test(flavor = "multi_thread")]
async fn hello_claiming_the_host_role_is_rejected() {
    let r = rig();
    let conn = r.raw().await;
    let (mut send, recv) = conn.open_bi().await.unwrap();
    let mut rx = Reader::new(recv);
    write(
        &mut send,
        &Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: r.viewer_id.device_id().as_u32(),
            device_name: "fake host".into(),
            role: Role::Host,
        }),
    )
    .await;
    assert_eq!(
        rx.next().await,
        Some(Message::AuthResult(AuthResult::Rejected {
            reason: RejectReason::NotAllowed
        }))
    );
    no_pending(&r.host).await;
}

/// Slowloris: connections that stall in the handshake (TLS only, a stream without a byte,
/// half a length prefix, half a frame) are closed after the handshake timeout and never delay
/// an honest viewer.
#[tokio::test(flavor = "multi_thread")]
async fn stalled_handshakes_time_out_without_blocking_honest_viewers() {
    let r = rig_with(EndpointConfig {
        handshake_timeout: Duration::from_millis(800),
        ..cfg(true)
    });
    let hello = encode_frame(&r.hello()).unwrap();
    let mut stalled = Vec::new();
    for i in 0..24 {
        let conn = r.raw().await;
        let keep = match i % 4 {
            0 => None,
            n => {
                let (mut send, recv) = conn.open_bi().await.unwrap();
                let upto = match n {
                    1 => 0,
                    2 => 2,
                    _ => hello.len() - 1,
                };
                send.write_all(&hello[..upto]).await.unwrap();
                Some((send, recv))
            }
        };
        stalled.push((conn, keep));
    }
    // An honest viewer gets through while all of them hang.
    let (_h, _v) = r.honest_session().await;
    for (conn, _keep) in &stalled {
        closed_by_host(conn).await;
    }
}

/// Connect/disconnect storm: peers that vanish after TLS, after `Hello`, or while waiting for
/// the prompt. The host survives, honest viewers still connect, and every connection is
/// released afterwards. A client that vanishes mid-TLS sends no close, so the host holds
/// that connection until its handshake timeout (3 s here) or the idle timeout — bounded, not
/// leaked. The storm peers themselves tolerate being dropped (a loaded machine may hit the
/// handshake timeout); only the host's behaviour is asserted.
#[tokio::test(flavor = "multi_thread")]
async fn connect_disconnect_storm_leaks_nothing() {
    let r = Arc::new(rig_with(EndpointConfig {
        handshake_timeout: Duration::from_secs(3),
        ..cfg(true)
    }));
    let pin = Pin::DeviceId(r.host.device_id());
    let hello = encode_frame(&r.hello()).unwrap();
    let prompt = encode_frame(&Message::AuthRequest(AuthRequest::AcceptPrompt)).unwrap();
    // Host application: rejects everything it sees during the storm.
    let host = r.host.clone();
    let app = tokio::spawn(async move {
        let mut seen = 0usize;
        while let Some(p) = host.accept().await {
            seen += 1;
            tokio::spawn(p.reject(RejectReason::UserDeclined));
        }
        seen
    });
    let mut tasks = Vec::new();
    for i in 0..60 {
        let r = r.clone();
        let (hello, prompt) = (hello.clone(), prompt.clone());
        tasks.push(tokio::spawn(async move {
            match i % 3 {
                0 => {
                    if let Ok(conn) = r.viewer.dial_raw(r.addr, pin).await {
                        conn.close(0u32.into(), b"gone");
                    }
                }
                1 => {
                    if let Ok(conn) = r.viewer.dial_raw(r.addr, pin).await {
                        if let Ok((mut send, _recv)) = conn.open_bi().await {
                            let _ = send.write_all(&hello).await;
                            if i % 2 == 0 {
                                let _ = send.write_all(&prompt).await;
                            }
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                        conn.close(0u32.into(), b"gone");
                    }
                }
                _ => {
                    // A full client that gives up while the prompt is open.
                    let _ = timeout(
                        Duration::from_millis(20),
                        r.viewer.connect(
                            r.addr,
                            Some(r.host.device_id()),
                            AuthMethod::AcceptPrompt,
                        ),
                    )
                    .await;
                }
            }
        }));
    }
    for t in tasks {
        timeout(T, t).await.unwrap().unwrap();
    }
    app.abort();
    // Honest viewer afterwards.
    let (h, v) = r.honest_session().await;
    v.send(&Message::RequestKeyframe).await.unwrap();
    assert_eq!(
        timeout(T, h.recv()).await.unwrap().unwrap(),
        Message::RequestKeyframe
    );
    drop((h, v));
    // Most connections close within the handshake timeout; one whose peer vanished without a
    // close frame is held until the 15 s QUIC idle timeout. Bounded either way.
    drained(&r.host, "host", Duration::from_secs(20)).await;
    drained(&r.viewer, "viewer", T).await;
}

/// Password brute force with malformed SPAKE2 messages: each one counts as a failed attempt
/// (`WrongCredentials`), and after five the peer is locked out (`RateLimited`) before any
/// PAKE work is done. Logged on the host (see `tracing` output with RUST_LOG=info).
#[tokio::test(flavor = "multi_thread")]
async fn password_brute_force_hits_the_rate_limiter() {
    let r = rig();
    let host = r.host.clone();
    let host_fp = r.host.fingerprint();
    let verifier = PasswordVerifier::derive("كلمة سر طويلة 2026", &host_fp).unwrap();
    let app = tokio::spawn(async move {
        let mut results = Vec::new();
        while let Some(p) = host.accept().await {
            assert_eq!(p.auth_kind(), AuthKind::Password);
            results.push(p.verify_password(&verifier, Permissions::ALL).await.err());
        }
        results
    });
    let msgs: [Vec<u8>; 7] = [
        vec![],
        vec![0],
        vec![0xFF; 33],
        vec![0x41; 33],
        (0..=255).collect(),
        vec![7; 4000],
        vec![0x42; 33],
    ];
    let mut replies = Vec::new();
    for m in msgs {
        let (conn, mut send, mut rx) = r.raw_after_hello().await;
        write(
            &mut send,
            &Message::AuthRequest(AuthRequest::Password { spake2_msg: m }),
        )
        .await;
        replies.push(rx.next().await);
        conn.close(0u32.into(), b"");
    }
    let rejected = |reason| Some(Message::AuthResult(AuthResult::Rejected { reason }));
    assert_eq!(
        replies[..5],
        vec![rejected(RejectReason::WrongCredentials); 5][..]
    );
    assert_eq!(
        replies[5..],
        vec![rejected(RejectReason::RateLimited); 2][..]
    );
    // An honest viewer from the same IP is locked out too, even with the right password.
    let res = r
        .viewer
        .connect(
            r.addr,
            Some(r.host.device_id()),
            AuthMethod::Password("كلمة سر طويلة 2026".into()),
        )
        .await;
    assert!(
        matches!(res, Err(NetError::Rejected(RejectReason::RateLimited))),
        "{res:?}"
    );
    app.abort();
}

// ------------------------------------------------------------------------------------------
// Established session
// ------------------------------------------------------------------------------------------

/// Video streams are host -> viewer only: a viewer opening one (uni or bidi) is refused, and
/// the session carries on.
#[tokio::test(flavor = "multi_thread")]
async fn video_streams_from_the_viewer_are_refused() {
    let r = rig();
    let (h, v) = r.honest_session().await;
    let header = StreamHeader::new(StreamKind::Video, 1).encode();
    let mut uni = v.open_raw_uni().await.unwrap();
    uni.write_all(&header).await.unwrap();
    uni.write_all(&encode_frame(&Message::RequestKeyframe).unwrap())
        .await
        .unwrap();
    assert_eq!(
        timeout(T, uni.stopped()).await.unwrap().unwrap(),
        Some(STREAM_REFUSED.into())
    );
    let (mut bi, _recv) = v.open_raw_bi().await.unwrap();
    bi.write_all(&header).await.unwrap();
    assert_eq!(
        timeout(T, bi.stopped()).await.unwrap().unwrap(),
        Some(STREAM_REFUSED.into())
    );
    v.send(&Message::RequestKeyframe).await.unwrap();
    assert_eq!(
        timeout(T, h.recv()).await.unwrap().unwrap(),
        Message::RequestKeyframe
    );
}

/// Unknown kinds on bidi streams and truncated headers are refused too.
#[tokio::test(flavor = "multi_thread")]
async fn malformed_side_stream_headers_are_refused() {
    let r = rig();
    let (h, v) = r.honest_session().await;
    let mut header = StreamHeader::new(StreamKind::Tunnel, 9).encode();
    header[0] = 0x80;
    let (mut bi, _recv) = v.open_raw_bi().await.unwrap();
    bi.write_all(&header).await.unwrap();
    assert_eq!(
        timeout(T, bi.stopped()).await.unwrap().unwrap(),
        Some(STREAM_REFUSED.into())
    );
    // Four bytes of a nine-byte header, then end of stream.
    let mut uni = v.open_raw_uni().await.unwrap();
    uni.write_all(&header[..4]).await.unwrap();
    uni.finish().unwrap();
    let stopped = timeout(T, uni.stopped()).await.unwrap();
    // Either refused, or the peer read to the end before refusing (both are clean).
    assert!(
        matches!(stopped, Ok(Some(c)) if c == STREAM_REFUSED.into()) || matches!(stopped, Ok(None)),
        "{stopped:?}"
    );
    assert!(timeout(
        Duration::from_millis(200),
        h.accept_side_stream(StreamKind::Tunnel)
    )
    .await
    .is_err());
}

/// A flood of side streams nobody accepts fills that kind's queue (64); the rest wait (back-
/// pressure, bounded by the QUIC stream limit) and are refused once the wait is over, instead of
/// piling up.
#[tokio::test(flavor = "multi_thread")]
async fn side_stream_flood_is_bounded() {
    let r = rig();
    let (h, v) = r.honest_session().await;
    let mut waits = tokio::task::JoinSet::new();
    for id in 0..100u64 {
        let mut s = v.open_uni(StreamKind::FileData, id).await.unwrap();
        s.write_all(b"x").await.unwrap();
        // Queued streams are never stopped: they time out here, which is the point.
        waits.spawn(async move {
            match timeout(Duration::from_secs(15), s.stopped()).await {
                Ok(Ok(Some(code))) => {
                    assert_eq!(code, STREAM_REFUSED.into());
                    true
                }
                _ => false,
            }
        });
    }
    let mut refused = 0;
    while let Some(r) = waits.join_next().await {
        refused += usize::from(r.unwrap());
    }
    assert_eq!(refused, 100 - 64);
    // The queued ones are still deliverable.
    let first = timeout(T, h.accept_side_stream(StreamKind::FileData))
        .await
        .unwrap()
        .unwrap();
    assert!(first.id < 100);
}

/// Regression: a feature that is slow to accept (busy machine, burst of small files) must not
/// lose streams. 150 streams opened at once while nobody accepts are all delivered once the
/// consumer catches up; none is refused (this used to hang `many_small_files` under load).
#[tokio::test(flavor = "multi_thread")]
async fn slow_consumer_receives_every_stream() {
    let r = rig();
    let (h, v) = r.honest_session().await;
    let mut sends = Vec::new();
    for id in 0..150u64 {
        let mut s = v.open_uni(StreamKind::FileData, id).await.unwrap();
        s.write_all(b"x").await.unwrap();
        s.finish().unwrap();
        sends.push(s);
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..150 {
        let got = timeout(T, h.accept_side_stream(StreamKind::FileData))
            .await
            .unwrap()
            .unwrap();
        seen.insert(got.id);
    }
    assert_eq!(seen.len(), 150);
    for s in sends {
        assert_eq!(s.stopped().await.unwrap(), None, "a stream was refused");
    }
}

/// Half-open side streams (no header ever sent) do not block the control stream or other
/// side streams, and are refused after the header timeout.
#[tokio::test(flavor = "multi_thread")]
async fn half_open_side_streams_do_not_block_the_session() {
    let r = rig();
    let (h, v) = r.honest_session().await;
    let mut hanging = Vec::new();
    for i in 0..40 {
        let mut s = v.open_raw_uni().await.unwrap();
        // Open the stream on the wire with 0..8 header bytes, then stall.
        s.write_all(&StreamHeader::new(StreamKind::Audio, i).encode()[..(i % 9) as usize])
            .await
            .unwrap();
        hanging.push(s);
    }
    // Control and a proper side stream still work at once.
    v.send(&Message::RequestKeyframe).await.unwrap();
    assert_eq!(
        timeout(T, h.recv()).await.unwrap().unwrap(),
        Message::RequestKeyframe
    );
    let mut ok = v.open_uni(StreamKind::Clipboard, 5).await.unwrap();
    ok.write_all("نص الحافظة".as_bytes()).await.unwrap();
    ok.finish().unwrap();
    let got = timeout(T, h.accept_side_stream(StreamKind::Clipboard))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got.id, 5);
    // The stalled ones are refused once the 10 s header timeout passes (streams with zero
    // bytes are never seen by the peer, so only those that sent something are checked).
    for (i, s) in hanging.iter_mut().enumerate() {
        if i % 9 == 0 {
            continue;
        }
        let r = timeout(Duration::from_secs(15), s.stopped()).await;
        assert!(matches!(r, Ok(Ok(Some(_)))), "stream {i}: {r:?}");
    }
}

/// Garbage on video streams (bad body, oversized prefix, wrong message) never breaks the
/// viewer: the next keyframe is delivered.
#[tokio::test(flavor = "multi_thread")]
async fn garbage_video_streams_do_not_break_the_viewer() {
    let r = rig();
    let (h, v) = r.honest_session().await;
    let header = StreamHeader::new(StreamKind::Video, 0).encode();
    let bodies: [Vec<u8>; 4] = [
        vec![0xFF; 32],
        u32::MAX.to_le_bytes().to_vec(),
        encode_frame(&Message::RequestKeyframe).unwrap(),
        {
            let mut b = 1000u32.to_le_bytes().to_vec();
            b.extend_from_slice(&[3; 10]);
            b
        },
    ];
    for body in bodies {
        let mut s = h.open_raw_uni().await.unwrap();
        s.write_all(&header).await.unwrap();
        s.write_all(&body).await.unwrap();
        s.finish().unwrap();
    }
    let kf = VideoFrame {
        seq: 1,
        timestamp_us: 1,
        width: 64,
        height: 64,
        codec: Codec::H264,
        keyframe: true,
        data: vec![1, 2, 3],
    };
    // Resend until delivered (the garbage may have been counted as a gap first).
    let got = timeout(T, async {
        let mut seq = 1;
        loop {
            let _ = h.send_video(VideoFrame { seq, ..kf.clone() });
            if let Ok(Ok(f)) = timeout(Duration::from_millis(200), v.recv_video()).await {
                return f;
            }
            seq += 1;
        }
    })
    .await
    .unwrap();
    assert!(got.keyframe);
    assert_eq!(got.data, vec![1, 2, 3]);
}
