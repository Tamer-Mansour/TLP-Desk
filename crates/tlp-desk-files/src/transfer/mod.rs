//! Fast file and folder transfer between the viewer and the host.
//!
//! This commit adds the building blocks (path rules, piece hashing, disk helpers); the
//! engine that uses them follows.
// Wired into the engine in the following commits.
#![allow(dead_code)]

mod disk;
mod error;
mod hash;
mod path;

pub use error::{FilesError, PathIssue};
pub use hash::PIECE;
pub use path::{validate_component, validate_rel_path, MAX_DEPTH};
