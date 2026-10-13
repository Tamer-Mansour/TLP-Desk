//! Fast file and folder transfer between the viewer and the host.
//!
//! Building blocks (path rules, piece hashing, disk helpers, local listing, receiver
//! planning); the engine that uses them follows.
// Wired into the engine in the following commits.
#![allow(dead_code)]

mod disk;
mod error;
mod hash;
mod path;
mod plan;
mod scan;
mod types;

pub use error::{FilesError, PathIssue};
pub use hash::PIECE;
pub use path::{validate_component, validate_rel_path, MAX_DEPTH};
pub use plan::{META_SUFFIX, PART_SUFFIX};
pub use types::{
    ConflictPolicy, TransferConfig, TransferEvent, TransferId, TransferInfo, TransferRole,
    TransferSnapshot, TransferState,
};
