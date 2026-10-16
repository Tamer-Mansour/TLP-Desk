//! Per-file progress rows and the "what was copied" record, on both sides of a transfer.

mod common;

use common::*;
use tlp_desk_files::{FileState, TransferState};

const MIB: usize = 1 << 20;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rows_follow_every_file_to_done_on_both_sides() {
    let p = pair().await;
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let top = src.path().join("مجلد");
    std::fs::create_dir_all(top.join("فرعي")).unwrap();
    std::fs::write(top.join("أ.txt"), "أ").unwrap();
    std::fs::write(top.join("فرعي").join("ب.bin"), content(3 * MIB, 4)).unwrap();
    std::fs::write(top.join("فارغ.dat"), b"").unwrap();

    let mut vev = p.viewer_ft.subscribe();
    let mut hev = p.host_ft.subscribe();
    let id = p
        .viewer_ft
        .start_upload(
            p.viewer_key,
            vec![top.clone()],
            dst.path().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    assert_eq!(
        finished(&mut vev, Some(id)).await.1,
        TransferState::Completed
    );
    finished(&mut hev, None).await;

    let host_id = p.host_ft.transfers()[0].info.id;
    for rows in [
        p.viewer_ft.file_rows(id, 0, 100),
        p.host_ft.file_rows(host_id, 0, 100),
    ] {
        let names: Vec<_> = rows.iter().map(|r| r.rel_path.as_str()).collect();
        assert_eq!(rows.len(), 3, "folders are not rows: {names:?}");
        assert!(names.contains(&"مجلد/فرعي/ب.bin"), "{names:?}");
        for r in &rows {
            assert_eq!(r.state, FileState::Done, "{}", r.rel_path);
            assert_eq!(r.done_bytes, r.size, "{}", r.rel_path);
        }
    }
    // Paging.
    let page = p.viewer_ft.file_rows(id, 1, 1);
    assert_eq!(page.len(), 1);
    assert!(p.viewer_ft.file_rows(id, 9, 10).is_empty());

    let origin = p.viewer_ft.origin(id).unwrap();
    assert!(origin.upload);
    assert_eq!(origin.local_paths, vec![top]);
    assert_eq!(origin.remote_dest, dst.path().to_str().unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_running_file_is_active_and_a_download_remembers_its_sources() {
    let p = pair().await;
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let file = src.path().join("فيديو.bin");
    std::fs::write(&file, content(40 * MIB, 5)).unwrap();
    p.host_ft.test_set_rate_limit(20 * MIB as u64);
    let mut vev = p.viewer_ft.subscribe();
    let id = p
        .viewer_ft
        .start_download(
            p.viewer_key,
            vec![file.to_str().unwrap().into()],
            dst.path().to_path_buf(),
        )
        .await
        .unwrap();
    within("file to become active", async {
        loop {
            let rows = p.viewer_ft.file_rows(id, 0, 10);
            if rows
                .first()
                .is_some_and(|r| r.state == FileState::Active && r.done_bytes > 0)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    p.host_ft.test_set_rate_limit(0);
    assert_eq!(
        finished(&mut vev, Some(id)).await.1,
        TransferState::Completed
    );
    let origin = p.viewer_ft.origin(id).unwrap();
    assert!(!origin.upload);
    assert_eq!(
        origin.remote_paths,
        vec![file.to_str().unwrap().to_string()]
    );
    assert_eq!(origin.local_dest.as_deref(), Some(dst.path()));
    assert!(p
        .viewer_ft
        .origin(tlp_desk_files::TransferId(9_999))
        .is_none());
}
