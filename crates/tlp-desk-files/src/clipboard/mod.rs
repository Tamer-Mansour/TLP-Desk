//! Two-way clipboard sync (text, HTML, images) for a session.
//!
//! * [`ClipboardFeature`] implements `tlp_desk_net::Feature`; register the same instance in
//!   `HostConfig::features` and `ViewerConfig::features`. Each session gets one task that
//!   forwards local clipboard changes to the peer and applies the peer's changes locally.
//! * [`ClipboardBackend`] abstracts the OS clipboard: [`SystemClipboard`] (arboard + an
//!   event-driven change listener on Windows) and [`MemoryClipboard`] (tests, CLI).
//! * Wire format (`wire`): `ClipboardData::Text`, `ClipboardHtml`, and PNG-compressed
//!   `ClipboardData::Png` (protocol v5). Payloads above [`INLINE_MAX`] bytes travel on a
//!   `StreamKind::Clipboard` side stream so they never delay input on the control stream.
//! * Loop prevention (`sync`): what we just wrote from the peer is never sent back (OS change
//!   sequence numbers plus content hashes), identical content is not resent, and bursts of
//!   changes are debounced.
//!
//! Every send and every apply checks the session's live `CLIPBOARD` permission and the
//! per-session "Sync clipboard" switch ([`ClipboardFeature::set_sync_enabled`]).
//! Clipboard contents are never logged; events carry only kinds and sizes.
//!
//! Files on the clipboard (`CF_HDROP`) belong to the file-transfer feature: it plugs in with
//! [`ClipboardFeature::set_files_hook`] and receives the local file list through
//! [`LocalFilesHook`].

mod backend;
mod error;
mod feature;
mod memory;
mod sync;
mod system;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;
mod wire;

use std::path::PathBuf;

pub use backend::ClipboardBackend;
pub use error::ClipboardError;
pub use feature::{ClipboardFeature, LocalFilesHook, FEATURE_NAME};
pub use memory::MemoryClipboard;
pub use system::SystemClipboard;
pub use wire::INLINE_MAX;

/// An RGBA8 image (tightly packed, row-major, `width * height * 4` bytes).
#[derive(Clone, PartialEq, Eq)]
pub struct ClipImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for ClipImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Sizes only: pixels may be a screenshot of something private.
        f.debug_struct("ClipImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.rgba.len())
            .finish()
    }
}

/// What is on a clipboard.
#[derive(Clone, PartialEq, Eq)]
pub enum ClipContent {
    /// Plain text, exactly as copied (every code point, CRLF vs LF, RLM/LRM, ZWJ/ZWNJ).
    Text(String),
    /// An HTML fragment plus its plain-text alternative.
    Html { html: String, alt_text: String },
    /// A bitmap.
    Image(ClipImage),
    /// Files copied in Explorer; handled by the file-transfer feature, never sent by this one.
    Files(Vec<PathBuf>),
}

impl ClipContent {
    /// The kind, for events and logs.
    pub fn kind(&self) -> ClipKind {
        match self {
            Self::Text(_) => ClipKind::Text,
            Self::Html { .. } => ClipKind::Html,
            Self::Image(_) => ClipKind::Image,
            Self::Files(_) => ClipKind::Files,
        }
    }

    /// Payload size in bytes (text bytes, HTML + alt text, raw RGBA, number of paths).
    pub fn size(&self) -> u64 {
        let n = match self {
            Self::Text(t) => t.len(),
            Self::Html { html, alt_text } => html.len().saturating_add(alt_text.len()),
            Self::Image(i) => i.rgba.len(),
            Self::Files(f) => f.len(),
        };
        u64::try_from(n).unwrap_or(u64::MAX)
    }
}

impl std::fmt::Debug for ClipContent {
    /// Kind and size only: clipboard contents are never printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ClipContent::{:?}({} bytes)", self.kind(), self.size())
    }
}

/// Kind of clipboard content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClipKind {
    Text,
    Html,
    Image,
    Files,
}

/// Limits and timing of clipboard sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardConfig {
    /// Largest text sent or accepted, in UTF-8 bytes (default 16 MiB; the 16 MiB frame limit
    /// applies on top).
    pub max_text_bytes: u64,
    /// Largest HTML (fragment + alt text) sent or accepted, in bytes (default 16 MiB).
    pub max_html_bytes: u64,
    /// Largest image sent or accepted, as decoded RGBA bytes (default 64 MiB).
    pub max_image_bytes: u64,
    /// Quiet time after a local change before the clipboard is read, so a burst of changes
    /// (apps that set several formats one by one) is sent once.
    pub debounce: std::time::Duration,
    /// Viewer only: send the current clipboard when a session starts (and when sync is
    /// switched back on), so "copy, then connect, then paste" works.
    pub push_on_start: bool,
    /// Whether new sessions start with sync switched on (the viewer's "Sync clipboard"
    /// default from the settings).
    pub enabled_by_default: bool,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            max_text_bytes: 16 << 20,
            max_html_bytes: 16 << 20,
            max_image_bytes: 64 << 20,
            debounce: std::time::Duration::from_millis(150),
            push_on_start: true,
            enabled_by_default: true,
        }
    }
}

/// Which way content moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Our clipboard was sent to the peer.
    Sent,
    /// The peer's clipboard was written to ours.
    Received,
}

/// Why clipboard content was not synced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefusedReason {
    /// Larger than the configured cap (or the protocol frame limit).
    TooLarge,
    /// The peer sent something malformed (bad PNG, sizes that do not match).
    Invalid,
    /// The local clipboard could not be read or written.
    ClipboardError,
}

/// UI payload of the clipboard feature (`FeatureEvent::payload::<ClipboardEvent>()`, feature
/// [`FEATURE_NAME`]). Carries kinds and sizes only, never contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardEvent {
    /// Content was synced (toast "Clipboard sent/received").
    Synced {
        direction: Direction,
        kind: ClipKind,
        bytes: u64,
    },
    /// Content was not synced.
    Refused {
        direction: Direction,
        kind: ClipKind,
        reason: RefusedReason,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_contents() {
        let c = ClipContent::Text("كلمة سر secret".into());
        let s = format!("{c:?}");
        assert!(!s.contains("secret") && !s.contains("كلمة"), "{s}");
        let i = ClipImage {
            width: 1,
            height: 1,
            rgba: vec![7, 7, 7, 7],
        };
        assert_eq!(
            format!("{i:?}"),
            "ClipImage { width: 1, height: 1, bytes: 4 }"
        );
    }

    #[test]
    fn sizes_are_in_bytes() {
        assert_eq!(ClipContent::Text("سلام".into()).size(), 8);
        assert_eq!(
            ClipContent::Html {
                html: "<b>x</b>".into(),
                alt_text: "x".into()
            }
            .size(),
            9
        );
    }
}
