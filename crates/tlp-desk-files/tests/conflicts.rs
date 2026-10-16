//! "Ask" conflict policy over real loopback sessions: the prompt, every answer, cancel,
//! timeout, and the host-side prompt for a viewer's upload. File names are Arabic.

mod common;

use std::path::Path;
use std::time::Duration;

use common::*;
use tlp_desk_files::{
    ConflictChoice, ConflictPolicy, ConflictRequest, ConflictResolution, FileTransfer, FilesError,
    TransferConfig, TransferEvent, TransferState,
};
use tlp_desk_proto::FsError;

fn ask() -> TransferConfig {
    TransferConfig {
        conflict: ConflictPolicy::Ask,
        download_conflict: ConflictPolicy::Ask,
        ..TransferConfig::default()
    }
}

/// Waits for the prompt on `rx`.
async fn prompt(
    rx: &mut tokio::sync::broadcast::Receiver<(u64, TransferEvent)>,
) -> ConflictRequest {
    within("conflict prompt", async {
        loop {
            if let Ok((_, TransferEvent::ConflictsFound(r))) = rx.recv().await {
                return r;
            }
        }
    })
    .await
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{p:?}: {e}"))
}

/// Source: تقرير.txt (new content), صورة.txt (new content), جديد.txt. Destination already
/// holds the first two with old content.
fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    for (n, c) in [
        ("تقرير.txt", "النسخة الجديدة"),
        ("صورة.txt", "new picture!!"),
        ("جديد.txt", "fresh"),
    ] {
        std::fs::write(src.path().join(n), c).unwrap();
    }
    std::fs::write(dst.path().join("تقرير.txt"), "old").unwrap();
    std::fs::write(dst.path().join("صورة.txt"), "old pic").unwrap();
    (src, dst)
}

/// Downloads the three files with the viewer asking, answers with `answer`, returns the
/// final state of the destination folder.
async fn download_with(answer: ConflictResolution) -> (tempfile::TempDir, ConflictRequest) {
    let p = pair_with(TransferConfig::default(), ask()).await;
    let (src, dst) = fixture();
    let remote: Vec<String> = ["تقرير.txt", "صورة.txt", "جديد.txt"]
        .iter()
        .map(|n| src.path().join(n).to_str().unwrap().to_string())
        .collect();
    let mut vev = p.viewer_ft.subscribe();
    let ft = p.viewer_ft.clone();
    let (key, dest) = (p.viewer_key, dst.path().to_path_buf());
    let start = tokio::spawn(async move { ft.start_download(key, remote, dest).await });
    let req = prompt(&mut vev).await;
    assert!(p.viewer_ft.resolve_conflicts(req.id, Some(answer)));
    let id = within("start", start).await.unwrap().unwrap();
    assert_eq!(id, req.id, "the prompt names the transfer that follows");
    assert_eq!(
        finished(&mut vev, Some(id)).await.1,
        TransferState::Completed
    );
    (dst, req)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prompt_lists_only_the_clashes_with_both_sizes() {
    let (dst, req) = download_with(ConflictResolution::all(ConflictChoice::KeepBoth)).await;
    assert!(req.initiated_locally);
    assert_eq!(req.total_conflicts, 2);
    let names: Vec<_> = req.conflicts.iter().map(|c| c.rel_path.as_str()).collect();
    assert_eq!(names, ["تقرير.txt", "صورة.txt"]);
    assert_eq!(req.conflicts[0].existing_size, 3);
    assert_eq!(
        req.conflicts[0].incoming_size,
        "النسخة الجديدة".len() as u64
    );
    // Keep both: old files untouched, new ones next to them.
    assert_eq!(read(&dst.path().join("تقرير.txt")), "old");
    assert_eq!(read(&dst.path().join("تقرير (2).txt")), "النسخة الجديدة");
    assert_eq!(read(&dst.path().join("صورة (2).txt")), "new picture!!");
    assert_eq!(read(&dst.path().join("جديد.txt")), "fresh");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn overwrite_all_replaces_both_files() {
    let (dst, _) = download_with(ConflictResolution::all(ConflictChoice::Overwrite)).await;
    assert_eq!(read(&dst.path().join("تقرير.txt")), "النسخة الجديدة");
    assert_eq!(read(&dst.path().join("صورة.txt")), "new picture!!");
    assert!(!dst.path().join("تقرير (2).txt").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn skip_all_keeps_the_old_files_but_still_copies_new_ones() {
    let (dst, _) = download_with(ConflictResolution::all(ConflictChoice::Skip)).await;
    assert_eq!(read(&dst.path().join("تقرير.txt")), "old");
    assert_eq!(read(&dst.path().join("صورة.txt")), "old pic");
    assert_eq!(read(&dst.path().join("جديد.txt")), "fresh");
    assert_eq!(std::fs::read_dir(dst.path()).unwrap().count(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_file_can_get_its_own_answer() {
    let (dst, _) = download_with(ConflictResolution {
        default: ConflictChoice::Skip,
        per_file: vec![(1, ConflictChoice::Overwrite)],
    })
    .await;
    // Items: 0 = تقرير, 1 = صورة, 2 = جديد.
    assert_eq!(read(&dst.path().join("تقرير.txt")), "old");
    assert_eq!(read(&dst.path().join("صورة.txt")), "new picture!!");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_clash_means_no_question() {
    let p = pair_with(TransferConfig::default(), ask()).await;
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("وحيد.txt"), "x").unwrap();
    let mut vev = p.viewer_ft.subscribe();
    let id = within(
        "start without prompt",
        p.viewer_ft.start_download(
            p.viewer_key,
            vec![src.path().join("وحيد.txt").to_str().unwrap().into()],
            dst.path().to_path_buf(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        finished(&mut vev, Some(id)).await.1,
        TransferState::Completed
    );
    assert_eq!(read(&dst.path().join("وحيد.txt")), "x");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_the_prompt_cancels_the_download() {
    let p = pair_with(TransferConfig::default(), ask()).await;
    let (src, dst) = fixture();
    let mut vev = p.viewer_ft.subscribe();
    let ft = p.viewer_ft.clone();
    let remote = vec![src.path().join("تقرير.txt").to_str().unwrap().to_string()];
    let (key, dest) = (p.viewer_key, dst.path().to_path_buf());
    let start = tokio::spawn(async move { ft.start_download(key, remote, dest).await });
    let req = prompt(&mut vev).await;
    assert!(p.viewer_ft.resolve_conflicts(req.id, None));
    let res = within("start", start).await.unwrap();
    assert!(matches!(res, Err(FilesError::Cancelled)), "{res:?}");
    assert_eq!(read(&dst.path().join("تقرير.txt")), "old");
    assert!(
        !p.viewer_ft.resolve_conflicts(req.id, None),
        "answered once only"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unanswered_prompt_keeps_both_after_the_timeout() {
    let p = pair_with(
        TransferConfig::default(),
        TransferConfig {
            ask_timeout: Duration::from_millis(300),
            ..ask()
        },
    )
    .await;
    let (src, dst) = fixture();
    let mut vev = p.viewer_ft.subscribe();
    let id = p
        .viewer_ft
        .start_download(
            p.viewer_key,
            vec![src.path().join("تقرير.txt").to_str().unwrap().into()],
            dst.path().to_path_buf(),
        )
        .await
        .unwrap();
    assert_eq!(
        finished(&mut vev, Some(id)).await.1,
        TransferState::Completed
    );
    assert_eq!(read(&dst.path().join("تقرير.txt")), "old");
    assert_eq!(read(&dst.path().join("تقرير (2).txt")), "النسخة الجديدة");
}

/// Host-side prompt for a viewer's upload: the host user answers.
async fn upload_to_asking_host(
    answer: Option<ConflictResolution>,
) -> (tempfile::TempDir, TransferState, ConflictRequest) {
    let p = pair_with(ask(), TransferConfig::default()).await;
    let (src, dst) = fixture();
    let mut hev = p.host_ft.subscribe();
    let mut vev = p.viewer_ft.subscribe();
    let id = p
        .viewer_ft
        .start_upload(
            p.viewer_key,
            ["تقرير.txt", "صورة.txt", "جديد.txt"]
                .iter()
                .map(|n| src.path().join(n))
                .collect(),
            dst.path().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    let req = prompt(&mut hev).await;
    assert!(!req.initiated_locally, "the host sees it as incoming");
    assert!(p.host_ft.resolve_conflicts(req.id, answer));
    let (_, state, _) = finished(&mut vev, Some(id)).await;
    (dst, state, req)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_user_can_overwrite_a_viewers_upload() {
    let (dst, state, req) =
        upload_to_asking_host(Some(ConflictResolution::all(ConflictChoice::Overwrite))).await;
    assert_eq!(state, TransferState::Completed);
    assert_eq!(req.total_conflicts, 2);
    assert_eq!(read(&dst.path().join("تقرير.txt")), "النسخة الجديدة");
    assert_eq!(read(&dst.path().join("جديد.txt")), "fresh");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_user_declining_rejects_the_upload() {
    let (dst, state, _) = upload_to_asking_host(None).await;
    assert_eq!(state, TransferState::Failed(FsError::Declined));
    assert_eq!(read(&dst.path().join("تقرير.txt")), "old");
    assert!(!dst.path().join("جديد.txt").exists());
}

#[test]
fn engine_default_stays_keep_both() {
    assert_eq!(TransferConfig::default().conflict, ConflictPolicy::Rename);
    let _ = FileTransfer::new(TransferConfig::default());
}
