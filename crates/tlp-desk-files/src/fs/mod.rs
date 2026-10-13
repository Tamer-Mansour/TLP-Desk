//! Remote file system: read and write access to the other device's files.
//!
//! * Host side: the [`FsFeature`] serves protocol v4 [`FsRequest`](tlp_desk_proto::Message::FsRequest)s
//!   (`Roots`, `ListDir`, `Stat`, `MakeDir`, `Rename`, `Delete`) on the host's disks, only
//!   while the session holds the `FILES` permission (checked live, per request).
//! * Viewer side: [`FsFeature::client`] hands out an [`FsClient`] per session that sends
//!   requests with ids and waits for the answers with a timeout.
//! * [`TransferStarter`]: the seam to the transfer engine for upload / download / paste.
//! * [`execute`] runs one [`FsOp`](tlp_desk_proto::FsOp) on *this* device (the app's local
//!   pane uses it too, so both panes behave the same).
//!
//! Boundaries: every path from the peer goes through [`path::parse`] (no traversal, devices,
//! alternate data streams or network paths) and the host's optional allow-list
//! ([`FsConfig::allowed_roots`]). A viewer never serves its own disks to the host.

mod client;
mod feature;
mod ops;
pub mod path;
mod roots;
mod starter;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

use std::path::PathBuf;

pub use client::{FsClient, FsClientError, DEFAULT_TIMEOUT};
pub use feature::{FsFeature, FEATURE_NAME};
pub use ops::{entry_order, execute, map_io, sort_entries, unix_ms, MAX_LIST_ENTRIES};
pub use roots::known_folder;
pub use starter::{StartError, TransferStarter};

/// Host-side settings of the remote file system.
#[derive(Debug, Clone, Default)]
pub struct FsConfig {
    /// Folders a viewer may browse, with everything below them (links are resolved, so a
    /// junction cannot lead outside). Empty (the default) = every local drive; network
    /// drives and UNC paths are never served.
    pub allowed_roots: Vec<PathBuf>,
}
