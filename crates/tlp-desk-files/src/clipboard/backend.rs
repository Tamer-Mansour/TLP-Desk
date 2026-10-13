//! The clipboard abstraction the sync feature talks to.

use tokio::sync::watch;

use super::ClipContent;
use crate::FilesError;

/// A clipboard: the OS one ([`super::SystemClipboard`]) or an in-memory fake
/// ([`super::MemoryClipboard`]).
///
/// `read` and `write` may block briefly (the OS clipboard is a shared lock); the feature calls
/// them from `spawn_blocking`.
pub trait ClipboardBackend: Send + Sync + 'static {
    /// The current content in the richest format this crate syncs (HTML, then text, then
    /// image; file lists are reported as [`ClipContent::Files`]). `Ok(None)` when the
    /// clipboard is empty or holds only formats we do not sync.
    ///
    /// # Errors
    /// [`FilesError::Clipboard`] if the clipboard cannot be opened or read.
    fn read(&self) -> Result<Option<ClipContent>, FilesError>;

    /// Replaces the clipboard with `content` and returns the change sequence number the
    /// clipboard has right after the write, so the change notification caused by our own
    /// write can be recognized and not sent back.
    ///
    /// # Errors
    /// [`FilesError::Clipboard`] if the clipboard cannot be opened or written.
    fn write(&self, content: &ClipContent) -> Result<u64, FilesError>;

    /// Change notifications: the value is the clipboard's change sequence number and is
    /// updated on every change. `None` if this backend cannot notify (local changes are then
    /// not synced, received ones still are).
    fn changes(&self) -> Option<watch::Receiver<u64>>;
}
