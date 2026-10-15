//! Loop prevention and dedupe, kept free of I/O so it is easy to test.
//!
//! Two independent guards stop an echo (peer sends X, we write X, our clipboard reports a
//! change, we would send X back):
//!
//! 1. **Change sequence numbers** ([`OwnWrites`]): every write we make records the clipboard's
//!    sequence number right after it; a change notification carrying one of those numbers is
//!    ours and is ignored. Shared by all sessions of one clipboard, so content a viewer pasted
//!    into the host is not forwarded to other viewers either.
//! 2. **Content hashes** ([`SyncState`]): content equal to what the peer last sent us, or to
//!    what we last sent, is not sent (covers backends without sequence numbers and apps that
//!    re-set the same data).

use std::collections::VecDeque;
use std::hash::{DefaultHasher, Hash, Hasher};

use super::ClipContent;

/// How many of our own recent writes are remembered.
const OWN_WRITES: usize = 16;

/// A stable 64-bit fingerprint of clipboard content (kind + exact bytes). Not cryptographic:
/// it only decides whether to skip a send.
pub(crate) fn content_hash(c: &ClipContent) -> u64 {
    let mut h = DefaultHasher::new();
    match c {
        ClipContent::Text(t) => (0u8, t.as_bytes()).hash(&mut h),
        ClipContent::Html { html, alt_text } => {
            (1u8, html.as_bytes(), alt_text.as_bytes()).hash(&mut h);
        }
        ClipContent::Image(i) => (2u8, i.width, i.height, i.rgba.as_slice()).hash(&mut h),
        ClipContent::Files(f) => (3u8, f).hash(&mut h),
    }
    h.finish()
}

/// Sequence numbers of the clipboard right after our own writes.
#[derive(Debug, Default)]
pub(crate) struct OwnWrites(VecDeque<u64>);

impl OwnWrites {
    pub(crate) fn record(&mut self, seq: u64) {
        if self.0.len() >= OWN_WRITES {
            self.0.pop_front();
        }
        self.0.push_back(seq);
    }

    /// Whether the change with this sequence number was made by us.
    pub(crate) fn is_own(&self, seq: u64) -> bool {
        self.0.contains(&seq)
    }
}

/// Per-session dedupe state.
#[derive(Debug, Default)]
pub(crate) struct SyncState {
    last_sent: Option<u64>,
    last_received: Option<u64>,
}

impl SyncState {
    /// Decides whether local content with this hash goes to the peer, and records it if so.
    pub(crate) fn should_send(&mut self, hash: u64) -> bool {
        if self.last_received == Some(hash) || self.last_sent == Some(hash) {
            return false;
        }
        self.last_sent = Some(hash);
        // The peer now holds what we sent; copying its earlier content again is a real change.
        self.last_received = None;
        true
    }

    /// Records content the peer sent us (and we wrote locally).
    pub(crate) fn received(&mut self, hash: u64) {
        self.last_received = Some(hash);
        self.last_sent = None;
    }

    /// Forgets what was sent (sync switched back on: resend the current clipboard).
    pub(crate) fn reset_sent(&mut self) {
        self.last_sent = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ClipImage;

    fn text(s: &str) -> ClipContent {
        ClipContent::Text(s.into())
    }

    #[test]
    fn received_content_is_not_echoed() {
        let mut s = SyncState::default();
        let x = content_hash(&text("من الطرف الآخر"));
        s.received(x);
        assert!(!s.should_send(x), "echo of received content");
        let y = content_hash(&text("نسخ محلي"));
        assert!(s.should_send(y));
        // After a local send, copying the earlier remote content again is a real change.
        assert!(s.should_send(x));
    }

    #[test]
    fn identical_content_is_sent_once() {
        let mut s = SyncState::default();
        let x = content_hash(&text("abc"));
        assert!(s.should_send(x));
        assert!(!s.should_send(x));
        s.reset_sent();
        assert!(s.should_send(x));
    }

    #[test]
    fn hash_distinguishes_every_code_point_and_kind() {
        let base = content_hash(&text("لا مي"));
        assert_ne!(base, content_hash(&text("لا\u{200C}مي")));
        assert_ne!(base, content_hash(&text("لا\u{200D}مي")));
        assert_ne!(content_hash(&text("a\r\nb")), content_hash(&text("a\nb")));
        assert_ne!(
            content_hash(&text("مَرحبا")),
            content_hash(&text("مرحبا")),
            "harakat matter"
        );
        assert_ne!(
            content_hash(&text("x")),
            content_hash(&ClipContent::Html {
                html: "x".into(),
                alt_text: String::new()
            })
        );
        let img = |w| {
            content_hash(&ClipContent::Image(ClipImage {
                width: w,
                height: 4 / w,
                rgba: vec![0; 16],
            }))
        };
        assert_ne!(img(1), img(2), "same pixels, different shape");
        assert_eq!(base, content_hash(&text("لا مي")), "stable");
    }

    #[test]
    fn own_writes_are_recognized_and_bounded() {
        let mut o = OwnWrites::default();
        for seq in 0..40 {
            o.record(seq);
        }
        assert!(o.is_own(39));
        assert!(o.is_own(24));
        assert!(!o.is_own(23), "only the last {OWN_WRITES} are kept");
        assert!(!o.is_own(100));
    }
}
