//! Loopback tests of the transfer engine: real QUIC sessions, real files in temp folders.

mod common;

use std::time::Duration;

use common::*;
use tlp_desk_files::{
    ConflictPolicy, TransferConfig, TransferEvent, TransferRole, TransferState, PART_SUFFIX,
};
use tlp_desk_proto::{
    FsError, Message, Permissions, TransferDirection, TransferItem, TransferOffer,
};

const MIB: usize = 1 << 20;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upload_nested_arabic_folder() {
    let p = pair().await;
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let top = make_tree(src.path(), 40 * MIB + 123);
    let single = src.path().join("ملف منفرد.txt");
    std::fs::write(&single, "سطر واحد").unwrap();

    let mut hev = p.host_ft.subscribe();
    let mut vev = p.viewer_ft.subscribe();
    let id = p
        .viewer_ft
        .start_upload(
            p.viewer_key,
            vec![top.clone(), single.clone()],
            dst.path().to_str().unwrap().to_string(),
        )
        .await
        .unwrap();
    let (_, vstate, vfailed) = finished(&mut vev, Some(id)).await;
    let (_, hstate, hfailed) = finished(&mut hev, None).await;
    assert_eq!((vstate, vfailed), (TransferState::Completed, 0));
    assert_eq!((hstate, hfailed), (TransferState::Completed, 0));

    assert_same_tree(&top, &dst.path().join("مجلد المشروع"));
    assert!(dst.path().join("مجلد المشروع").join("فارغ").is_dir());
    assert_eq!(
        std::fs::read_to_string(dst.path().join("ملف منفرد.txt")).unwrap(),
        "سطر واحد"
    );
    let host = p.host_ft.transfers();
    assert_eq!(host.len(), 1);
    assert_eq!(host[0].info.role, TransferRole::Receiving);
    assert_eq!(host[0].info.direction, TransferDirection::Upload);
    assert_eq!(host[0].info.total_files, 5);
    assert_eq!(host[0].info.total_dirs, 4);
    assert_eq!(host[0].bytes_done, host[0].info.total_bytes);
    let viewer = p.viewer_ft.snapshot(id).unwrap();
    assert_eq!(viewer.info.label, "مجلد المشروع");
    assert_eq!(viewer.info.top_level_items, 2);
    no_partials(dst.path());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn download_nested_arabic_folder() {
    let p = pair().await;
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let top = make_tree(src.path(), 20 * MIB + 7);
    let mut vev = p.viewer_ft.subscribe();
    let mut hev = p.host_ft.subscribe();
    let id = p
        .viewer_ft
        .start_download(
            p.viewer_key,
            vec![top.to_str().unwrap().to_string()],
            dst.path().to_path_buf(),
        )
        .await
        .unwrap();
    let (_, vstate, _) = finished(&mut vev, Some(id)).await;
    let (_, hstate, _) = finished(&mut hev, None).await;
    assert_eq!(vstate, TransferState::Completed);
    assert_eq!(hstate, TransferState::Completed);
    assert_same_tree(&top, &dst.path().join("مجلد المشروع"));
    let snap = p.viewer_ft.snapshot(id).unwrap();
    assert_eq!(snap.info.role, TransferRole::Receiving);
    assert_eq!(snap.info.direction, TransferDirection::Download);
    assert!(snap.local_dest.is_some());
    no_partials(dst.path());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_and_resume_sends_only_the_rest() {
    // One stream: pieces complete strictly in order, so pausing at half keeps whole pieces
    // (with several streams the resume point is the end of the first unfinished piece).
    let p = pair_with(
        TransferConfig::default(),
        TransferConfig {
            streams: 1,
            ..TransferConfig::default()
        },
    )
    .await;
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let file = src.path().join("فيديو كبير.bin");
    let data = content(80 * MIB + 5, 9);
    std::fs::write(&file, &data).unwrap();
    // 40 MiB/s: the transfer takes ~2 s, so we can pause it in the middle.
    p.viewer_ft.test_set_rate_limit(40 * MIB as u64);
    let mut vev = p.viewer_ft.subscribe();
    let mut hev = p.host_ft.subscribe();
    let id = p
        .viewer_ft
        .start_upload(
            p.viewer_key,
            vec![file.clone()],
            dst.path().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    within("half of the file", async {
        loop {
            if p.host_ft
                .transfers()
                .first()
                .is_some_and(|s| s.bytes_done >= 40 * MIB as u64)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(p.viewer_ft.pause(id));
    let (_, vstate, _) = finished(&mut vev, Some(id)).await;
    assert_eq!(vstate, TransferState::Paused);
    let (_, hstate, _) = finished(&mut hev, None).await;
    assert_eq!(hstate, TransferState::Cancelled);
    // The partial file and its resume record are on the host, the final name is not.
    let final_path = dst.path().join("فيديو كبير.bin");
    within("resume record", async {
        while !dst
            .path()
            .join(format!("فيديو كبير.bin{PART_SUFFIX}.meta"))
            .exists()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(!final_path.exists());

    p.viewer_ft.test_set_rate_limit(0);
    p.viewer_ft.resume(id, p.viewer_key).await.unwrap();
    let (_, vstate, _) = finished(&mut vev, Some(id)).await;
    assert_eq!(vstate, TransferState::Completed);
    let snap = p.viewer_ft.snapshot(id).unwrap();
    assert!(
        snap.resumed_bytes >= 16 * MIB as u64,
        "resumed only {} bytes",
        snap.resumed_bytes
    );
    assert_eq!(std::fs::read(&final_path).unwrap(), data);
    no_partials(dst.path());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn corrupted_data_is_detected_by_the_hash() {
    let p = pair().await;
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let file = src.path().join("سليم.bin");
    std::fs::write(&file, content(3 * MIB, 3)).unwrap();
    p.viewer_ft.test_corrupt_next_buffer();
    let mut vev = p.viewer_ft.subscribe();
    let mut hev = p.host_ft.subscribe();
    let id = p
        .viewer_ft
        .start_upload(
            p.viewer_key,
            vec![file],
            dst.path().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    let mut item_error = None;
    within("item failure", async {
        loop {
            if let Ok((
                _,
                TransferEvent::ItemFailed {
                    rel_path, error, ..
                },
            )) = hev.recv().await
            {
                item_error = Some((rel_path, error));
                break;
            }
        }
    })
    .await;
    assert_eq!(item_error, Some(("سليم.bin".into(), FsError::HashMismatch)));
    let (_, state, failed) = finished(&mut vev, Some(id)).await;
    assert_eq!((state, failed), (TransferState::Completed, 1));
    // Nothing with the real name, no partial left behind.
    assert!(!dst.path().join("سليم.bin").exists());
    no_partials(dst.path());
}

/// Raw offers with hostile paths are refused before anything touches the disk.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn traversal_attempts_are_rejected() {
    let p = pair().await;
    let outer = tempfile::tempdir().unwrap();
    let dst = outer.path().join("dest");
    std::fs::create_dir(&dst).unwrap();
    let dest = dst.to_str().unwrap().to_string();
    let cases: Vec<(String, &str)> = vec![
        (dest.clone(), "../escaped.txt"),
        (dest.clone(), "a/../../escaped.txt"),
        (dest.clone(), "C:/Windows/evil.dll"),
        (dest.clone(), "/etc/evil"),
        (dest.clone(), "ok.txt:hidden-stream"),
        (dest.clone(), "..\\escaped.txt"),
        (dest.clone(), "NUL"),
        (dest.clone(), "com1.txt"),
        (dest.clone(), "trailing."),
        (format!("{dest}\\..\\"), "escaped.txt"),
        ("relative\\dir".into(), "x.txt"),
    ];
    let mut tap = p.viewer_tap.subscribe();
    for (n, (dest_dir, rel)) in cases.iter().enumerate() {
        let id = 9_000 + n as u64;
        let offer = TransferOffer {
            id,
            direction: TransferDirection::Upload,
            source_dir: "C:\\src".into(),
            dest_dir: dest_dir.clone(),
            items: vec![TransferItem {
                rel_path: rel.to_string(),
                size: 4,
                modified_unix_ms: 0,
                is_dir: false,
            }],
            total_bytes: 4,
            resume: vec![],
        };
        p.viewer_session
            .send(&Message::TransferOffer(offer))
            .await
            .unwrap();
        let reply = within("reject", async {
            loop {
                let m = tap.recv().await.unwrap();
                if matches!(
                    m,
                    Message::TransferReject { .. } | Message::TransferAccept { .. }
                ) {
                    return m;
                }
            }
        })
        .await;
        assert_eq!(
            reply,
            Message::TransferReject {
                id,
                error: FsError::InvalidPath
            },
            "{rel} into {dest_dir}"
        );
    }
    // Nothing was created anywhere.
    assert_eq!(std::fs::read_dir(&dst).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(outer.path()).unwrap().count(), 1);
    assert!(p.host_ft.transfers().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoking_files_stops_the_transfer() {
    let p = pair().await;
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let file = src.path().join("سري.bin");
    std::fs::write(&file, content(64 * MIB, 4)).unwrap();
    p.viewer_ft.test_set_rate_limit(16 * MIB as u64);
    let mut vev = p.viewer_ft.subscribe();
    let mut hev = p.host_ft.subscribe();
    let id = p
        .viewer_ft
        .start_upload(
            p.viewer_key,
            vec![file.clone()],
            dst.path().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    within("some progress", async {
        while !p
            .host_ft
            .transfers()
            .first()
            .is_some_and(|s| s.bytes_done > 0)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    p.set_permissions(Permissions::VIEW);
    let (_, hstate, _) = finished(&mut hev, None).await;
    assert_eq!(hstate, TransferState::Failed(FsError::PermissionDenied));
    let (_, vstate, _) = finished(&mut vev, Some(id)).await;
    assert_eq!(vstate, TransferState::Failed(FsError::PermissionDenied));
    assert!(!dst.path().join("سري.bin").exists());
    // The viewer learned about the revocation: new transfers are refused locally.
    within("viewer permission update", async {
        loop {
            let r = p
                .viewer_ft
                .start_upload(
                    p.viewer_key,
                    vec![file.clone()],
                    dst.path().to_str().unwrap().into(),
                )
                .await;
            if matches!(r, Err(tlp_desk_files::FilesError::PermissionDenied)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_small_files() {
    let p = pair().await;
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let top = src.path().join("صغيرة");
    for d in 0..6 {
        let dir = top.join(format!("مجلد {d}"));
        std::fs::create_dir_all(&dir).unwrap();
        for f in 0..100 {
            std::fs::write(
                dir.join(format!("ملف {f}.txt")),
                content(1 + (d * 100 + f) * 13, f as u64),
            )
            .unwrap();
        }
    }
    let mut vev = p.viewer_ft.subscribe();
    let started = std::time::Instant::now();
    let id = p
        .viewer_ft
        .start_upload(
            p.viewer_key,
            vec![top.clone()],
            dst.path().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    let (_, state, failed) = finished(&mut vev, Some(id)).await;
    assert_eq!((state, failed), (TransferState::Completed, 0));
    eprintln!("600 small files in {:?}", started.elapsed());
    assert_same_tree(&top, &dst.path().join("صغيرة"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conflicts_rename_and_identical_files_are_kept() {
    let p = pair_with(
        TransferConfig {
            conflict: ConflictPolicy::Rename,
            ..TransferConfig::default()
        },
        TransferConfig::default(),
    )
    .await;
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let file = src.path().join("تقرير.txt");
    std::fs::write(&file, "النسخة الأولى").unwrap();
    let dest = dst.path().to_str().unwrap().to_string();
    let mut vev = p.viewer_ft.subscribe();
    for _ in 0..2 {
        let id = p
            .viewer_ft
            .start_upload(p.viewer_key, vec![file.clone()], dest.clone())
            .await
            .unwrap();
        assert_eq!(
            finished(&mut vev, Some(id)).await.1,
            TransferState::Completed
        );
    }
    // Same size and time: the second upload recognised the identical file.
    assert_eq!(std::fs::read_dir(dst.path()).unwrap().count(), 1);
    std::fs::write(&file, "النسخة الثانية!!").unwrap();
    let id = p
        .viewer_ft
        .start_upload(p.viewer_key, vec![file.clone()], dest.clone())
        .await
        .unwrap();
    assert_eq!(
        finished(&mut vev, Some(id)).await.1,
        TransferState::Completed
    );
    assert_eq!(
        std::fs::read_to_string(dst.path().join("تقرير (2).txt")).unwrap(),
        "النسخة الثانية!!"
    );
    assert_eq!(
        std::fs::read_to_string(dst.path().join("تقرير.txt")).unwrap(),
        "النسخة الأولى"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn data_streams_without_a_transfer_are_refused() {
    let p = pair().await;
    let mut s = p
        .viewer_session
        .open_uni(tlp_desk_net::StreamKind::FileData, 424242)
        .await
        .unwrap();
    let _ = s
        .write_all(&tlp_desk_proto::FileDataHeader { item: 0, offset: 0 }.encode())
        .await;
    let _ = s.write_all(&[0u8; 1024]).await;
    let stopped = within("stream refusal", s.stopped()).await;
    assert!(matches!(stopped, Ok(Some(code)) if code == tlp_desk_net::STREAM_REFUSED.into()));
}

fn no_partials(dir: &std::path::Path) {
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().to_string_lossy().to_string();
        assert!(!name.contains(PART_SUFFIX), "leftover {name}");
        if e.file_type().unwrap().is_dir() {
            no_partials(&e.path());
        }
    }
}
