//! Loopback harness: a real host and viewer `Session` over QUIC on 127.0.0.1, each with a
//! `FeatureDispatch` and a control loop that routes messages the way `tlp-desk-host` and
//! `tlp-desk-viewer` do (the host gates by `Message::required_permissions` against the live
//! permissions, the viewer applies `Message::Permissions`).

#![allow(dead_code)]

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tlp_desk_crypto::Identity;
use tlp_desk_net::{
    AuthMethod, Endpoint, EndpointConfig, FeatureDispatch, FeatureEvent, FeatureEventSink,
    FeatureSet, Session,
};
use tlp_desk_proto::{Message, Permissions};

pub const T: Duration = Duration::from_secs(15);
pub const HOST_KEY: u64 = 1;
pub const VIEWER_KEY: u64 = 7;

pub struct Pair {
    pub host: Arc<Session>,
    pub viewer: Arc<Session>,
    host_perms: Arc<AtomicU16>,
    host_dispatch: Arc<FeatureDispatch>,
    pub viewer_events: Arc<Mutex<Vec<FeatureEvent>>>,
    pub host_events: Arc<Mutex<Vec<FeatureEvent>>>,
    /// Messages the host received that no host feature handles (for tests to inspect).
    pub host_unrouted: Arc<Mutex<Vec<Message>>>,
    /// Messages the viewer received that no viewer feature handles.
    pub viewer_unrouted: Arc<Mutex<Vec<Message>>>,
    _endpoints: (Arc<Endpoint>, Arc<Endpoint>),
}

/// Waits for the first message in `store` that `pick` accepts and returns what it extracts.
pub async fn take_unrouted<R>(
    store: &Arc<Mutex<Vec<Message>>>,
    mut pick: impl FnMut(&Message) -> Option<R>,
) -> R {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        if let Some(r) = store.lock().unwrap().iter().find_map(&mut pick) {
            return r;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "message not received in time"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

impl Pair {
    /// Live permission change, as `Host::set_permissions` does it: features first, then the
    /// viewer is told.
    pub async fn set_permissions(&self, p: Permissions) {
        self.host_perms.store(p.0, Ordering::Release);
        self.host_dispatch.set_permissions(p);
        self.host.send(&Message::Permissions(p)).await.unwrap();
    }

    /// Changes only what the host's router lets through (to test the features' own checks).
    pub fn set_permissions_router_only(&self, p: Permissions) {
        self.host_perms.store(p.0, Ordering::Release);
    }
}

fn endpoint(listen: bool) -> Arc<Endpoint> {
    Arc::new(
        Endpoint::bind(
            Arc::new(Identity::generate().unwrap()),
            EndpointConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                listen,
                device_name: if listen { "مضيف" } else { "viewer-pc" }.into(),
                ..EndpointConfig::default()
            },
        )
        .unwrap(),
    )
}

fn sink(store: Arc<Mutex<Vec<FeatureEvent>>>) -> FeatureEventSink {
    Arc::new(move |ev| store.lock().unwrap().push(ev))
}

/// Connects a viewer to a host that grants `perms`, and starts `host_features` /
/// `viewer_features` on the two sessions.
pub async fn connect(
    perms: Permissions,
    host_features: FeatureSet,
    viewer_features: FeatureSet,
) -> Pair {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_test_writer()
        .try_init();
    let host_ep = endpoint(true);
    let viewer_ep = endpoint(false);
    let addr = host_ep.local_addr().unwrap();
    let h = {
        let host_ep = host_ep.clone();
        tokio::spawn(async move {
            let pending = host_ep.accept().await.expect("pending session");
            pending.accept(perms).await.unwrap()
        })
    };
    let v = tokio::time::timeout(
        T,
        viewer_ep.connect(addr, Some(host_ep.device_id()), AuthMethod::AcceptPrompt),
    )
    .await
    .unwrap()
    .unwrap();
    let h = tokio::time::timeout(T, h).await.unwrap().unwrap();
    let host = Arc::new(h);
    let viewer = Arc::new(v);

    let host_events = Arc::new(Mutex::new(Vec::new()));
    let viewer_events = Arc::new(Mutex::new(Vec::new()));
    let host_dispatch = Arc::new(FeatureDispatch::start(
        &host_features,
        host.clone(),
        HOST_KEY,
        perms,
        sink(host_events.clone()),
    ));
    let viewer_dispatch = FeatureDispatch::start(
        &viewer_features,
        viewer.clone(),
        VIEWER_KEY,
        viewer.initial_permissions(),
        sink(viewer_events.clone()),
    );
    let host_perms = Arc::new(AtomicU16::new(perms.0));
    let host_unrouted = Arc::new(Mutex::new(Vec::new()));
    let viewer_unrouted = Arc::new(Mutex::new(Vec::new()));

    // Host control loop: the permission gate of `tlp_desk_host::route_to_feature`.
    {
        let host = host.clone();
        let dispatch = host_dispatch.clone();
        let perms = host_perms.clone();
        let unrouted = host_unrouted.clone();
        tokio::spawn(async move {
            while let Ok(msg) = host.recv().await {
                let now = Permissions(perms.load(Ordering::Acquire));
                if now.contains(msg.required_permissions()) {
                    if let Err(msg) = dispatch.dispatch(msg) {
                        unrouted.lock().unwrap().push(*msg);
                    }
                }
            }
        });
    }
    // Viewer control loop: permissions update the features, everything else is routed.
    {
        let viewer = viewer.clone();
        let unrouted = viewer_unrouted.clone();
        tokio::spawn(async move {
            while let Ok(msg) = viewer.recv().await {
                match msg {
                    Message::Permissions(p) => viewer_dispatch.set_permissions(p),
                    other => {
                        if let Err(msg) = viewer_dispatch.dispatch(other) {
                            unrouted.lock().unwrap().push(*msg);
                        }
                    }
                }
            }
        });
    }
    Pair {
        host,
        viewer,
        host_perms,
        host_dispatch,
        viewer_events,
        host_events,
        host_unrouted,
        viewer_unrouted,
        _endpoints: (host_ep, viewer_ep),
    }
}

/// Polls `cond` until it holds or [`T`] passes.
pub async fn wait_until(mut cond: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + T;
    while !cond() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "condition not met in time"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
