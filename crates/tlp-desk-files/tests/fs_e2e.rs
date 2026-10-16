//! Remote file system end to end over a real loopback session: list, stat, mkdir, rename and
//! delete with Arabic names, permission gating, live revocation, hostile paths, a 10k-entry
//! folder.

#[path = "fs_common/mod.rs"]
mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{connect, take_unrouted, Pair, T, VIEWER_KEY};
use tlp_desk_files::fs::{FsClient, FsClientError, FsConfig, FsFeature};
use tlp_desk_net::FeatureSet;
use tlp_desk_proto::{FsError, FsOp, FsReply, FsRootKind, Message, Permissions};

const WITH_FILES: Permissions = Permissions(Permissions::VIEW.0 | Permissions::FILES.0);

fn s(p: &Path) -> String {
    p.to_str().unwrap().to_owned()
}

async fn setup(perms: Permissions, cfg: FsConfig) -> (Pair, FsClient) {
    let host_fs = FsFeature::new(cfg);
    let viewer_fs = FsFeature::new(FsConfig::default());
    let pair = connect(
        perms,
        FeatureSet::new().with(host_fs),
        FeatureSet::new().with(viewer_fs.clone()),
    )
    .await;
    let client = viewer_fs.client(VIEWER_KEY).expect("viewer client");
    (pair, client)
}

#[tokio::test(flavor = "multi_thread")]
async fn list_stat_mkdir_rename_delete_with_arabic_names() {
    let (_pair, fs) = setup(WITH_FILES, FsConfig::default()).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("تقرير 2024.txt"), "نص عربي").unwrap();

    let roots = fs.roots().await.unwrap();
    assert!(roots.iter().any(|r| r.kind == FsRootKind::Drive));

    let folder = root.join("مجلد جديد");
    fs.make_dir(&s(&folder)).await.unwrap();
    assert!(folder.is_dir());
    assert_eq!(
        fs.make_dir(&s(&folder)).await,
        Err(FsClientError::Remote(FsError::AlreadyExists))
    );

    let list = fs.list_dir(&s(root)).await.unwrap();
    let names: Vec<_> = list.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["مجلد جديد", "تقرير 2024.txt"]);

    let entry = fs.stat(&s(&root.join("تقرير 2024.txt"))).await.unwrap();
    assert_eq!(entry.size, "نص عربي".len() as u64);
    assert!(!entry.is_dir);

    let renamed = folder.join("التقرير النهائي.txt");
    fs.rename(&s(&root.join("تقرير 2024.txt")), &s(&renamed))
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&renamed).unwrap(), "نص عربي");

    assert_eq!(
        fs.delete(&s(&folder), false).await,
        Err(FsClientError::Remote(FsError::NotEmpty))
    );
    fs.delete(&s(&folder), true).await.unwrap();
    assert!(!folder.exists());
    assert_eq!(
        fs.stat(&s(&folder)).await,
        Err(FsClientError::Remote(FsError::NotFound))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_files_permission_means_no_access() {
    let (pair, fs) = setup(Permissions::VIEW, FsConfig::default()).await;
    let tmp = tempfile::tempdir().unwrap();
    // The client refuses locally...
    assert_eq!(
        fs.list_dir(&s(tmp.path())).await,
        Err(FsClientError::NoPermission)
    );
    // ...and a request sent anyway is dropped by the host's permission gate: no answer.
    pair.viewer
        .send(&Message::FsRequest {
            req_id: 99,
            op: FsOp::MakeDir {
                path: s(&tmp.path().join("x")),
            },
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!tmp.path().join("x").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_feature_itself_refuses_without_files() {
    // Even if a router forgot the permission check, the host feature answers PermissionDenied.
    let host_fs = FsFeature::new(FsConfig::default());
    let pair = connect(
        Permissions::VIEW | Permissions::FILES,
        FeatureSet::new().with(host_fs),
        FeatureSet::new(),
    )
    .await;
    // Revoke on the feature side only: the test router still lets FILES messages through.
    pair.set_permissions(Permissions::VIEW).await;
    pair.set_permissions_router_only(WITH_FILES);
    let tmp = tempfile::tempdir().unwrap();
    pair.viewer
        .send(&Message::FsRequest {
            req_id: 5,
            op: FsOp::ListDir {
                path: s(tmp.path()),
            },
        })
        .await
        .unwrap();
    let answer = take_unrouted(&pair.viewer_unrouted, |m| match m {
        Message::FsResponse { req_id, result } => Some((*req_id, result.clone())),
        _ => None,
    })
    .await;
    assert_eq!(answer, (5, Err(FsError::PermissionDenied)));
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_files_mid_session_stops_access_and_regrant_restores_it() {
    let (pair, fs) = setup(WITH_FILES, FsConfig::default()).await;
    let tmp = tempfile::tempdir().unwrap();
    assert!(fs.list_dir(&s(tmp.path())).await.is_ok());

    pair.set_permissions(Permissions::VIEW).await;
    common::wait_until(|| !fs.has_permission()).await;
    assert_eq!(
        fs.make_dir(&s(&tmp.path().join("بعد السحب"))).await,
        Err(FsClientError::NoPermission)
    );
    assert!(!tmp.path().join("بعد السحب").exists());

    pair.set_permissions(WITH_FILES).await;
    common::wait_until(|| fs.has_permission()).await;
    fs.make_dir(&s(&tmp.path().join("بعد الإعادة")))
        .await
        .unwrap();
    assert!(tmp.path().join("بعد الإعادة").is_dir());
}

#[tokio::test(flavor = "multi_thread")]
async fn hostile_paths_are_rejected_over_the_wire() {
    let (_pair, fs) = setup(WITH_FILES, FsConfig::default()).await;
    let tmp = tempfile::tempdir().unwrap();
    let base = s(tmp.path());
    let file = tmp.path().join("a.txt");
    std::fs::write(&file, "x").unwrap();
    for bad in [
        format!("{base}\\..\\..\\Windows"),
        format!("{base}/../x"),
        format!("{}:secret", s(&file)),
        format!("{base}\\NUL"),
        r"\\127.0.0.1\c$".to_owned(),
        r"\\.\PhysicalDrive0".to_owned(),
        "relative\\path".to_owned(),
        String::new(),
    ] {
        #[cfg(not(windows))]
        if bad.contains('\\') || bad.contains(':') || bad.contains("NUL") {
            continue;
        }
        assert_eq!(
            fs.list_dir(&bad).await,
            Err(FsClientError::Remote(FsError::InvalidPath)),
            "{bad:?}"
        );
    }
    // Nothing was written next to the file.
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn allow_list_confines_the_viewer() {
    let tmp = tempfile::tempdir().unwrap();
    let shared = tmp.path().join("مشترك");
    let private = tmp.path().join("خاص");
    std::fs::create_dir(&shared).unwrap();
    std::fs::create_dir(&private).unwrap();
    let cfg = FsConfig {
        allowed_roots: vec![shared.clone()],
    };
    let (_pair, fs) = setup(WITH_FILES, cfg).await;
    let roots = fs.roots().await.unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].path, s(&shared));
    assert!(fs.list_dir(&s(&shared)).await.is_ok());
    assert_eq!(
        fs.list_dir(&s(&private)).await,
        Err(FsClientError::Remote(FsError::PermissionDenied))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ten_thousand_entries_list_in_under_a_second() {
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..10_000u32 {
        std::fs::File::create(tmp.path().join(format!("ملف رقم {i:05}.dat"))).unwrap();
    }
    let (_pair, fs) = setup(WITH_FILES, FsConfig::default()).await;
    let start = Instant::now();
    let list = fs.list_dir(&s(tmp.path())).await.unwrap();
    let took = start.elapsed();
    assert_eq!(list.len(), 10_000);
    assert_eq!(list[0].name, "ملف رقم 00000.dat");
    assert!(
        took < Duration::from_secs(1),
        "remote listing took {took:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn viewer_refuses_to_serve_its_own_disk() {
    let viewer_fs = FsFeature::new(FsConfig::default());
    let pair = connect(
        WITH_FILES,
        FeatureSet::new(),
        FeatureSet::new().with(viewer_fs),
    )
    .await;
    pair.host
        .send(&Message::FsRequest {
            req_id: 1,
            op: FsOp::Roots,
        })
        .await
        .unwrap();
    let answer = take_unrouted(&pair.host_unrouted, |m| match m {
        Message::FsResponse { req_id, result } => Some((*req_id, result.clone())),
        _ => None,
    })
    .await;
    assert_eq!(answer, (1, Err::<FsReply, _>(FsError::PermissionDenied)));
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_time_out_and_end_with_the_session() {
    // A host without the feature never answers.
    let viewer_fs = FsFeature::new(FsConfig::default());
    let pair = connect(
        WITH_FILES,
        FeatureSet::new(),
        FeatureSet::new().with(viewer_fs.clone()),
    )
    .await;
    let fs = viewer_fs.client(VIEWER_KEY).unwrap();
    assert_eq!(
        fs.request_timeout(FsOp::Roots, Duration::from_millis(200))
            .await,
        Err(FsClientError::Timeout)
    );
    let waiting = {
        let fs = fs.clone();
        tokio::spawn(async move { fs.roots().await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    pair.host.close(tlp_desk_proto::ByeReason::UserClosed).await;
    let r = tokio::time::timeout(T, waiting).await.unwrap().unwrap();
    assert!(
        matches!(r, Err(FsClientError::Closed | FsClientError::Send(_))),
        "{r:?}"
    );
    common::wait_until(|| viewer_fs.client(VIEWER_KEY).is_none()).await;
}
