//! Files lane glue for the real backend: the `FsFeature` / `ClipFilesFeature` instances shared
//! by host and viewer sessions, file-system requests for both browser panes, transfer
//! requests, and the mapping of the features' events to [`BackendEvent`]s.

use std::path::PathBuf;
use std::sync::Arc;

use tlp_desk_files::clipfiles::{self, ClipFilesConfig, ClipFilesFeature, RemoteClipboardFiles};
use tlp_desk_files::fs::{
    execute, known_folder, FsConfig, FsFeature, StartError, StartFuture, TransferStarter,
};
use tlp_desk_files::shellclip::{self, ShellClipboard};
use tlp_desk_files::{ConflictPolicy, FileTransfer, FilesError};
use tlp_desk_net::{FeatureEvent, FeatureSet};
use tlp_desk_proto::FsRootKind;
use tokio::runtime::Handle;

use super::files::{FilesNotice, FsFailure, FsOp, FsRequestId, FsSide, TransferRequest};
use super::{BackendEvent, SessionId};

/// Shared by every session of the real backend.
pub struct FilesBridge {
    fs: Arc<FsFeature>,
    clip: Arc<ClipFilesFeature>,
    /// The transfer engine, once one is wired in.
    starter: Option<Arc<dyn TransferStarter>>,
}

impl Default for FilesBridge {
    fn default() -> Self {
        Self::new(FsConfig::default(), ClipFilesConfig::default())
    }
}

impl FilesBridge {
    /// Features with the given host settings and clipboard source.
    pub fn new(fs: FsConfig, clip: ClipFilesConfig) -> Self {
        Self {
            fs: FsFeature::new(fs),
            clip: ClipFilesFeature::new(clip),
            starter: None,
        }
    }

    /// Wires in the transfer engine used by [`FilesBridge::transfer`].
    pub fn with_starter(mut self, starter: Arc<dyn TransferStarter>) -> Self {
        self.starter = Some(starter);
        self
    }

    /// Lets the engine serve pastes of the files announced by this PC's clipboard feature.
    pub fn link_engine(&self, engine: &FileTransfer) {
        let clip = self.clip.clone();
        engine.set_announced_source(Arc::new(move |key, id| {
            let a = clip.announced(key, id)?;
            Some(a.set.source_dir.to_string_lossy().into_owned())
        }));
    }

    /// Makes the peer's copied files pasteable in Explorer: the clipboard feature, the transfer
    /// engine and this PC's clipboard are connected (see `tlp_desk_files::shellclip`). The
    /// clipboard is only touched when a peer announces files.
    pub fn enable_explorer_paste(&self, engine: &Arc<FileTransfer>, policy: ConflictPolicy) {
        shellclip::engine::install(
            &self.clip,
            engine,
            ShellClipboard::system(),
            policy,
            shellclip::engine::default_staging_root(),
        );
    }

    /// `set` plus the files features (registration for host and viewer sessions).
    pub fn register(&self, set: FeatureSet) -> FeatureSet {
        set.with(self.fs.clone()).with(self.clip.clone())
    }

    /// Runs `op` on `side` in the background and hands the answer to `emit`.
    /// `feature_key` is the viewer session's key for a remote request.
    pub fn request(
        &self,
        rt: &Handle,
        side: FsSide,
        feature_key: Option<u64>,
        op: FsOp,
        request: FsRequestId,
        emit: impl FnOnce(BackendEvent) + Send + 'static,
    ) {
        let client = feature_key.and_then(|k| self.fs.client(k));
        rt.spawn(async move {
            let result = match side {
                // This PC: the same code the host runs for its viewers, without an allow-list.
                FsSide::Local => tokio::task::spawn_blocking(move || {
                    execute(&op, &FsConfig::default()).map_err(FsFailure::Fs)
                })
                .await
                .unwrap_or_else(|e| Err(FsFailure::Other(e.to_string()))),
                FsSide::Remote(_) => match client {
                    Some(c) => c.request(op).await.map_err(FsFailure::from),
                    None => Err(FsFailure::Disconnected),
                },
            };
            emit(BackendEvent::FsResult {
                side,
                request,
                result,
            });
        });
    }

    /// Starts `req` for the viewer session `session` (feature key `key`); the outcome goes to
    /// `emit` as a [`BackendEvent::FilesNotice`].
    pub fn transfer(
        &self,
        rt: &Handle,
        session: SessionId,
        key: Option<u64>,
        req: TransferRequest,
        emit: impl Fn(BackendEvent) + Send + 'static,
    ) {
        let notice = move |notice| emit(BackendEvent::FilesNotice { session, notice });
        let Some(key) = key else {
            notice(FilesNotice::Failed(FsFailure::Disconnected));
            return;
        };
        let starter = self.starter.clone();
        let remote_clip = self.clip.remote_files(key);
        rt.spawn(async move {
            let outcome = start(starter.as_deref(), key, req, remote_clip).await;
            notice(match outcome {
                Ok(Some((count, upload))) => FilesNotice::TransferStarted { count, upload },
                Ok(None) => FilesNotice::NothingToPaste,
                Err(e) => FilesNotice::Failed(e.into()),
            });
        });
    }

    /// Maps a feature event of viewer session `session` to a UI event, if it is ours.
    pub fn map_event(session: SessionId, event: &FeatureEvent) -> Option<BackendEvent> {
        if event.feature != clipfiles::FEATURE_NAME {
            return None;
        }
        let files = event.payload::<RemoteClipboardFiles>()?;
        Some(BackendEvent::RemoteClipboardFiles {
            session,
            files: (!files.items.is_empty()).then(|| files.clone()),
        })
    }
}

/// The real [`TransferStarter`]: the transfer engine of the app.
pub struct EngineStarter {
    engine: Arc<FileTransfer>,
    policy: ConflictPolicy,
}

impl EngineStarter {
    /// `policy` decides what happens when a downloaded file already exists locally.
    pub fn new(engine: Arc<FileTransfer>, policy: ConflictPolicy) -> Self {
        Self { engine, policy }
    }
}

fn start_error(e: FilesError) -> StartError {
    match e {
        FilesError::PermissionDenied => StartError::NoPermission,
        other => StartError::Other(other.to_string()),
    }
}

impl TransferStarter for EngineStarter {
    fn upload(&self, key: u64, local: Vec<PathBuf>, remote_dir: String) -> StartFuture<'_> {
        Box::pin(async move {
            let id = self.engine.start_upload(key, local, remote_dir).await;
            id.map(|i| i.0).map_err(start_error)
        })
    }

    fn download(&self, key: u64, remote: Vec<String>, local_dir: PathBuf) -> StartFuture<'_> {
        Box::pin(async move {
            let id = self.engine.start_download(key, remote, local_dir).await;
            id.map(|i| i.0).map_err(start_error)
        })
    }

    fn paste_remote_clipboard<'a>(
        &'a self,
        key: u64,
        files: &'a RemoteClipboardFiles,
        local_dir: PathBuf,
    ) -> StartFuture<'a> {
        Box::pin(async move {
            // The host finds the source folder from the announced id (empty `source_dir`).
            let id = self
                .engine
                .start_download_items(
                    key,
                    Some(files.transfer_id),
                    String::new(),
                    files.items.clone(),
                    local_dir,
                    self.policy,
                )
                .await;
            id.map(|i| i.0).map_err(start_error)
        })
    }
}

/// `Ok(Some((top-level count, upload)))` when started, `Ok(None)` when there was nothing to
/// paste.
async fn start(
    starter: Option<&dyn TransferStarter>,
    key: u64,
    req: TransferRequest,
    remote_clip: Option<RemoteClipboardFiles>,
) -> Result<Option<(usize, bool)>, StartError> {
    let starter = starter.ok_or(StartError::Unavailable)?;
    match req {
        TransferRequest::Upload { local, remote_dir } => {
            let n = local.len();
            starter.upload(key, local, remote_dir).await?;
            Ok(Some((n, true)))
        }
        TransferRequest::Download { remote, local_dir } => {
            let n = remote.len();
            starter.download(key, remote, local_dir).await?;
            Ok(Some((n, false)))
        }
        TransferRequest::PasteRemoteClipboard { local_dir } => {
            let Some(files) = remote_clip else {
                return Ok(None);
            };
            let dir = local_dir
                .or_else(|| known_folder(FsRootKind::Downloads))
                .ok_or_else(|| StartError::Other("no Downloads folder".into()))?;
            starter.paste_remote_clipboard(key, &files, dir).await?;
            Ok(Some((files.top_level_count(), false)))
        }
        TransferRequest::PasteLocalClipboard { remote_dir } => {
            // Reading the clipboard can block briefly (another app holds it open).
            let local: Vec<PathBuf> = tokio::task::spawn_blocking(clipfiles::local_file_list)
                .await
                .unwrap_or_default();
            if local.is_empty() {
                return Ok(None);
            }
            let n = local.len();
            starter.upload(key, local, remote_dir).await?;
            Ok(Some((n, true)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records what the browser asked the transfer engine to do.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);

    impl TransferStarter for Recorder {
        fn upload(&self, _: u64, local: Vec<PathBuf>, remote_dir: String) -> StartFuture<'_> {
            self.0
                .lock()
                .unwrap()
                .push(format!("up {local:?} -> {remote_dir}"));
            Box::pin(async { Ok(1) })
        }
        fn download(&self, _: u64, remote: Vec<String>, local_dir: PathBuf) -> StartFuture<'_> {
            self.0
                .lock()
                .unwrap()
                .push(format!("down {remote:?} -> {}", local_dir.display()));
            Box::pin(async { Ok(2) })
        }
        fn paste_remote_clipboard<'a>(
            &'a self,
            _: u64,
            files: &'a RemoteClipboardFiles,
            local_dir: PathBuf,
        ) -> StartFuture<'a> {
            self.0.lock().unwrap().push(format!(
                "paste {} -> {}",
                files.transfer_id,
                local_dir.display()
            ));
            Box::pin(async { Ok(3) })
        }
    }

    #[tokio::test]
    async fn requests_reach_the_transfer_engine_or_report_unavailable() {
        let rec = Recorder::default();
        let up = TransferRequest::Upload {
            local: vec![PathBuf::from("C:\\ملف.txt")],
            remote_dir: "D:\\مجلد".into(),
        };
        assert_eq!(
            start(None, 1, up.clone(), None).await,
            Err(StartError::Unavailable)
        );
        assert_eq!(start(Some(&rec), 1, up, None).await, Ok(Some((1, true))));
        let paste = TransferRequest::PasteRemoteClipboard {
            local_dir: Some(PathBuf::from("C:\\in")),
        };
        assert_eq!(start(Some(&rec), 1, paste.clone(), None).await, Ok(None));
        let files = RemoteClipboardFiles {
            transfer_id: 1 << 63,
            items: vec![],
        };
        assert_eq!(
            start(Some(&rec), 1, paste, Some(files)).await,
            Ok(Some((0, false)))
        );
        let log = rec.0.lock().unwrap().clone();
        assert_eq!(log.len(), 2);
        assert!(
            log[0].contains("ملف.txt") && log[0].contains("مجلد"),
            "{log:?}"
        );
        assert!(log[1].starts_with("paste 9223372036854775808"), "{log:?}");
    }

    #[tokio::test]
    async fn local_requests_run_on_this_pc() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("تجربة.txt"), "x").unwrap();
        let bridge = FilesBridge::new(FsConfig::default(), ClipFilesConfig::default());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        bridge.request(
            &Handle::current(),
            FsSide::Local,
            None,
            FsOp::ListDir {
                path: tmp.path().to_str().unwrap().into(),
            },
            FsRequestId(9),
            move |e| {
                let _ = tx.send(e);
            },
        );
        let ev = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let BackendEvent::FsResult {
            request,
            result: Ok(tlp_desk_proto::FsReply::Entries(e)),
            ..
        } = ev
        else {
            panic!("unexpected {ev:?}");
        };
        assert_eq!(request, FsRequestId(9));
        assert_eq!(e[0].name, "تجربة.txt");
    }
}
