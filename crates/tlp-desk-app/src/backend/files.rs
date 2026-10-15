//! Files lane (`TLP-files-fs`) types at the backend boundary: remote / local file-system
//! requests for the file browser, Explorer copy/paste of files, transfer requests.

use std::path::PathBuf;

use tlp_desk_files::fs::{FsClientError, StartError};
use tlp_desk_proto::FsError;

pub use tlp_desk_files::clipfiles::RemoteClipboardFiles;
pub use tlp_desk_proto::{FsEntry, FsOp, FsReply, FsRoot, FsRootKind};

use super::SessionId;

/// Which device a file-system request runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FsSide {
    /// This PC.
    Local,
    /// The host of this viewer session.
    Remote(SessionId),
}

/// Identifies one request started with `Backend::fs_request`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FsRequestId(pub u64);

/// Why a file-system request or a transfer request failed (the UI picks the message).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsFailure {
    /// The device answered with this error.
    Fs(FsError),
    /// The session lacks the `FILES` permission.
    NoPermission,
    /// No answer in time.
    Timeout,
    /// The session ended.
    Disconnected,
    /// Transfers are not available (no transfer engine in this build).
    TransfersUnavailable,
    /// Anything else, for the details line.
    Other(String),
}

impl From<FsClientError> for FsFailure {
    fn from(e: FsClientError) -> Self {
        match e {
            FsClientError::Remote(fs) => Self::Fs(fs),
            FsClientError::NoPermission => Self::NoPermission,
            FsClientError::Timeout => Self::Timeout,
            FsClientError::Closed => Self::Disconnected,
            other => Self::Other(other.to_string()),
        }
    }
}

impl From<StartError> for FsFailure {
    fn from(e: StartError) -> Self {
        match e {
            StartError::Unavailable => Self::TransfersUnavailable,
            StartError::NoPermission => Self::NoPermission,
            StartError::Closed => Self::Disconnected,
            StartError::Other(d) => Self::Other(d),
        }
    }
}

/// Something the files UI should tell the user about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilesNotice {
    /// A copy started (`count` top-level entries). Progress belongs to the transfer UI.
    TransferStarted { count: usize, upload: bool },
    /// "Paste" found nothing copied.
    NothingToPaste,
    /// A copy could not be started.
    Failed(FsFailure),
}

/// What to copy where (arguments of `Backend::fs_transfer`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferRequest {
    /// Local files/folders into a folder of the remote PC.
    Upload {
        local: Vec<PathBuf>,
        remote_dir: String,
    },
    /// Remote files/folders (absolute remote paths) into a local folder.
    Download {
        remote: Vec<String>,
        local_dir: PathBuf,
    },
    /// The files copied in Explorer on the remote PC into a local folder (`None` = the
    /// Downloads folder).
    PasteRemoteClipboard { local_dir: Option<PathBuf> },
    /// The files copied in Explorer on this PC into a folder of the remote PC.
    PasteLocalClipboard { remote_dir: String },
}
