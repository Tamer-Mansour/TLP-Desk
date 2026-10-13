//! Discovery tests on loopback (broadcast fallback with explicit unicast targets) and an
//! opt-in mDNS test (`cargo test -p tlp-desk-net -- --ignored`), since multicast depends on
//! the host's network adapters.

use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use tlp_desk_crypto::DeviceId;
use tlp_desk_net::{Advertisement, Discovery, DiscoveryConfig, DiscoveryEvent};
use tokio::time::timeout;

fn free_udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn ad(id: u32, name: &str) -> Advertisement {
    Advertisement {
        device_id: DeviceId::new(id).unwrap(),
        name: name.into(),
        port: 47400,
    }
}

async fn wait_for(d: &Discovery, id: u32) -> tlp_desk_net::DiscoveredDevice {
    let mut rx = d.subscribe();
    let want = DeviceId::new(id).unwrap();
    timeout(Duration::from_secs(10), async {
        loop {
            if let Some(dev) = d.devices().into_iter().find(|x| x.device_id == want) {
                return dev;
            }
            if let Ok(DiscoveryEvent::Updated(dev)) = rx.recv().await {
                if dev.device_id == want {
                    return dev;
                }
            }
        }
    })
    .await
    .expect("device not discovered in time")
}

#[tokio::test(flavor = "multi_thread")]
async fn broadcast_fallback_discovers_peer_and_expires_it() {
    let (pa, pb) = (free_udp_port(), free_udp_port());
    let target = |p: u16| Some(vec![SocketAddr::from(([127, 0, 0, 1], p))]);
    let a = Discovery::start(DiscoveryConfig {
        advertise: Some(ad(111_111_111, "PC-A")),
        enable_mdns: false,
        broadcast_port: pa,
        broadcast_targets: target(pb),
        announce_interval: Duration::from_millis(200),
        expiry: Duration::from_millis(1500),
        ..DiscoveryConfig::default()
    })
    .await
    .unwrap();
    // B is browse-only: it finds A through A's periodic announcements or as a reply to its query.
    let b = Discovery::start(DiscoveryConfig {
        advertise: None,
        enable_mdns: false,
        broadcast_port: pb,
        broadcast_targets: target(pa),
        announce_interval: Duration::from_millis(200),
        expiry: Duration::from_millis(1500),
        ..DiscoveryConfig::default()
    })
    .await
    .unwrap();
    let dev = wait_for(&b, 111_111_111).await;
    assert_eq!(dev.name, "PC-A");
    assert!(dev.via_broadcast);
    assert_eq!(dev.addrs, vec![SocketAddr::from(([127, 0, 0, 1], 47400))]);
    assert_eq!(dev.protocol_version, tlp_desk_proto::PROTOCOL_VERSION);
    // A never lists itself
    assert!(a.devices().is_empty());

    // A stops announcing -> B drops it after expiry.
    let mut rx = b.subscribe();
    drop(a);
    let lost = timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(DiscoveryEvent::Lost(id)) = rx.recv().await {
                return id;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(lost.as_u32(), 111_111_111);
    assert!(b.devices().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a multicast-capable network adapter; run with --ignored"]
async fn mdns_advertise_and_browse() {
    let a = Discovery::start(DiscoveryConfig {
        advertise: Some(ad(222_222_222, "PC-mDNS")),
        enable_broadcast: false,
        ..DiscoveryConfig::default()
    })
    .await
    .unwrap();
    let b = Discovery::start(DiscoveryConfig {
        enable_broadcast: false,
        ..DiscoveryConfig::default()
    })
    .await
    .unwrap();
    let dev = wait_for(&b, 222_222_222).await;
    assert_eq!(dev.name, "PC-mDNS");
    assert!(dev.via_mdns);
    assert!(dev.addrs.iter().all(|a| a.port() == 47400));
    drop(a);
}
