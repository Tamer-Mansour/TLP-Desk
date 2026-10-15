//! The crate's error type for file transfers.

use std::path::PathBuf;

use tlp_desk_net::NetError;
use tlp_desk_proto::FsError;

/// Why a relative transfer path was refused (see [`crate::validate_rel_path`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathIssue {
    /// The path or one of its components is empty (`""`, `a//b`, trailing `/`).
    Empty,
    /// Starts with `/` or a drive / UNC prefix.
    Absolute,
    /// A `..` component (would escape the destination).
    ParentRef,
    /// A `.` component.
    CurDir,
    /// Contains `\`, which Windows treats as a separator.
    Backslash,
    /// Contains `:` (drive letters, NTFS alternate data streams).
    Colon,
    /// A character Windows forbids in file names (`<>"|?*`, control characters, NUL).
    InvalidChar,
    /// Ends with `.` or a space (Windows strips them, so two names could alias).
    TrailingDotOrSpace,
    /// A reserved device name (`CON`, `NUL`, `COM1`, `LPT1`, ...), with or without extension.
    Reserved,
    /// The path or a component is longer than the limits.
    TooLong,
    /// More nested directories than [`crate::transfer::MAX_DEPTH`].
    TooDeep,
    /// The resolved path is outside the destination directory (e.g. through a junction).
    Escapes,
    /// The local name is not valid Unicode and cannot be sent losslessly.
    NotUnicode,
    /// Two items of one transfer have the same path (case-insensitive on Windows).
    Duplicate,
}

/// Errors of the file-transfer engine.
#[derive(Debug, thiserror::Error)]
pub enum FilesError {
    /// A file-system operation failed.
    #[error("i/o error on {path:?}: {source}")]
    Io {
        /// The path the operation was about.
        path: PathBuf,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// A path from the peer or the user was refused.
    #[error("invalid path {path:?}: {issue:?}")]
    InvalidPath {
        /// The offending path (as received or given).
        path: String,
        /// What was wrong with it.
        issue: PathIssue,
    },
    /// No live session with this key has the transfer feature running.
    #[error("no session with key {0}")]
    NoSession(u64),
    /// No transfer with this id.
    #[error("no transfer with id {0}")]
    NoTransfer(u64),
    /// The session does not hold the `FILES` permission (or it was revoked).
    #[error("the session lacks the files permission")]
    PermissionDenied,
    /// The selection holds more files and folders than one transfer may carry.
    #[error("{count} items exceed the limit of {max} per transfer")]
    TooManyItems {
        /// Items found so far.
        count: usize,
        /// The limit.
        max: usize,
    },
    /// The destination volume does not have room for the data.
    #[error("not enough free space: {need} bytes needed, {free} available")]
    NoSpace {
        /// Bytes still to be written.
        need: u64,
        /// Bytes available to this user on the volume.
        free: u64,
    },
    /// The peer refused or failed the transfer.
    #[error("the peer reported {0:?}")]
    Remote(FsError),
    /// The transfer was cancelled.
    #[error("transfer cancelled")]
    Cancelled,
    /// The received data did not match the sender's BLAKE3 hash.
    #[error("received data does not match its blake3 hash")]
    HashMismatch,
    /// The peer broke the transfer protocol (bad item index, overlapping ranges, ...).
    #[error("protocol violation: {0}")]
    Protocol(String),
    /// The session's network layer failed.
    #[error("network error: {0}")]
    Net(#[from] NetError),
}

impl FilesError {
    /// Wraps an I/O error with the path it concerns.
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    /// The wire form sent to the peer in `TransferReject` / `TransferError`.
    pub fn to_fs_error(&self) -> FsError {
        match self {
            Self::Io { source, .. } => io_to_fs_error(source),
            Self::InvalidPath { .. } | Self::TooManyItems { .. } => FsError::InvalidPath,
            Self::PermissionDenied => FsError::PermissionDenied,
            Self::NoSpace { .. } => FsError::NoSpace,
            Self::Remote(e) => e.clone(),
            Self::Cancelled => FsError::Cancelled,
            Self::HashMismatch => FsError::HashMismatch,
            Self::NoSession(_) | Self::NoTransfer(_) => FsError::NotFound,
            Self::Protocol(m) => FsError::Other(m.clone()),
            Self::Net(e) => FsError::Other(e.to_string()),
        }
    }
}

/// Maps an OS error to the closest wire error.
pub(crate) fn io_to_fs_error(e: &std::io::Error) -> FsError {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::NotFound => FsError::NotFound,
        K::PermissionDenied => FsError::PermissionDenied,
        K::AlreadyExists => FsError::AlreadyExists,
        K::StorageFull => FsError::NoSpace,
        K::DirectoryNotEmpty => FsError::NotEmpty,
        K::InvalidFilename => FsError::InvalidPath,
        K::ResourceBusy => FsError::Busy,
        _ => match e.raw_os_error() {
            // ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION: the file is open elsewhere.
            Some(32) | Some(33) if cfg!(windows) => FsError::Busy,
            // ERROR_DISK_FULL, ERROR_HANDLE_DISK_FULL.
            Some(112) | Some(39) if cfg!(windows) => FsError::NoSpace,
            _ => FsError::Other(e.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_errors_map_to_wire_errors() {
        let nf = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(io_to_fs_error(&nf), FsError::NotFound);
        let e = FilesError::io(
            "C:\\Ù…Ù„Ù.txt",
            std::io::ErrorKind::PermissionDenied.into(),
        );
        assert_eq!(e.to_fs_error(), FsError::PermissionDenied);
        assert_eq!(
            FilesError::HashMismatch.to_fs_error(),
            FsError::HashMismatch
        );
    }
}
