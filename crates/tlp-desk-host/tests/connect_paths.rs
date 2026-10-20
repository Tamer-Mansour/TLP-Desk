//! How a viewer picks among a device's addresses, end to end over QUIC on loopback.
//!
//! Regressions for two field bugs:
//! * "TLP-Desk says I am connecting to my own device": a peer's address list contained an
//!   address that was really ours (a virtual adapter both PCs have, a stale DHCP lease), and
//!   dialing it reached this PC.
//! * "the device is listed but connecting fails until Wi-Fi is restarted": the viewer dialed
//!   a dead address first and waited a whole QUIC idle timeout on it before trying the next.

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tlp_desk_crypto::Identity;
use tlp_desk_host::{Host, HostEvent};
use tlp_desk_net::{DiscoveredDevice, Endpoint, EndpointConfig};
use tlp_desk_proto::Permissions;
use tlp_desk_testkit::pair::{self, TestPair, VIEWER_NAME};
use tlp_desk_testkit::{fakes, wait};
use tlp_desk_viewer::{AuthMethod, ConnectTarget, Viewer, ViewerError};

/// A loopback endpoint with a short handshake timeout, so dead addresses fail fast here.
fn quick_endpoint(listen: bool, name: &str) -> Arc<Endpoint> {
    let ep = Endpoint::bind(
        Arc::new(Identity::generate().unwrap()),
        EndpointConfig {
            bind: SocketAddr::from(([127, 0, 0, 1], 0)),
            listen,
            device_name: name.to_owned(),
            handshake_timeout: Duration::from_secs(2),
            ..EndpointConfig::default()
        },
    )
    .unwrap();
    Arc::new(ep)
}

/// A UDP socket that swallows everything: a host that went away without a goodbye.
fn black_hole() -> (UdpSocket, SocketAddr) {
    let s = UdpSocket::bind("127.0.0.1:0").unwrap();
    let a = s.local_addr().unwrap();
    (s, a)
}

fn device(ep: &Endpoint, addrs: Vec<SocketAddr>) -> DiscoveredDevice {
    DiscoveredDevice {
        device_id: ep.device_id(),
        name: "مكتب".into(),
        addrs,
        protocol_version: tlp_desk_proto::PROTOCOL_VERSION,
        app_version: None,
        via_mdns: true,
        via_broadcast: false,
        last_seen: Instant::now(),
    }
}

/// A host that also accepts connections (like every GUI install) and a viewer on it.
fn listening_viewer() -> (Arc<Endpoint>, Viewer) {
    let ep = quick_endpoint(true, VIEWER_NAME);
    let viewer = Viewer::new(ep.clone(), pair::viewer_config()).unwrap();
    (ep, viewer)
}

/// Starts a host with fakes on a fresh loopback endpoint.
async fn host() -> (Arc<Endpoint>, Host, tlp_desk_host::HostEvents) {
    let alive = Arc::new(fakes::Alive::default());
    let cfg = pair::host_config(
        &alive,
        &fakes::Recorded::default(),
        &Arc::new(fakes::FakeLocalActivity::default()),
    );
    let ep = quick_endpoint(true, pair::HOST_NAME);
    let (h, ev) = Host::start(ep.clone(), cfg).await.unwrap();
    (ep, h, ev)
}

async fn accept_next(host: &Host, events: &mut tlp_desk_host::HostEvents) {
    let req = match wait::host_event(events, |e| matches!(e, HostEvent::IncomingRequest(_))).await {
        HostEvent::IncomingRequest(r) => r.id,
        other => panic!("expected a prompt, got {other:?}"),
    };
    assert!(host.accept(req, Some(Permissions::VIEW)));
}

/// A dead address and an address that is really this PC come before the host's real
/// address. The viewer must still reach the right host, quickly, and must never show the
/// accept prompt to itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stale_and_own_addresses_ahead_of_the_real_one_still_connect() {
    wait::init_tracing();
    let (host_ep, host, mut events) = host().await;
    let (my_ep, viewer) = listening_viewer();
    let (_hole, dead) = black_hole();
    let target = ConnectTarget::Device(device(
        &host_ep,
        vec![
            dead,
            my_ep.local_addr().unwrap(),
            host_ep.local_addr().unwrap(),
        ],
    ));

    let started = Instant::now();
    let dial = tokio::spawn(async move {
        let r = viewer.connect(target, AuthMethod::AcceptPrompt).await;
        (viewer, r)
    });
    accept_next(&host, &mut events).await;
    let (_viewer, res) = wait::within("the viewer to connect", dial).await.unwrap();
    let (session, _ev) = res.unwrap_or_else(|e| panic!("connect: {e}"));
    assert_eq!(session.peer().device_id, host_ep.device_id());
    // Two staggers (0.25 s each) plus loopback handshakes; the old sequential dial waited a
    // full handshake timeout on the dead address alone.
    assert!(
        started.elapsed() < Duration::from_millis(1800),
        "took {:?}",
        started.elapsed()
    );
    // Our own endpoint refused our own key: nobody was asked to accept themselves.
    assert!(
        wait::quiet(Duration::from_millis(300), my_ep.accept())
            .await
            .is_none(),
        "this PC was prompted to accept a connection from itself"
    );
}

/// Typing your own ID is reported as such, and nothing is dialed.
#[tokio::test(flavor = "multi_thread")]
async fn dialing_your_own_id_is_a_self_connection() {
    let (my_ep, viewer) = listening_viewer();
    let err = viewer
        .connect(
            ConnectTarget::Id(my_ep.device_id()),
            AuthMethod::AcceptPrompt,
        )
        .await
        .expect_err("connected to itself");
    assert!(matches!(err, ViewerError::SelfConnection), "{err}");
}

/// Dialing an address that turns out to be this PC (trust-on-first-use, no ID to compare)
/// is detected from the key, reported as a self-connection, and prompts no one.
#[tokio::test(flavor = "multi_thread")]
async fn dialing_your_own_address_is_a_self_connection() {
    let (my_ep, viewer) = listening_viewer();
    let err = viewer
        .connect(
            ConnectTarget::Addr(my_ep.local_addr().unwrap()),
            AuthMethod::AcceptPrompt,
        )
        .await
        .expect_err("connected to itself");
    assert!(matches!(err, ViewerError::SelfConnection), "{err}");
    assert!(wait::quiet(Duration::from_millis(300), my_ep.accept())
        .await
        .is_none());
}

/// Two different PCs on one LAN (same subnet, same router, same public IP) are never
/// confused: host A's stale address now belongs to host B. Dialing A by ID with B's address
/// listed first reaches A; with only B's address it fails with an identity error instead of
/// connecting to B.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_devices_on_one_lan_are_never_confused() {
    wait::init_tracing();
    let (a_ep, a, mut a_events) = host().await;
    let (b_ep, _b, mut b_events) = host().await;
    assert_ne!(a_ep.device_id(), b_ep.device_id());
    let (_me, viewer) = listening_viewer();
    let viewer = Arc::new(viewer);

    let target = ConnectTarget::Device(device(
        &a_ep,
        vec![b_ep.local_addr().unwrap(), a_ep.local_addr().unwrap()],
    ));
    let v = viewer.clone();
    let dial = tokio::spawn(async move { v.connect(target, AuthMethod::AcceptPrompt).await });
    accept_next(&a, &mut a_events).await;
    let (session, _ev) = wait::within("the viewer to connect", dial)
        .await
        .unwrap()
        .unwrap_or_else(|e| panic!("connect: {e}"));
    assert_eq!(session.peer().device_id, a_ep.device_id());
    session.disconnect();

    // Only B's address: never a session with B, never a prompt on B.
    let target = ConnectTarget::Device(device(&a_ep, vec![b_ep.local_addr().unwrap()]));
    let err = viewer
        .connect(target, AuthMethod::AcceptPrompt)
        .await
        .expect_err("connected to the wrong device");
    match err {
        ViewerError::IdentityMismatch {
            expected,
            presented,
        } => {
            assert_eq!(expected, a_ep.device_id());
            assert_eq!(presented, b_ep.device_id());
        }
        other => panic!("expected an identity mismatch, got {other}"),
    }
    assert!(
        wait::quiet(
            Duration::from_millis(300),
            wait::host_event(&mut b_events, |e| matches!(
                e,
                HostEvent::IncomingRequest(_)
            ))
        )
        .await
        .is_none(),
        "the wrong host was asked to accept"
    );
}

/// When every address is dead the attempt ends with "unreachable" after one handshake
/// timeout, not one timeout per address.
#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_device_fails_in_bounded_time() {
    let (_me, viewer) = listening_viewer();
    let holes: Vec<(UdpSocket, SocketAddr)> = (0..4).map(|_| black_hole()).collect();
    let some_host = quick_endpoint(false, "gone");
    let target = ConnectTarget::Device(device(&some_host, holes.iter().map(|(_, a)| *a).collect()));
    let started = Instant::now();
    let err = viewer
        .connect(target, AuthMethod::AcceptPrompt)
        .await
        .expect_err("connected to a black hole");
    assert!(matches!(err, ViewerError::Unreachable(_)), "{err}");
    // 2 s handshake timeout; the last address starts 0.75 s in.
    let took = started.elapsed();
    assert!(took < Duration::from_millis(3500), "took {took:?}");
}

/// The default test pair still works through the new dial path (pinned ID at an address).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_test_pair_still_connects() {
    let mut pair = TestPair::start().await;
    let conn = pair.connect(Permissions::VIEW).await;
    assert_eq!(conn.session.peer().device_id, pair.host.device_id());
}
