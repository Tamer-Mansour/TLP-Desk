//! Public types of the transfer engine: ids, configuration, snapshots and events.

use std::path::PathBuf;

use tlp_desk_proto::{FsError, TransferDirection};

/// Local handle of a transfer, unique within the process (across sessions). It stays the
/// same when a paused or failed transfer is resumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TransferId(pub u64);

impl std::fmt::Display for TransferId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What this side does in a transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferRole {
    /// This side reads local files and sends them.
    Sending,
    /// This side receives and writes files.
    Receiving,
}

/// What the receiver does when a file already exists at the destination.
///
/// A file with the same size and modification time as the incoming one is treated as already
/// transferred (its hash is still verified), whatever the policy: that is what makes resuming
/// a folder transfer cheap.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConflictPolicy {
    /// Keep both: the new file becomes `name (2).ext` (default).
    #[default]
    Rename,
    /// Replace the existing file (atomically, after verification).
    Overwrite,
    /// Keep the existing file and skip the incoming one.
    Skip,
    /// Ask the user, once per transfer, before anything is written: the engine emits
    /// [`TransferEvent::ConflictsFound`] and waits for
    /// [`crate::FileTransfer::resolve_conflicts`]. Without conflicts nothing is asked. After
    /// [`TransferConfig::ask_timeout`] without an answer the files are kept both.
    Ask,
}

impl ConflictPolicy {
    /// The policy stored by `policy as u8` (anything unknown is the default, keep both).
    pub(crate) fn from_u8(v: u8) -> Self {
        [Self::Rename, Self::Overwrite, Self::Skip, Self::Ask]
            .into_iter()
            .find(|p| *p as u8 == v)
            .unwrap_or_default()
    }
}

/// Engine settings (one per [`crate::FileTransfer`]).
#[derive(Debug, Clone)]
pub struct TransferConfig {
    /// Parallel data streams per transfer for large files. Small-file transfers use twice as
    /// many (each small file is its own stream). Clamped to `1..=32`.
    pub streams: usize,
    /// Conflict policy for transfers this side receives from the peer (host: uploads).
    /// Default [`ConflictPolicy::Rename`]: the host user is never interrupted unless the app
    /// configures [`ConflictPolicy::Ask`].
    pub conflict: ConflictPolicy,
    /// Conflict policy of downloads this side starts (viewer: files fetched from the host).
    /// Apps use [`ConflictPolicy::Ask`], the command line keeps [`ConflictPolicy::Rename`]
    /// unless told otherwise.
    pub download_conflict: ConflictPolicy,
    /// Host: where uploads go when the viewer leaves the destination empty. `None` = the
    /// user's Desktop (or home folder).
    pub default_upload_dir: Option<PathBuf>,
    /// Maximum files + folders in one transfer.
    pub max_items: usize,
    /// How long a [`ConflictPolicy::Ask`] prompt waits for the user before keeping both files.
    pub ask_timeout: std::time::Duration,
}

impl Default for TransferConfig {
    fn default() -> Self {
        Self {
            streams: 8,
            conflict: ConflictPolicy::Rename,
            download_conflict: ConflictPolicy::Rename,
            default_upload_dir: None,
            max_items: 200_000,
            ask_timeout: std::time::Duration::from_secs(120),
        }
    }
}

/// Description of a transfer, sent with [`TransferEvent::Started`] and in snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferInfo {
    /// Local handle.
    pub id: TransferId,
    /// The session (`FeatureCtx::session_key`) the transfer belongs to.
    pub session_key: u64,
    /// Whether this side sends or receives.
    pub role: TransferRole,
    /// Direction from the viewer's point of view (upload = viewer to host).
    pub direction: TransferDirection,
    /// `true` if this side started it (always the viewer); `false` on the host.
    pub initiated_locally: bool,
    /// Name of the single top-level item, or of the first one when there are several.
    pub label: String,
    /// Number of top-level items the user selected.
    pub top_level_items: u32,
    /// Source folder (on the sending side), for display.
    pub source_dir: String,
    /// Destination folder (on the receiving side), for display.
    pub dest_dir: String,
    /// Sum of all file sizes.
    pub total_bytes: u64,
    /// Number of files.
    pub total_files: u32,
    /// Number of folders.
    pub total_dirs: u32,
    /// When the transfer was created (Unix milliseconds).
    pub started_unix_ms: u64,
}

/// Live state of a transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferState {
    /// Listing files or waiting for the peer to accept.
    Preparing,
    /// Bytes are moving.
    Running,
    /// Stopped by the user; [`crate::FileTransfer::resume`] continues where it stopped.
    Paused,
    /// Every item was transferred and verified (some may have failed, see `failed_items`).
    Completed,
    /// Cancelled by either side.
    Cancelled,
    /// The transfer failed as a whole.
    Failed(FsError),
    /// The session ended before the transfer finished.
    Disconnected,
}

impl TransferState {
    /// Whether the transfer has stopped (successfully or not).
    pub fn is_final(&self) -> bool {
        !matches!(self, Self::Preparing | Self::Running)
    }
}

/// Where one file of a transfer stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    /// Not started.
    Pending,
    /// Bytes are moving.
    Active,
    /// Transferred and verified (or already identical at the destination).
    Done,
    /// Failed (see [`TransferEvent::ItemFailed`]).
    Failed,
    /// Kept as it was at the destination ([`ConflictPolicy::Skip`]).
    Skipped,
}

impl FileState {
    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Active,
            2 => Self::Done,
            3 => Self::Failed,
            4 => Self::Skipped,
            _ => Self::Pending,
        }
    }
}

/// One file of a transfer, for the per-file list (see [`crate::FileTransfer::file_rows`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    /// `/`-separated path relative to the transfer's top-level folder.
    pub rel_path: String,
    /// Size in bytes.
    pub size: u64,
    /// Bytes sent or written so far (including resumed bytes).
    pub done_bytes: u64,
    /// State.
    pub state: FileState,
}

/// Point-in-time view of a transfer (see [`crate::FileTransfer::transfers`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferSnapshot {
    /// What it is.
    pub info: TransferInfo,
    /// Where it stands.
    pub state: TransferState,
    /// Bytes sent (sender) or written (receiver), including resumed bytes.
    pub bytes_done: u64,
    /// Bytes that were already at the receiver when this attempt started (resumed pieces,
    /// identical and skipped files): they were not sent again.
    pub resumed_bytes: u64,
    /// Files finished (verified, skipped or identical).
    pub files_done: u32,
    /// Files that failed.
    pub failed_items: u32,
    /// Recent throughput in bytes per second.
    pub bytes_per_sec: u64,
    /// Receiver only: the local folder the files were written to (for "open folder").
    pub local_dest: Option<PathBuf>,
}

/// Something the UI should show. Emitted as the payload of a `FeatureEvent` named
/// [`crate::FEATURE_NAME`] and on [`crate::FileTransfer::subscribe`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferEvent {
    /// A transfer was created (locally or by the peer).
    Started(TransferInfo),
    /// About four times per second while bytes move.
    Progress {
        /// Which transfer.
        id: TransferId,
        /// Bytes done so far.
        bytes_done: u64,
        /// Total bytes.
        total_bytes: u64,
        /// Files finished.
        files_done: u32,
        /// Total files.
        total_files: u32,
        /// Recent throughput in bytes per second.
        bytes_per_sec: u64,
    },
    /// One file failed; the rest of the transfer continues.
    ItemFailed {
        /// Which transfer.
        id: TransferId,
        /// The file, `/`-separated relative path.
        rel_path: String,
        /// Why.
        error: FsError,
    },
    /// Files already exist at the destination and the policy is [`ConflictPolicy::Ask`]:
    /// show a prompt and answer with [`crate::FileTransfer::resolve_conflicts`]. The transfer
    /// itself starts (event [`TransferEvent::Started`]) only after the answer.
    ConflictsFound(super::conflict::ConflictRequest),
    /// The prompt of this transfer was answered, timed out or its session ended: close it.
    ConflictsResolved(TransferId),
    /// The transfer stopped (see [`TransferState::is_final`]).
    Finished {
        /// Which transfer.
        id: TransferId,
        /// How it ended.
        state: TransferState,
        /// Files that failed.
        failed_items: u32,
        /// Receiver: the folder the files went to.
        local_dest: Option<PathBuf>,
    },
}

impl TransferEvent {
    /// The transfer this event is about.
    pub fn id(&self) -> TransferId {
        match self {
            Self::Started(i) => i.id,
            Self::ConflictsFound(r) => r.id,
            Self::ConflictsResolved(id) => *id,
            Self::Progress { id, .. } | Self::ItemFailed { id, .. } | Self::Finished { id, .. } => {
                *id
            }
        }
    }
}
