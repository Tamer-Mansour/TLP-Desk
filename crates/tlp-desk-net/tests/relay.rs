//! TLP-relay end to end on loopback: a relay server, a host that registers with it, and a
//! viewer that reaches the host only through the relay (the viewer never learns the
//! host's real address). The session is the ordinary pinned QUIC session.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tlp_desk_crypto::{DeviceId, Identity};
use tlp_desk_net::relay::server::{RelayServer, RelayServerConfig};
use tlp_desk_net::relay::{self, RelayConfig, Token};
use tlp_desk_net::{AllowList, AuthMethod, Endpoint, EndpointConfig, NetError, Pin};
use tlp_desk_proto::Permissions;
use tokio::time::timeout;

const T: Duration = Duration::from_secs(15);

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tlp_desk_net=info".into()),
        )
        .with_test_writer()
        .try_init();
}

fn endpoint(listen: bool, name: &str) -> Arc<Endpoint> {
    Arc::new(
        Endpoint::bind(
            Arc::new(Identity::generate().unwrap()),
            EndpointConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                device_name: name.into(),
                listen,
                // The host's allow-list stays the LAN default: the relay's address (loopback
                // here, a public IP in real life) gets in only through `set_relay_peer`.
                allow_list: AllowList::lan_default(),
                ..EndpointConfig::default()
            },
        )
        .unwrap(),
    )
}

/// A relay on loopback with a few session ports, picked by binding port 0 and freeing it
/// (a tiny race, acceptable in a test).
async fn relay(secret: &str) -> (Arc<RelayServer>, RelayConfig) {
    let mut ports = Vec::new();
    for _ in 0..3 {
        let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        ports.push(s.local_addr().unwrap().port());
    }
    let (lo, hi) = (*ports.iter().min().unwrap(), *ports.iter().max().unwrap());
    let server = Arc::new(
        RelayServer::bind(RelayServerConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            ports: lo..=hi,
            token: Token::from_secret(secret).unwrap(),
        })
        .await
        .unwrap(),
    );
    let run = server.clone();
    tokio::spawn(async move { run.run().await });
    let addr = server.control_addr().unwrap();
    (server, RelayConfig::new(&addr.to_string(), secret).unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn viewer_reaches_host_through_the_relay() {
    init_tracing();
    let (server, cfg) = relay("pass-phrase").await;
    let host = endpoint(true, "host-pc");
    let viewer = endpoint(false, "viewer-pc");
    let host_id = host.device_id();
    let viewer_id = viewer.device_id();
    let registrar = tokio::spawn(relay::run_host(host.clone(), cfg.clone()));

    // Wait for the registration to land.
    timeout(T, async {
        while server.registered() == 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("host registers");
    assert_eq!(host.relay_peer(), Some("127.0.0.1".parse().unwrap()));

    let h = {
        let host = host.clone();
        tokio::spawn(async move {
            let pending = host.accept().await.expect("pending session");
            assert_eq!(pending.peer().device_id, viewer_id);
            pending.accept(Permissions::VIEW).await.unwrap()
        })
    };
    let target = timeout(T, relay::request(&viewer, &cfg, host_id, T))
        .await
        .unwrap()
        .expect("relay allocates and both sides punch");
    assert_eq!(target.ip(), cfg.resolve().await.unwrap().ip());
    assert_ne!(
        target,
        host.local_addr().unwrap(),
        "the viewer dials the relay, not the host"
    );
    let dialed = timeout(T, viewer.dial_any(&[target], Pin::DeviceId(host_id)))
        .await
        .unwrap()
        .expect("pinned handshake through the relay");
    assert_eq!(dialed.fingerprint().device_id(), host_id);
    let v = timeout(T, viewer.authorize(dialed, AuthMethod::AcceptPrompt, None))
        .await
        .unwrap()
        .unwrap();
    let hs = timeout(T, h).await.unwrap().unwrap();
    assert_eq!(v.peer().device_id, host_id);
    assert_eq!(hs.peer().device_id, viewer_id);
    assert_eq!(server.live_sessions(), 1);
    // The host saw the viewer come from the relay's session port.
    assert_eq!(hs.peer().addr.ip(), target.ip());
    registrar.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_device_is_reported_offline_and_wrong_token_is_ignored() {
    init_tracing();
    let (_server, cfg) = relay("pass-phrase").await;
    let viewer = endpoint(false, "viewer-pc");
    let nobody = DeviceId::new(123_456_789).unwrap();
    let err = timeout(
        T,
        relay::request(&viewer, &cfg, nobody, Duration::from_secs(5)),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(matches!(err, NetError::RelayOffline), "{err}");

    // A viewer with the wrong token never gets an answer (the relay drops the frame).
    let wrong = RelayConfig::new(&cfg.addr, "other").unwrap();
    let err = timeout(
        T,
        relay::request(&viewer, &wrong, nobody, Duration::from_secs(2)),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(matches!(err, NetError::Relay(_)), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn relay_peer_bypasses_the_allow_list_only_for_that_address() {
    let host = endpoint(true, "host-pc");
    assert_eq!(host.relay_peer(), None);
    host.set_relay_peer(Some("203.0.113.5".parse().unwrap()));
    assert_eq!(host.relay_peer(), Some("203.0.113.5".parse().unwrap()));
    let mapped: SocketAddr = "[::ffff:203.0.113.5]:47410".parse().unwrap();
    assert!(relay::is_relay_addr(host.relay_peer(), mapped));
    assert!(!relay::is_relay_addr(
        host.relay_peer(),
        "203.0.113.6:47410".parse().unwrap()
    ));
    host.set_relay_peer(None);
    assert_eq!(host.relay_peer(), None);
}
