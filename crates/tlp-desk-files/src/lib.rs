//! File transfer and clipboard sync.
//!
//! * [`transfer`]: fast file and folder transfer between viewer and host on parallel QUIC
//!   streams, with resume, BLAKE3 verification and atomic writes. Entry point:
//!   [`FileTransfer`] (a session `Feature`).
//!
//! No function in this crate contacts anything outside the session's peer.
#![forbid(unsafe_code)]

// --- files-transfer lane ---
pub mod transfer;
pub use transfer::{
    ConflictPolicy, FileTransfer, FilesError, PathIssue, TransferConfig, TransferEvent, TransferId,
    TransferInfo, TransferRole, TransferSnapshot, TransferState, FEATURE_NAME, META_SUFFIX,
    PART_SUFFIX, TRANSFER_REQ_ID_BIT,
};
// --- end files-transfer lane ---
