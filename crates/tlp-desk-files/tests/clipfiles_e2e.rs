//! Explorer copy on the host, announced to the viewer over a real loopback session.

#[path = "fs_common/mod.rs"]
mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{connect, wait_until, Pair, HOST_KEY, VIEWER_KEY};
use tlp_desk_files::clipfiles::{
    ClipFilesConfig, ClipFilesFeature, ManualClipboard, RemoteClipboardFiles, CLIPBOARD_ID_BIT,
    FEATURE_NAME,
};
use tlp_desk_net::FeatureSet;
use tlp_desk_proto::Permissions;

const BOTH: Permissions =
    Permissions(Permissions::VIEW.0 | Permissions::CLIPBOARD.0 | Permissions::FILES.0);

struct Setup {
    pair: Pair,
    clip: ManualClipboard,
    host: Arc<ClipFilesFeature>,
    viewer: Arc<ClipFilesFeature>,
}

async fn setup(perms: Permissions) -> Setup {
    let clip = ManualClipboard::default();
    let host = ClipFilesFeature::new(ClipFilesConfig {
        source: clip.factory(),
        poll: Duration::from_millis(10),
    });
    // The viewer's own clipboard is never read in these tests.
    let viewer = ClipFilesFeature::new(ClipFilesConfig {
        source: ManualClipboard::default().factory(),
        poll: Duration::from_millis(10),
    });
    let pair = connect(
        perms,
        FeatureSet::new().with(host.clone()),
        FeatureSet::new().with(viewer.clone()),
    )
    .await;
    Setup {
        pair,
        clip,
        host,
        viewer,
    }
}

fn last_event(pair: &Pair) -> Option<RemoteClipboardFiles> {
    let events = pair.viewer_events.lock().unwrap();
    events
        .iter()
        .rev()
        .find(|e| e.feature == FEATURE_NAME)
        .and_then(|e| e.payload::<RemoteClipboardFiles>().cloned())
}

#[tokio::test(flavor = "multi_thread")]
async fn copied_files_are_announced_with_arabic_names_and_cleared() {
    let s = setup(BOTH).await;
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("مستندات العمل");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("عقد.pdf"), [0u8; 10]).unwrap();
    std::fs::write(tmp.path().join("صورة 1.png"), [0u8; 5]).unwrap();
    s.clip
        .set(vec![folder.clone(), tmp.path().join("صورة 1.png")]);

    wait_until(|| s.viewer.remote_files(VIEWER_KEY).is_some()).await;
    let got = s.viewer.remote_files(VIEWER_KEY).unwrap();
    assert_ne!(got.transfer_id & CLIPBOARD_ID_BIT, 0);
    let rel: Vec<_> = got.items.iter().map(|i| i.rel_path.as_str()).collect();
    assert_eq!(
        rel,
        ["مستندات العمل", "مستندات العمل/عقد.pdf", "صورة 1.png"]
    );
    assert_eq!(got.top_level_count(), 2);
    assert_eq!(got.total_bytes(), 15);
    assert_eq!(last_event(&s.pair), Some(got.clone()));

    // The host can resolve the id the viewer will use to pull the files.
    let announced = s.host.announced(common::HOST_KEY, got.transfer_id).unwrap();
    assert_eq!(announced.set.source_dir, tmp.path());
    assert_eq!(announced.set.items, got.items);
    assert!(s.host.announced(HOST_KEY, got.transfer_id ^ 1).is_none());

    // Copying something else (text) clears the viewer's entry.
    s.clip.set(Vec::new());
    wait_until(|| s.viewer.remote_files(VIEWER_KEY).is_none()).await;
    assert_eq!(last_event(&s.pair).map(|e| e.items.len()), Some(0));
    assert!(s.host.announced(HOST_KEY, got.transfer_id).is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_announced_without_clipboard_and_files() {
    for perms in [
        Permissions::VIEW | Permissions::CLIPBOARD,
        Permissions::VIEW | Permissions::FILES,
    ] {
        let s = setup(perms).await;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("سر.txt"), "x").unwrap();
        s.clip.set(vec![tmp.path().join("سر.txt")]);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(s.viewer.remote_files(VIEWER_KEY).is_none(), "{perms:?}");
        assert!(last_event(&s.pair).is_none(), "{perms:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_mid_session_clears_and_regrant_announces_again() {
    let s = setup(BOTH).await;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("ملف.txt"), "x").unwrap();
    s.clip.set(vec![tmp.path().join("ملف.txt")]);
    wait_until(|| s.viewer.remote_files(VIEWER_KEY).is_some()).await;

    s.pair
        .set_permissions(Permissions::VIEW | Permissions::CLIPBOARD)
        .await;
    wait_until(|| s.viewer.remote_files(VIEWER_KEY).is_none()).await;

    // While revoked, new copies are not announced.
    std::fs::write(tmp.path().join("آخر.txt"), "y").unwrap();
    s.clip.set(vec![tmp.path().join("آخر.txt")]);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(s.viewer.remote_files(VIEWER_KEY).is_none());

    s.pair.set_permissions(BOTH).await;
    wait_until(|| s.viewer.remote_files(VIEWER_KEY).is_some()).await;
    let got = s.viewer.remote_files(VIEWER_KEY).unwrap();
    assert_eq!(got.items[0].rel_path, "آخر.txt");
}

#[tokio::test(flavor = "multi_thread")]
async fn already_copied_files_are_announced_when_the_session_starts() {
    let clip = ManualClipboard::default();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("قبل الاتصال.txt"), "x").unwrap();
    clip.set(vec![tmp.path().join("قبل الاتصال.txt")]);
    let host = ClipFilesFeature::new(ClipFilesConfig {
        source: clip.factory(),
        poll: Duration::from_millis(10),
    });
    let viewer = ClipFilesFeature::new(ClipFilesConfig::default());
    let _pair = connect(
        BOTH,
        FeatureSet::new().with(host),
        FeatureSet::new().with(viewer.clone()),
    )
    .await;
    wait_until(|| viewer.remote_files(VIEWER_KEY).is_some()).await;
}
