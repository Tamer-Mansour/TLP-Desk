//! Remote file system: read and write access to the other device's files.
//!
//! Boundaries: every path from the peer goes through [`path::parse`] (no traversal, devices,
//! alternate data streams or network paths).

pub mod path;
