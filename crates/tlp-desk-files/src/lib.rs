//! File transfer and clipboard sync.
//!
//! * [`clipboard`]: two-way text / HTML / image clipboard sync for a session
//!   ([`ClipboardFeature`], registered in the host's and viewer's `FeatureSet`). The system
//!   clipboard sits behind [`ClipboardBackend`] so tests use [`MemoryClipboard`] and never touch
//!   the user's real clipboard.
//! * [`transfer`]: fast file and folder transfer between viewer and host on parallel QUIC
//!   streams, with resume, BLAKE3 verification and atomic writes. Entry point:
//!   [`FileTransfer`] (a session `Feature`).
//! * [`clipfiles`]: Explorer copy/paste of files between the devices (`ClipboardFiles`).
//! * [`shellclip`]: the peer's copied files as virtual files on this PC's clipboard (Explorer
//!   Ctrl+V), via COM delayed rendering.
//! * [`fs`]: browse and change the other device's files (protocol v4 `FsRequest`/`FsResponse`).
//!
//! No function in this crate contacts anything outside the session's peer.
//! Clipboard contents are never logged: only kinds and sizes.
// `deny`, not `forbid`: the Win32 wrappers `fs::windows` (drive labels, free space, known
// folders) and `clipboard::windows` (change notifications) opt back in, with a
// `// SAFETY:` comment on every block.
#![deny(unsafe_code)]

pub mod clipboard;
pub mod clipfiles;
pub mod fs;
pub mod shellclip;

// --- files-transfer lane ---
pub mod transfer;
pub use transfer::{
    display_path, ConflictChoice, ConflictPolicy, ConflictRequest, ConflictResolution,
    FileConflict, FileRow, FileState, FileTransfer, FilesError, PathIssue, TransferConfig,
    TransferEvent, TransferId, TransferInfo, TransferOrigin, TransferRole, TransferSnapshot,
    TransferState, FEATURE_NAME, META_SUFFIX, PART_SUFFIX, TRANSFER_REQ_ID_BIT,
};
// --- end files-transfer lane ---
pub use clipboard::{
    ClipContent, ClipImage, ClipKind, ClipboardBackend, ClipboardConfig, ClipboardError,
    ClipboardEvent, ClipboardFeature, Direction, LocalFilesHook, MemoryClipboard, RefusedReason,
    SystemClipboard, FEATURE_NAME as CLIPBOARD_FEATURE_NAME, INLINE_MAX,
};
