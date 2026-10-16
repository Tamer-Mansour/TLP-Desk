//! Explorer-style paste end to end over a real loopback session: the peer copies files, this
//! PC gets a virtual clipboard offer, and reading a virtual file pulls the bytes through the
//! real transfer engine. Only the OS clipboard is replaced by a recording fake.

#[path = "fs_common/mod.rs"]
mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{connect, wait_until, Pair};
use tlp_desk_files::clipfiles::{ClipFilesConfig, ClipFilesFeature, ManualClipboard};
use tlp_desk_files::shellclip::engine::install;
use tlp_desk_files::shellclip::{ClipboardPort, ShellClipboard, StagedItems};
use tlp_desk_files::{ConflictPolicy, FileTransfer, TransferConfig};
use tlp_desk_net::FeatureSet;
use tlp_desk_proto::Permissions;

const BOTH: Permissions =
    Permissions(Permissions::VIEW.0 | Permissions::CLIPBOARD.0 | Permissions::FILES.0);

/// Stands in for the OS clipboard.
#[derive(Default)]
struct FakePort {
    offer: Mutex<Option<Arc<StagedItems>>>,
    clears: Mutex<u32>,
}

impl ClipboardPort for FakePort {
    fn set(&self, offer: Arc<StagedItems>) {
        *self.offer.lock().unwrap() = Some(offer);
    }
    fn clear(&self) {
        *self.offer.lock().unwrap() = None;
        *self.clears.lock().unwrap() += 1;
    }
}

struct Side {
    clip: ManualClipboard,
    port: Arc<FakePort>,
    _staging: tempfile::TempDir,
}

struct Setup {
    pair: Pair,
    host: Side,
    viewer: Side,
}

fn side(feature_clip: &ManualClipboard) -> (Arc<ClipFilesFeature>, Arc<FileTransfer>, Side) {
    let clip = feature_clip.clone();
    let feature = ClipFilesFeature::new(ClipFilesConfig {
        source: clip.factory(),
        poll: Duration::from_millis(10),
    });
    let engine = FileTransfer::new(TransferConfig::default());
    let port = Arc::new(FakePort::default());
    let staging = tempfile::tempdir().unwrap();
    install(
        &feature,
        &engine,
        ShellClipboard::with_port(port.clone()),
        ConflictPolicy::Overwrite,
        staging.path().to_path_buf(),
    );
    (
        feature,
        engine,
        Side {
            clip,
            port,
            _staging: staging,
        },
    )
}

async fn setup() -> Setup {
    let (hf, he, host) = side(&ManualClipboard::default());
    let (vf, ve, viewer) = side(&ManualClipboard::default());
    let pair = connect(
        BOTH,
        FeatureSet::new().with(hf).with(he),
        FeatureSet::new().with(vf).with(ve),
    )
    .await;
    Setup { pair, host, viewer }
}

fn offer_of(side: &Side) -> Option<Arc<StagedItems>> {
    side.port.offer.lock().unwrap().clone()
}

/// Reads virtual file `rel` like Explorer's stream would (waits for the transfer).
async fn read_virtual(staged: &Arc<StagedItems>, rel: &str) -> Vec<u8> {
    let idx = staged
        .items()
        .iter()
        .position(|i| i.rel_path == rel)
        .unwrap_or_else(|| panic!("{rel} not announced"));
    let staged = staged.clone();
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut f = staged.open(idx, &mut || {}).expect("virtual file");
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).unwrap();
        buf
    })
    .await
    .unwrap()
}

fn make_files(dir: &Path) -> std::path::PathBuf {
    let folder = dir.join("مجلد النسخ");
    std::fs::create_dir_all(folder.join("داخل")).unwrap();
    std::fs::write(folder.join("نص.txt"), "مرحبا بالعالم").unwrap();
    std::fs::write(folder.join("داخل").join("كبير.bin"), vec![9u8; 3 << 20]).unwrap();
    std::fs::write(dir.join("منفرد.txt"), "abc").unwrap();
    folder
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_copy_pastes_on_the_viewer_through_the_engine() {
    let s = setup().await;
    let tmp = tempfile::tempdir().unwrap();
    let folder = make_files(tmp.path());
    s.host.clip.set(vec![folder, tmp.path().join("منفرد.txt")]);

    wait_until(|| offer_of(&s.viewer).is_some()).await;
    let staged = offer_of(&s.viewer).unwrap();
    assert!(staged
        .items()
        .iter()
        .any(|i| i.rel_path == "مجلد النسخ/داخل" && i.is_dir));
    // Nothing moved yet: delayed rendering.
    assert!(!staged.is_ready(0));
    assert_eq!(read_virtual(&staged, "منفرد.txt").await, b"abc");
    assert_eq!(
        read_virtual(&staged, "مجلد النسخ/نص.txt").await,
        "مرحبا بالعالم".as_bytes()
    );
    assert_eq!(
        read_virtual(&staged, "مجلد النسخ/داخل/كبير.bin").await,
        vec![9u8; 3 << 20]
    );

    // The host copies something that is not files: the viewer's clipboard offer goes away.
    s.host.clip.set(Vec::new());
    wait_until(|| offer_of(&s.viewer).is_none()).await;
    assert!(staged.is_cancelled());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn viewer_copy_pastes_on_the_host_through_a_pull() {
    let s = setup().await;
    let tmp = tempfile::tempdir().unwrap();
    let folder = make_files(tmp.path());
    s.viewer
        .clip
        .set(vec![folder, tmp.path().join("منفرد.txt")]);

    wait_until(|| offer_of(&s.host).is_some()).await;
    let staged = offer_of(&s.host).unwrap();
    assert!(!staged.is_ready(0));
    assert_eq!(read_virtual(&staged, "منفرد.txt").await, b"abc");
    assert_eq!(
        read_virtual(&staged, "مجلد النسخ/داخل/كبير.bin").await,
        vec![9u8; 3 << 20]
    );
    assert_eq!(
        read_virtual(&staged, "مجلد النسخ/نص.txt").await,
        "مرحبا بالعالم".as_bytes()
    );

    s.viewer.clip.set(Vec::new());
    wait_until(|| offer_of(&s.host).is_none()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoking_files_removes_the_virtual_clipboard() {
    let s = setup().await;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("ملف.txt"), "x").unwrap();
    s.host.clip.set(vec![tmp.path().join("ملف.txt")]);
    wait_until(|| offer_of(&s.viewer).is_some()).await;

    s.pair
        .set_permissions(Permissions::VIEW | Permissions::CLIPBOARD)
        .await;
    wait_until(|| offer_of(&s.viewer).is_none()).await;
    assert!(*s.viewer.port.clears.lock().unwrap() >= 1);
}
