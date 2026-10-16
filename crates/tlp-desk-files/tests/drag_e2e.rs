//! Drag-out end to end over a real loopback session: the viewer drags host files out of the
//! file browser. The remote selection is listed through the real transfer engine as soon as the
//! drag starts, and reading a virtual file the way Explorer's stream does downloads the
//! selection into the staging folder. Only the OS drag loop (`DoDragDrop`) is left out.

mod common;

use std::io::Read;
use std::sync::Arc;

use common::*;
use tlp_desk_files::shellclip::drag::{DragEngine, DragOut};
use tlp_desk_files::shellclip::StagedItems;
use tokio::runtime::Handle;

/// Reads virtual file `rel` like Explorer would (waits for the listing and the transfer).
async fn read_virtual(staged: &Arc<StagedItems>, rel: &str) -> Vec<u8> {
    let staged = staged.clone();
    let rel = rel.to_owned();
    tokio::task::spawn_blocking(move || {
        let items = staged.wait_items(&mut || {}).expect("listing").to_vec();
        let idx = items
            .iter()
            .position(|i| i.rel_path == rel)
            .unwrap_or_else(|| panic!("{rel} not listed: {items:?}"));
        let mut f = staged.open(idx, &mut || {}).expect("virtual file");
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).unwrap();
        buf
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dragged_host_folders_are_listed_then_pulled_on_read() {
    let p = pair().await;
    let src = tempfile::tempdir().unwrap();
    let staging = tempfile::tempdir().unwrap();
    let top = make_tree(src.path(), 3 << 20);
    let single = src.path().join("ملف منفرد.txt");
    std::fs::write(&single, "سطر واحد").unwrap();

    let engine: Arc<dyn DragEngine> = p.viewer_ft.clone();
    let drag = DragOut::prepare(
        engine,
        &Handle::current(),
        p.viewer_key,
        vec![
            top.to_str().unwrap().to_owned(),
            single.to_str().unwrap().to_owned(),
        ],
        staging.path(),
    )
    .unwrap();
    let staged = drag.staged().clone();
    let listed = {
        let s = staged.clone();
        tokio::task::spawn_blocking(move || s.wait_items(&mut || {}).map(<[_]>::to_vec))
            .await
            .unwrap()
            .unwrap()
    };
    // Every file and folder is described before any byte moves (the drop target needs the
    // whole tree for its descriptor), including the empty folder.
    for (rel, dir) in [
        ("مجلد المشروع", true),
        ("مجلد المشروع/فارغ", true),
        ("مجلد المشروع/الصور/2024/صورة (1).bin", false),
        ("ملف منفرد.txt", false),
    ] {
        assert!(
            listed.iter().any(|i| i.rel_path == rel && i.is_dir == dir),
            "{rel} missing from {listed:?}"
        );
    }
    assert!(
        p.viewer_ft.transfers().is_empty(),
        "nothing transferred yet"
    );

    assert_eq!(
        read_virtual(&staged, "مجلد المشروع/تقرير نهائي.txt").await,
        "مرحبا بالعالم — hello 123".as_bytes()
    );
    assert_eq!(
        read_virtual(&staged, "مجلد المشروع/كبير.bin").await,
        content(3 << 20, 2)
    );
    assert_eq!(
        read_virtual(&staged, "ملف منفرد.txt").await,
        "سطر واحد".as_bytes()
    );
    assert_eq!(
        p.viewer_ft.transfers().len(),
        1,
        "one download serves the whole drag"
    );

    // The drop target let go: the staging folder is removed.
    let dir = staged.dir().clone();
    drag.abandon();
    for _ in 0..100 {
        if !dir.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }
    assert!(!dir.exists(), "staging folder left behind");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_drag_of_a_missing_host_file_fails_instead_of_hanging() {
    let p = pair().await;
    let staging = tempfile::tempdir().unwrap();
    let engine: Arc<dyn DragEngine> = p.viewer_ft.clone();
    let drag = DragOut::prepare(
        engine,
        &Handle::current(),
        p.viewer_key,
        vec!["C:\\لا يوجد\\ملف.txt".into()],
        staging.path(),
    )
    .unwrap();
    let staged = drag.staged().clone();
    let res = tokio::task::spawn_blocking(move || staged.wait_items(&mut || {}).map(|_| ()))
        .await
        .unwrap();
    assert!(res.is_err());
    drag.abandon();
}
