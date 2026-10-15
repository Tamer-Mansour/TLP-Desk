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
}

impl TransferView {
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

/// Why a transfer could not even start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferStartError {
    /// The host did not grant the files permission.
    NoPermission,
    /// Anything else (unreadable path, host path missing, ...), with the engine's message.
    Other(String),
}
