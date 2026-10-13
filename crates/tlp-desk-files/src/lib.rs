//! File transfer and clipboard sync.
//!
//! * [`clipboard`]: two-way text / HTML / image clipboard sync for a session
//!   ([`ClipboardFeature`], registered in the host's and viewer's `FeatureSet`). The system
//!   clipboard sits behind [`ClipboardBackend`] so tests use [`MemoryClipboard`] and never touch
//!   the user's real clipboard.
//!
//! Boundaries: this crate never logs clipboard or file contents; every feature is gated by the
//! session's live `Permissions`. `unsafe` is denied workspace-wide and only allowed in the
//! Windows platform module (`clipboard/windows.rs`, clipboard change notifications).

pub mod clipboard;
mod error;

pub use clipboard::{
    ClipContent, ClipImage, ClipKind, ClipboardBackend, ClipboardConfig, ClipboardEvent,
    ClipboardFeature, Direction, LocalFilesHook, MemoryClipboard, RefusedReason, FEATURE_NAME,
    INLINE_MAX,
};
pub use error::FilesError;
