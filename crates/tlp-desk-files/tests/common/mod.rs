//! Loopback harness: a real host session and a real viewer session over QUIC on 127.0.0.1,
//! each running its own `FileTransfer` through a `FeatureDispatch`, with the same permission
//! gating the host crate applies (`Message::required_permissions`).

#![allow(dead_code)]

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tlp_desk_crypto::Identity;
use tlp_desk_files::{FileTransfer, TransferConfig, TransferEvent, TransferId, TransferState};
use tlp_desk_net::{
    AuthMethod, Endpoint, EndpointConfig, FeatureDispatch, FeatureEventSink, FeatureSet, Session,
};
use tlp_desk_proto::{Message, Permissions};
use tokio::sync::{broadcast, watch};

pub const WAIT: Duration = Duration::from_secs(60);

pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_test_writer()
        .try_init();
}

pub async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, fut)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

pub struct Pair {
    pub host_ft: Arc<FileTransfer>,
    pub viewer_ft: Arc<FileTransfer>,
    pub host_session: Arc<Session>,
    pub viewer_session: Arc<Session>,
    pub host_key: u64,
    pub viewer_key: u64,
    /// Every control message the viewer received (for raw-protocol tests).
    pub viewer_tap: broadcast::Sender<Message>,
    host_perms: watch::Sender<Permissions>,
    _endpoints: (Arc<Endpoint>, Arc<Endpoint>),
}

impl Pair {
    /// Changes the host's grant live, like `Host::set_permissions`.
    pub fn set_permissions(&self, p: Permissions) {
        self.host_perms.send_replace(p);
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

pub async fn pair() -> Pair {
    pair_with(TransferConfig::default(), TransferConfig::default()).await
}

pub async fn pair_with(host_cfg: TransferConfig, viewer_cfg: TransferConfig) -> Pair {
    init_tracing();
    let host_ep = endpoint(true);
    let viewer_ep = endpoint(false);
    let addr = host_ep.local_addr().unwrap();
    let perms = Permissions::VIEW | Permissions::FILES;
    let h = {
        let host_ep = host_ep.clone();
        tokio::spawn(async move { host_ep.accept().await.unwrap().accept(perms).await.unwrap() })
    };
    let viewer_session = Arc::new(
        within(
            "connect",
            viewer_ep.connect(addr, Some(host_ep.device_id()), AuthMethod::AcceptPrompt),
        )
        .await
        .unwrap(),
    );
    let host_session = Arc::new(within("accept", h).await.unwrap());

    let host_ft = FileTransfer::new(host_cfg);
    let viewer_ft = FileTransfer::new(viewer_cfg);
    let (host_key, viewer_key) = (1001, 2002);
    let sink: FeatureEventSink = Arc::new(|_| {});
    let host_dispatch = FeatureDispatch::start(
        &FeatureSet::new().with(host_ft.clone()),
        host_session.clone(),
        host_key,
        perms,
        sink.clone(),
    );
    let viewer_dispatch = FeatureDispatch::start(
        &FeatureSet::new().with(viewer_ft.clone()),
        viewer_session.clone(),
        viewer_key,
        perms,
        sink,
    );
    let (host_perms, mut perm_rx) = watch::channel(perms);

    // Host control loop: permission gate + live permission changes, like tlp-desk-host.
    let hs = host_session.clone();
    tokio::spawn(async move {
        let current = Arc::new(Mutex::new(perms));
        loop {
            tokio::select! {
                changed = perm_rx.changed() => {
                    if changed.is_err() { break; }
                    let p = *perm_rx.borrow();
                    *current.lock().unwrap() = p;
                    host_dispatch.set_permissions(p);
                    let _ = hs.send(&Message::Permissions(p)).await;
                }
                msg = hs.recv() => {
                    let Ok(msg) = msg else { break };
                    let p = *current.lock().unwrap();
                    if p.contains(msg.required_permissions()) {
                        let _ = host_dispatch.dispatch(msg);
                    }
                }
            }
        }
    });
    // Viewer control loop.
    let (viewer_tap, _) = broadcast::channel(4096);
    let vs = viewer_session.clone();
    let tap = viewer_tap.clone();
    tokio::spawn(async move {
        while let Ok(msg) = vs.recv().await {
            let _ = tap.send(msg.clone());
            match msg {
                Message::Permissions(p) => viewer_dispatch.set_permissions(p),
                other => {
                    let _ = viewer_dispatch.dispatch(other);
                }
            }
        }
    });
    Pair {
        host_ft,
        viewer_ft,
        host_session,
        viewer_session,
        host_key,
        viewer_key,
        viewer_tap,
        host_perms,
        _endpoints: (host_ep, viewer_ep),
    }
}

/// Waits for the `Finished` event of transfer `id` (or of the first transfer when `None`).
pub async fn finished(
    rx: &mut broadcast::Receiver<(u64, TransferEvent)>,
    id: Option<TransferId>,
) -> (TransferId, TransferState, u32) {
    within("transfer to finish", async {
        loop {
            match rx.recv().await {
                Ok((
                    _,
                    TransferEvent::Finished {
                        id: got,
                        state,
                        failed_items,
                        ..
                    },
                )) if id.is_none_or(|i| i == got) => return (got, state, failed_items),
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(e) => panic!("event channel: {e}"),
            }
        }
    })
    .await
}

/// Deterministic pseudo-random content.
pub fn content(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// A nested tree with Arabic names, an empty folder and a multi-piece file.
pub fn make_tree(root: &Path, big: usize) -> PathBuf {
    let top = root.join("مجلد المشروع");
    let sub = top.join("الصور").join("2024");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::create_dir_all(top.join("فارغ")).unwrap();
    std::fs::write(
        top.join("تقرير نهائي.txt"),
        "مرحبا بالعالم — hello 123".as_bytes(),
    )
    .unwrap();
    std::fs::write(sub.join("صورة (1).bin"), content(300_000, 1)).unwrap();
    std::fs::write(sub.join("empty.dat"), b"").unwrap();
    std::fs::write(top.join("كبير.bin"), content(big, 2)).unwrap();
    top
}

/// Asserts two trees have the same structure, contents and file times.
pub fn assert_same_tree(a: &Path, b: &Path) {
    let mut ea: Vec<_> = std::fs::read_dir(a).unwrap().map(|e| e.unwrap()).collect();
    let mut eb: Vec<_> = std::fs::read_dir(b).unwrap().map(|e| e.unwrap()).collect();
    ea.sort_by_key(|e| e.file_name());
    eb.sort_by_key(|e| e.file_name());
    let na: Vec<_> = ea.iter().map(|e| e.file_name()).collect();
    let nb: Vec<_> = eb.iter().map(|e| e.file_name()).collect();
    assert_eq!(na, nb, "entries of {a:?} vs {b:?}");
    for (x, y) in ea.iter().zip(&eb) {
        let (mx, my) = (x.metadata().unwrap(), y.metadata().unwrap());
        assert_eq!(mx.is_dir(), my.is_dir());
        if mx.is_dir() {
            assert_same_tree(&x.path(), &y.path());
        } else {
            assert_eq!(
                std::fs::read(x.path()).unwrap(),
                std::fs::read(y.path()).unwrap()
            );
            let ms = |m: &std::fs::Metadata| {
                m.modified()
                    .unwrap()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis()
            };
            assert_eq!(ms(&mx), ms(&my), "mtime of {:?}", x.path());
        }
    }
}
