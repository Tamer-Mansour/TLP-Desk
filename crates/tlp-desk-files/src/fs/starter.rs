//! The seam between the file browser / clipboard paste and the transfer engine.
//!
//! The browser decides *what* to copy where; the transfer code (`TLP-files-transfer` lane,
//! module `transfer`) moves the bytes. The app holds an `Arc<dyn TransferStarter>` and calls it
//! from the UI; every method returns at once (the transfer runs in the background and reports
//! progress through its own events).

use std::path::PathBuf;

use crate::clipfiles::RemoteClipboardFiles;

/// Why a transfer could not be started.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StartError {
    /// No transfer engine is available in this build or session.
    #[error("file transfers are not available")]
    Unavailable,
    /// The session does not hold the `FILES` permission.
    #[error("the host does not allow file access in this session")]
    NoPermission,
    /// The session is gone.
    #[error("the session has ended")]
    Closed,
    /// Anything else (e.g. a local path that cannot be read), for the log.
    #[error("{0}")]
    Other(String),
}

/// Starts transfers for a viewer session (`session_key` = `ViewerSession::feature_key`).
pub trait TransferStarter: Send + Sync {
    /// Copies local files and folders into the folder `remote_dir` of the remote PC. Returns
    /// the transfer id.
    ///
    /// # Errors
    ///
    /// See [`StartError`].
    fn upload(
        &self,
        session_key: u64,
        local_paths: Vec<PathBuf>,
        remote_dir: String,
    ) -> Result<u64, StartError>;

    /// Copies files and folders of the remote PC (absolute remote paths, all in one folder)
    /// into the local folder `local_dir`. Returns the transfer id.
    ///
    /// # Errors
    ///
    /// See [`StartError`].
    fn download(
        &self,
        session_key: u64,
        remote_paths: Vec<String>,
        local_dir: PathBuf,
    ) -> Result<u64, StartError>;

    /// Pastes files copied in Explorer on the remote PC into `local_dir`: a download offer
    /// carrying `files.transfer_id` and `files.items` (the host resolves the source folder).
    ///
    /// # Errors
    ///
    /// See [`StartError`].
    fn paste_remote_clipboard(
        &self,
        session_key: u64,
        files: &RemoteClipboardFiles,
        local_dir: PathBuf,
    ) -> Result<u64, StartError>;
}
