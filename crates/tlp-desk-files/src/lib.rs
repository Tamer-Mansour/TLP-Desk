//! File transfer and clipboard sync.
//!
//! * [`transfer`]: fast file and folder transfer between viewer and host on parallel QUIC
//!   streams, with resume, BLAKE3 verification and atomic writes. Entry point:
//!   [`FileTransfer`] (a session `Feature`).
//! * [`clipfiles`]: Explorer copy/paste of files between the devices (`ClipboardFiles`).
//! * [`fs`]: browse and change the other device's files (protocol v4 `FsRequest`/`FsResponse`).
//!
//! No function in this crate contacts anything outside the session's peer.
// `deny`, not `forbid`: only the Win32 wrapper `fs::windows` opts back in (drive labels, free
// space, known folders), with a `// SAFETY:` comment on every block.
#![deny(unsafe_code)]

pub mod clipfiles;
pub mod fs;

// --- files-transfer lane ---
pub mod transfer;
pub use transfer::{
    ConflictPolicy, FileTransfer, FilesError, PathIssue, TransferConfig, TransferEvent, TransferId,
    TransferInfo, TransferRole, TransferSnapshot, TransferState, FEATURE_NAME, META_SUFFIX,
    PART_SUFFIX, TRANSFER_REQ_ID_BIT,
};
// --- end files-transfer lane ---
