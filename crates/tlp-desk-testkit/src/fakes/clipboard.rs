//! An in-memory clipboard.
//!
//! There is no clipboard trait in the workspace yet; the clipboard lane wraps this in its own
//! trait implementation. It mirrors what the OS clipboard holds for us: text, an RGBA image,
//! HTML with a plain-text alternative, or a list of files, plus a change counter like
//! Windows' `GetClipboardSequenceNumber`.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// What the clipboard currently holds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ClipboardContent {
    /// Nothing (or a format we do not sync).
    #[default]
    Empty,
    /// UTF-8 text (Arabic, RTL marks and joiners kept exactly).
    Text(String),
    /// Tightly packed RGBA8 pixels.
    Image {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    /// An HTML fragment and its plain-text alternative.
    Html { html: String, alt_text: String },
    /// Absolute paths of copied files (Arabic names allowed).
    Files(Vec<String>),
}

#[derive(Debug, Default)]
struct State {
    content: ClipboardContent,
    sequence: u64,
}

/// An in-memory clipboard. Clones share state, like two handles to the OS clipboard.
#[derive(Debug, Clone, Default)]
pub struct FakeClipboard(Arc<Mutex<State>>);

impl FakeClipboard {
    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The current content.
    pub fn get(&self) -> ClipboardContent {
        self.state().content.clone()
    }

    /// Replaces the content (like a copy by the local user) and bumps [`Self::sequence`].
    pub fn set(&self, content: ClipboardContent) {
        let mut s = self.state();
        s.content = content;
        s.sequence += 1;
    }

    /// The text, if the clipboard holds text.
    pub fn text(&self) -> Option<String> {
        match self.get() {
            ClipboardContent::Text(t) => Some(t),
            _ => None,
        }
    }

    /// Shorthand for `set(ClipboardContent::Text(..))`.
    pub fn set_text(&self, text: impl Into<String>) {
        self.set(ClipboardContent::Text(text.into()));
    }

    /// Increases on every change; poll it to detect copies.
    pub fn sequence(&self) -> u64 {
        self.state().sequence
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_share_content_and_sequence() {
        let a = FakeClipboard::default();
        let b = a.clone();
        assert_eq!(a.get(), ClipboardContent::Empty);
        b.set_text("نسخ ولصق \u{200F}copy");
        assert_eq!(a.text().as_deref(), Some("نسخ ولصق \u{200F}copy"));
        assert_eq!(a.sequence(), 1);
        a.set(ClipboardContent::Files(vec![
            r"C:\Users\مستخدم\ملف.txt".into()
        ]));
        assert_eq!(b.sequence(), 2);
        assert!(b.text().is_none());
    }
}
