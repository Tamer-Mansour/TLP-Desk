//! Clipboard sync wiring for the real backend (TLP-clipboard): the shared feature instance
//! and the mapping of its feature events to [`BackendEvent::Clipboard`].

use std::sync::Arc;

use tlp_desk_files::{
    ClipboardBackend, ClipboardConfig, ClipboardEvent, ClipboardFeature, MemoryClipboard,
    FEATURE_NAME,
};
use tlp_desk_net::{FeatureEvent, FeatureSet};

use super::{BackendEvent, SessionId};

/// The clipboard feature shared by every host and viewer session. Without an explicit
/// clipboard (tests) it syncs a private in-memory clipboard, never the user's real one.
pub(super) fn feature(clipboard: Option<Arc<dyn ClipboardBackend>>) -> Arc<ClipboardFeature> {
    let clipboard = clipboard.unwrap_or_else(|| Arc::new(MemoryClipboard::new()));
    ClipboardFeature::new(clipboard, ClipboardConfig::default())
}

/// `set` plus the clipboard feature.
pub(super) fn register(set: FeatureSet, feature: &Arc<ClipboardFeature>) -> FeatureSet {
    set.with(feature.clone())
}

/// The UI event for a clipboard feature event, if `event` is one.
pub(super) fn map(session: SessionId, event: &FeatureEvent) -> Option<BackendEvent> {
    if event.feature != FEATURE_NAME {
        return None;
    }
    event
        .payload::<ClipboardEvent>()
        .map(|e| BackendEvent::Clipboard {
            session,
            event: e.clone(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tlp_desk_files::{ClipKind, Direction};

    #[test]
    fn maps_only_clipboard_events() {
        let ev = ClipboardEvent::Synced {
            direction: Direction::Received,
            kind: ClipKind::Text,
            bytes: 4,
        };
        let fe = FeatureEvent::new(FEATURE_NAME, 3, ev.clone());
        match map(SessionId(9), &fe) {
            Some(BackendEvent::Clipboard { session, event }) => {
                assert_eq!(session, SessionId(9));
                assert_eq!(event, ev);
            }
            other => panic!("unexpected {other:?}"),
        }
        let chat = FeatureEvent::new("chat", 3, String::from("مرحبا"));
        assert!(map(SessionId(9), &chat).is_none());
    }
}
