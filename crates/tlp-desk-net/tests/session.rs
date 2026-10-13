//! In-process loopback tests: two peers, real QUIC + TLS + SPAKE2.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tlp_desk_crypto::{
    DeviceId, HostKey, Identity, KnownHosts, OneTimeCode, PasswordVerifier, TrustCheck,
};
use tlp_desk_net::{
    AllowList, AuthKind, AuthMethod, Endpoint, EndpointConfig, NetError, Pin, Session,
    VideoSendStatus,
};
use tlp_desk_proto::{
    ByeReason, ClipboardData, Codec, InputEvent, Message, Permissions, RejectReason, Role,
    VideoFrame,
};
use tokio::time::timeout;

const T: Duration = Duration::from_secs(10);

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tlp_desk_net=info".into()),
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

struct Pair {
    host: Arc<Endpoint>,
    viewer: Arc<Endpoint>,
    addr: SocketAddr,
}

fn pair_with(host_cfg: EndpointConfig) -> Pair {
    init_tracing();
    let host = Arc::new(Endpoint::bind(Arc::new(Identity::generate().unwrap()), host_cfg).unwrap());
    let viewer =
        Arc::new(Endpoint::bind(Arc::new(Identity::generate().unwrap()), cfg(false)).unwrap());
    let addr = host.local_addr().unwrap();
    Pair { host, viewer, addr }
}

fn pair() -> Pair {
    pair_with(cfg(true))
}

fn frame(seq: u64, keyframe: bool, len: usize) -> VideoFrame {
    VideoFrame {
        seq,
        timestamp_us: seq * 16_666,
        width: 1920,
        height: 1080,
        codec: Codec::H264,
        keyframe,
        data: vec![(seq % 251) as u8; len],
    }
}

/// Host accepts the prompt; returns (host session, viewer session).
async fn connect_accept(p: &Pair) -> (Session, Session) {
    let host = p.host.clone();
    let viewer_id = p.viewer.device_id();
    let h = tokio::spawn(async move {
        let pending = host.accept().await.expect("pending session");
        assert_eq!(pending.auth_kind(), AuthKind::AcceptPrompt);
        assert_eq!(pending.peer().device_id, viewer_id);
        assert_eq!(pending.peer().name, "viewer-pc");
        pending
            .accept(Permissions::VIEW | Permissions::CONTROL)
            .await
            .unwrap()
    });
    let v = timeout(
        T,
        p.viewer
            .connect(p.addr, Some(p.host.device_id()), AuthMethod::AcceptPrompt),
    )
    .await
    .unwrap()
    .unwrap();
    let h = timeout(T, h).await.unwrap().unwrap();
    (h, v)
}

#[tokio::test(flavor = "multi_thread")]
async fn two_peers_connect_with_accept_prompt() {
    let p = pair();
    let (h, v) = connect_accept(&p).await;
    assert_eq!(h.role(), Role::Host);
    assert_eq!(v.role(), Role::Viewer);
    assert_eq!(v.peer().device_id, p.host.device_id());
    assert_eq!(v.peer().fingerprint, p.host.fingerprint());
    assert_eq!(v.peer().name, "host-pc");
    assert_eq!(h.peer().fingerprint, p.viewer.fingerprint());
    assert_eq!(
        v.initial_permissions(),
        Permissions::VIEW | Permissions::CONTROL
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn awaiting_accept_hook_fires_before_the_host_decides() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let p = pair();
    let calls = Arc::new(AtomicU32::new(0));
    let (decide_tx, decide_rx) = tokio::sync::oneshot::channel::<()>();
    let host = p.host.clone();
    let seen = calls.clone();
    let h = tokio::spawn(async move {
        let pending = host.accept().await.expect("pending session");
        // The viewer must already know it is waiting for the human.
        decide_rx.await.unwrap();
        assert_eq!(seen.load(Ordering::SeqCst), 1);
        pending.accept(Permissions::VIEW).await.unwrap()
    });
    let hook_calls = calls.clone();
    let hook: tlp_desk_net::AwaitingAcceptFn = Arc::new(move || {
        hook_calls.fetch_add(1, Ordering::SeqCst);
    });
    let viewer = p.viewer.clone();
    let addr = p.addr;
    let v = tokio::spawn(async move {
        viewer
            .connect_pinned_with_progress(addr, Pin::Tofu, AuthMethod::AcceptPrompt, Some(hook))
            .await
    });
    timeout(T, async {
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("hook not called");
    decide_tx.send(()).unwrap();
    let v = timeout(T, v).await.unwrap().unwrap().unwrap();
    let _h = timeout(T, h).await.unwrap().unwrap();
    assert_eq!(v.initial_permissions(), Permissions::VIEW);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Code auth never asks the human, so the hook must not fire.
    let p = pair();
    let code = OneTimeCode::from_fixed("123456");
    let host = p.host.clone();
    let host_code = code.clone();
    let h = tokio::spawn(async move {
        let pending = host.accept().await.unwrap();
        // Keep the host session alive until the viewer has its result.
        pending.verify_code(&host_code, Permissions::VIEW).await
    });
    let none = Arc::new(AtomicU32::new(0));
    let n2 = none.clone();
    let hook: tlp_desk_net::AwaitingAcceptFn = Arc::new(move || {
        n2.fetch_add(1, Ordering::SeqCst);
    });
    timeout(
        T,
        p.viewer.connect_pinned_with_progress(
            p.addr,
            Pin::Tofu,
            AuthMethod::OneTimeCode("123 456".into()),
            Some(hook),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let _host_session = timeout(T, h).await.unwrap().unwrap().unwrap();
    assert_eq!(none.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn accept_prompt_reject_reaches_viewer() {
    let p = pair();
    let host = p.host.clone();
    tokio::spawn(async move {
        let pending = host.accept().await.unwrap();
        pending.reject(RejectReason::UserDeclined).await;
    });
    let err = timeout(T, p.viewer.connect(p.addr, None, AuthMethod::AcceptPrompt))
        .await
        .unwrap()
        .unwrap_err();
    assert!(
        matches!(err, NetError::Rejected(RejectReason::UserDeclined)),
        "{err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn frames_flow_on_control_and_video_streams() {
    let p = pair();
    let (h, v) = connect_accept(&p).await;

    // control: viewer -> host
    let input = Message::InputEvent(InputEvent::MouseMove { x: 100, y: 200 });
    v.send(&input).await.unwrap();
    assert_eq!(timeout(T, h.recv()).await.unwrap().unwrap(), input);
    // control: host -> viewer
    let clip = Message::ClipboardData(ClipboardData::Text("مرحبا".into()));
    h.send(&clip).await.unwrap();
    assert_eq!(timeout(T, v.recv()).await.unwrap().unwrap(), clip);

    // video: host -> viewer, one stream per frame, including a large keyframe
    for seq in 0..3u64 {
        let f = frame(seq, seq == 0, if seq == 0 { 600_000 } else { 20_000 });
        assert_eq!(h.send_video(f).unwrap(), VideoSendStatus::Sent);
        let got = timeout(T, v.recv_video()).await.unwrap().unwrap();
        assert_eq!(got.seq, seq);
        assert_eq!(got.keyframe, seq == 0);
        assert_eq!(got.data.len(), if seq == 0 { 600_000 } else { 20_000 });
    }
    // role checks
    assert!(v.send_video(frame(9, true, 1)).is_err());
    assert!(h.recv_video().await.is_err());

    // graceful close
    v.close(ByeReason::UserClosed).await;
    match timeout(T, h.recv()).await.unwrap() {
        Ok(Message::Bye(b)) => assert_eq!(b.reason, ByeReason::UserClosed),
        other => panic!("expected Bye, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn host_drops_video_instead_of_queueing() {
    let p = pair();
    let (h, _v) = connect_accept(&p).await;
    assert_eq!(
        h.send_video(frame(0, true, 1)).unwrap(),
        VideoSendStatus::Sent
    );
    // Burst of big P-frames faster than they can be acknowledged.
    let mut statuses = Vec::new();
    for seq in 1..=12 {
        statuses.push(h.send_video(frame(seq, false, 2_000_000)).unwrap());
    }
    let first_drop = statuses
        .iter()
        .position(|s| *s == VideoSendStatus::Dropped)
        .expect("some frame must be dropped under backpressure");
    // After the first drop, every non-key frame is dropped until a keyframe.
    assert!(statuses[first_drop..]
        .iter()
        .all(|s| *s == VideoSendStatus::Dropped));
    // Once the backlog drains, a keyframe resumes the stream.
    timeout(T, async {
        while h.video_in_flight() > 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        h.send_video(frame(13, false, 10)).unwrap(),
        VideoSendStatus::Dropped
    );
    assert_eq!(
        h.send_video(frame(14, true, 10)).unwrap(),
        VideoSendStatus::Sent
    );
    assert_eq!(
        h.send_video(frame(15, false, 10)).unwrap(),
        VideoSendStatus::Sent
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn viewer_skips_to_keyframe_and_requests_one_after_loss() {
    let p = pair();
    let (h, v) = connect_accept(&p).await;
    h.send_video(frame(0, true, 100)).unwrap();
    assert_eq!(timeout(T, v.recv_video()).await.unwrap().unwrap().seq, 0);
    // seq 1 is "lost" (never sent); seq 2 must not be handed to the decoder.
    tokio::time::sleep(Duration::from_millis(50)).await;
    h.send_video(frame(2, false, 100)).unwrap();
    let vv = Arc::new(v);
    let reader = {
        let vv = vv.clone();
        tokio::spawn(async move { vv.recv_video().await })
    };
    // Host gets a keyframe request on the control stream.
    match timeout(T, h.recv()).await.unwrap().unwrap() {
        Message::RequestKeyframe => {}
        other => panic!("expected RequestKeyframe, got {other:?}"),
    }
    h.send_video(frame(3, true, 100)).unwrap();
    let got = timeout(T, reader).await.unwrap().unwrap().unwrap();
    assert_eq!(got.seq, 3);
    assert!(got.keyframe);
}

#[tokio::test(flavor = "multi_thread")]
async fn correct_one_time_code_succeeds_and_rotates() {
    let p = pair();
    let code = OneTimeCode::new();
    let typed = code.display(); // "123 456" form, as a user would type it
    let before = code.current();
    let host = p.host.clone();
    let host_code = code.clone();
    let h = tokio::spawn(async move {
        let pending = host.accept().await.unwrap();
        assert_eq!(pending.auth_kind(), AuthKind::OneTimeCode);
        pending.verify_code(&host_code, Permissions::ALL).await
    });
    let v = timeout(
        T,
        p.viewer.connect(
            p.addr,
            Some(p.host.device_id()),
            AuthMethod::OneTimeCode(typed),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let h = timeout(T, h).await.unwrap().unwrap().unwrap();
    assert_eq!(v.initial_permissions(), Permissions::ALL);
    assert_eq!(h.peer().device_id, p.viewer.device_id());
    assert_ne!(code.current(), before, "code must rotate after use");
    // session works
    v.send(&Message::InputEvent(InputEvent::ReleaseAll))
        .await
        .unwrap();
    assert_eq!(
        timeout(T, h.recv()).await.unwrap().unwrap(),
        Message::InputEvent(InputEvent::ReleaseAll)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_code_fails_on_both_sides() {
    let p = pair();
    let code = OneTimeCode::from_fixed("111111");
    let host = p.host.clone();
    let host_code = code.clone();
    let h = tokio::spawn(async move {
        let pending = host.accept().await.unwrap();
        pending.verify_code(&host_code, Permissions::ALL).await
    });
    let v = timeout(
        T,
        p.viewer.connect(
            p.addr,
            Some(p.host.device_id()),
            AuthMethod::OneTimeCode("222222".into()),
        ),
    )
    .await
    .unwrap();
    assert!(
        matches!(v, Err(NetError::Rejected(RejectReason::WrongCredentials))),
        "viewer: {v:?}"
    );
    let h = timeout(T, h).await.unwrap().unwrap();
    assert!(matches!(h, Err(NetError::AuthFailed)), "host: {h:?}");
    assert_eq!(code.current(), "111111", "one failure does not rotate yet");
}

#[tokio::test(flavor = "multi_thread")]
async fn password_auth_right_and_wrong() {
    let p = pair();
    let verifier = PasswordVerifier::derive("correct horse", &p.host.fingerprint()).unwrap();
    let host = p.host.clone();
    let hv = verifier.clone();
    let h = tokio::spawn(async move {
        let mut results = Vec::new();
        for _ in 0..2 {
            let pending = host.accept().await.unwrap();
            assert_eq!(pending.auth_kind(), AuthKind::Password);
            // keep the session alive so the viewer receives `Accepted`
            results.push(pending.verify_password(&hv, Permissions::VIEW).await);
        }
        results
    });
    let bad = p
        .viewer
        .connect(p.addr, None, AuthMethod::Password("wrong".into()))
        .await;
    assert!(matches!(
        bad,
        Err(NetError::Rejected(RejectReason::WrongCredentials))
    ));
    let good = p
        .viewer
        .connect(p.addr, None, AuthMethod::Password("correct horse".into()))
        .await;
    assert!(good.is_ok(), "{good:?}");
    let results = timeout(T, h).await.unwrap().unwrap();
    assert!(matches!(results[0], Err(NetError::AuthFailed)));
    assert!(results[1].is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn host_with_different_key_than_dialed_id_is_rejected() {
    let p = pair();
    // An ID that is not the host's.
    let wrong = DeviceId::new(if p.host.device_id().as_u32() == 123_456_789 {
        987_654_321
    } else {
        123_456_789
    })
    .unwrap();
    let err = timeout(
        T,
        p.viewer
            .connect(p.addr, Some(wrong), AuthMethod::AcceptPrompt),
    )
    .await
    .unwrap()
    .unwrap_err();
    match err {
        NetError::PinMismatch {
            actual_id,
            actual_fp,
            ..
        } => {
            assert_eq!(actual_id, p.host.device_id());
            assert_eq!(actual_fp, p.host.fingerprint());
        }
        other => panic!("expected PinMismatch, got {other:?}"),
    }
    // Full-fingerprint pin mismatch is rejected too.
    let other_fp = Identity::generate().unwrap().fingerprint();
    let err = p
        .viewer
        .connect_pinned(p.addr, Pin::Fingerprint(other_fp), AuthMethod::AcceptPrompt)
        .await
        .unwrap_err();
    assert!(matches!(err, NetError::PinMismatch { .. }), "{err:?}");
    // The host never saw a pending session (TLS failed before Hello).
    assert!(timeout(Duration::from_millis(300), p.host.accept())
        .await
        .is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn tofu_reports_host_key_for_known_hosts() {
    let p = pair();
    let host = p.host.clone();
    tokio::spawn(async move {
        let mut keep = Vec::new();
        while let Some(pending) = host.accept().await {
            keep.push(pending.accept(Permissions::VIEW).await);
        }
    });
    let mut kh = KnownHosts::in_memory();
    let key = HostKey::Addr(p.addr.ip().to_string());
    let s = p
        .viewer
        .connect(p.addr, None, AuthMethod::AcceptPrompt)
        .await
        .unwrap();
    assert_eq!(kh.check(&key, &s.peer().fingerprint), TrustCheck::Unknown);
    kh.remember(key.clone(), s.peer().fingerprint);
    // second time: pin the full fingerprint from the store
    let fp = kh.fingerprint_for(&key).unwrap();
    let s2 = p
        .viewer
        .connect_pinned(p.addr, Pin::Fingerprint(fp), AuthMethod::AcceptPrompt)
        .await
        .unwrap();
    assert_eq!(kh.check(&key, &s2.peer().fingerprint), TrustCheck::Trusted);
}

#[tokio::test(flavor = "multi_thread")]
async fn peer_outside_allow_list_is_refused() {
    let mut c = cfg(true);
    c.allow_list = AllowList::parse(&["10.0.0.0/8"]).unwrap();
    let p = pair_with(c);
    let res = timeout(T, p.viewer.connect(p.addr, None, AuthMethod::AcceptPrompt))
        .await
        .unwrap();
    assert!(res.is_err(), "loopback must be refused: {res:?}");
    assert!(timeout(Duration::from_millis(300), p.host.accept())
        .await
        .is_err());
    // Allowing it at runtime works.
    p.host.set_allow_list(AllowList::default());
    let host = p.host.clone();
    let h = tokio::spawn(async move {
        let pending = host.accept().await.unwrap();
        pending.accept(Permissions::VIEW).await
    });
    assert!(p
        .viewer
        .connect(p.addr, None, AuthMethod::AcceptPrompt)
        .await
        .is_ok());
    assert!(h.await.unwrap().is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn repeated_wrong_codes_trigger_lockout() {
    let p = pair();
    let code = OneTimeCode::from_fixed("111111");
    let host = p.host.clone();
    let hc = code.clone();
    tokio::spawn(async move {
        while let Some(pending) = host.accept().await {
            let _ = pending.verify_code(&hc, Permissions::ALL).await;
        }
    });
    for i in 0..5 {
        let r = p
            .viewer
            .connect(p.addr, None, AuthMethod::OneTimeCode("000000".into()))
            .await;
        assert!(
            matches!(r, Err(NetError::Rejected(RejectReason::WrongCredentials))),
            "attempt {i}: {r:?}"
        );
    }
    // 6th attempt within the minute: locked out, even with the right code.
    for _ in 0..2 {
        let r = p
            .viewer
            .connect(p.addr, None, AuthMethod::OneTimeCode(code.current()))
            .await;
        assert!(
            matches!(r, Err(NetError::Rejected(RejectReason::RateLimited))),
            "{r:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn second_endpoint_on_a_taken_port_fails_with_addr_in_use() {
    init_tracing();
    let any = |port| EndpointConfig {
        bind: SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), port),
        ..cfg(true)
    };
    let first = Endpoint::bind(Arc::new(Identity::generate().unwrap()), any(0)).unwrap();
    let port = first.local_addr().unwrap().port();
    let err = Endpoint::bind(Arc::new(Identity::generate().unwrap()), any(port)).unwrap_err();
    assert!(
        matches!(&err, NetError::Io(e) if e.kind() == std::io::ErrorKind::AddrInUse),
        "two endpoints must not share a port: {err:?}"
    );
}
