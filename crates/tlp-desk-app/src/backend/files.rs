//! UI-facing view of file transfers (lane files-transfer). The engine lives in `tlp-desk-files`;
//! the backend turns its snapshots into these plain types so the UI never depends on it.

use std::path::PathBuf;

pub use tlp_desk_proto::FsError;

/// Identifies one transfer inside a session (stable across retries).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TransferId(pub u64);

/// Which way the bytes go, seen from this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDir {
    /// From this device to the host.
    Send,
    /// From the host to this device.
    Receive,
}

/// Where a transfer stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferStatus {
    /// Listing files or waiting for the other side.
    Preparing,
    /// Bytes are moving.
    Running,
    /// Stopped by the user; retry continues where it stopped.
    Paused,
    /// Done (see `failed_files` for files that did not make it).
    Completed,
    /// Cancelled by either side.
    Cancelled,
    /// Failed as a whole.
    Failed(FsError),
    /// The session ended first.
    Disconnected,
}

impl TransferStatus {
    /// Whether the transfer has stopped, successfully or not.
    pub fn is_final(&self) -> bool {
        !matches!(self, Self::Preparing | Self::Running)
    }

    /// Whether "retry" makes sense (resumes from the partial data).
    pub fn can_retry(&self) -> bool {
        matches!(
            self,
            Self::Paused | Self::Cancelled | Self::Failed(_) | Self::Disconnected
        )
    }
}

/// Everything the transfer queue shows about one transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferView {
    /// Which transfer.
    pub id: TransferId,
    /// Direction seen from this device.
    pub dir: TransferDir,
    /// Short name: the file or folder name, or "3 items".
    pub label: String,
    /// State.
    pub status: TransferStatus,
    /// Bytes done (including resumed bytes).
    pub bytes_done: u64,
    /// Total bytes.
    pub total_bytes: u64,
    /// Files finished.
    pub files_done: u32,
    /// Total files.
    pub total_files: u32,
    /// Files that failed.
    pub failed_files: u32,
    /// Recent speed in bytes per second.
    pub bytes_per_sec: u64,
    /// Receiving: the local folder the files go to (for "open folder").
    pub local_dest: Option<PathBuf>,
    /// `true` if this device started the transfer (as the viewer), `false` if the other side
    /// did (files arriving at, or taken from, this host).
    pub local: bool,
    /// When it was created (Unix milliseconds), for ordering by age.
    pub started_unix_ms: u64,
}

impl TransferView {
    /// Whether "pause" applies: only transfers this device started can be paused and resumed.
    pub fn can_pause(&self) -> bool {
        self.local && matches!(self.status, TransferStatus::Running)
    }

    /// Estimated seconds left, if the speed is known.
    pub fn eta_secs(&self) -> Option<u64> {
        self.total_bytes
            .saturating_sub(self.bytes_done)
            .checked_div(self.bytes_per_sec)
    }

    /// Progress from 0.0 to 1.0.
    pub fn fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            return if self.status == TransferStatus::Completed {
                1.0
            } else {
                0.0
            };
        }
        (self.bytes_done as f64 / self.total_bytes as f64).clamp(0.0, 1.0) as f32
    }
}

/// A question the engine needs answered before it copies: files with these names already
/// exist at the destination (see [`ConflictResolution`] for the answer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictPrompt {
    /// The transfer that waits.
    pub id: TransferId,
    /// `true` if this device started the transfer, `false` for a viewer's upload to this host.
    pub local: bool,
    /// Name of the first top-level item.
    pub label: String,
    /// The destination folder.
    pub dest_dir: String,
    /// How many files clash (can exceed `files.len()`).
    pub total: u32,
    /// The first clashes, with both versions' size and time.
    pub files: Vec<FileConflict>,
}

pub use tlp_desk_files::{ConflictChoice, ConflictResolution, FileConflict, FileRow, FileState};

/// The UI form of an engine question.
pub fn conflict_prompt(req: &tlp_desk_files::ConflictRequest) -> ConflictPrompt {
    ConflictPrompt {
        id: TransferId(req.id.0),
        local: req.initiated_locally,
        label: req.label.clone(),
        dest_dir: req.dest_dir.clone(),
        total: req.total_conflicts,
        files: req.conflicts.clone(),
    }
}

/// Why a transfer could not even start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferStartError {
    /// The host did not grant the files permission.
    NoPermission,
    /// Anything else (unreadable path, host path missing, ...), with the engine's message.
    Other(String),
}

// ---------------------------------------------------------------------------------------------
// Remote file browser, Explorer copy/paste of files (lane files-fs).
// ---------------------------------------------------------------------------------------------

use tlp_desk_files::fs::{FsClientError, StartError};

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
