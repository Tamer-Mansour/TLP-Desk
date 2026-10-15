//! Chat types at the backend boundary (TLP-chat), and the glue between the real backend and
//! the [`ChatFeature`]s of its host and viewer sides.

use std::sync::Arc;

use tlp_desk_host::HostConfig;
use tlp_desk_net::{
    ChatAuthor, ChatEntry, ChatEvent, ChatFeature, ChatRejected, FeatureEvent, CHAT_FEATURE,
};
use tlp_desk_viewer::ViewerConfig;

use super::{BackendEvent, SessionId};

/// One chat message for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatLine {
    /// Id chosen by its author (unique per author and session).
    pub id: u64,
    /// Written by the local user.
    pub mine: bool,
    /// Exactly as written (logical-order UTF-8).
    pub text: String,
    /// Our clock when it was sent or received, in Unix milliseconds.
    pub at_unix_ms: u64,
}

impl From<&ChatEntry> for ChatLine {
    fn from(e: &ChatEntry) -> Self {
        Self {
            id: e.id,
            mine: e.author == ChatAuthor::Me,
            text: e.text.clone(),
            at_unix_ms: e.local_unix_ms,
        }
    }
}

/// Why a chat message was not sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatSendError {
    /// No chat in this session (it ended, or the backend has no chat).
    Unavailable,
    /// The host turned chat off for this session.
    NotAllowed,
    /// Nothing but whitespace.
    Empty,
    /// More than `max` characters.
    TooLong { max: usize },
    /// Sending too fast; try again in a moment.
    RateLimited,
}

impl From<ChatRejected> for ChatSendError {
    fn from(r: ChatRejected) -> Self {
        match r {
            ChatRejected::NoSession => Self::Unavailable,
            ChatRejected::NotAllowed => Self::NotAllowed,
            ChatRejected::Empty => Self::Empty,
            ChatRejected::TooLong { max, .. } => Self::TooLong { max },
            ChatRejected::RateLimited { .. } => Self::RateLimited,
        }
    }
}

/// Longest message the UI lets the user send, in characters.
pub fn max_chat_chars() -> usize {
    tlp_desk_net::ChatLimits::default().max_chars
}

/// The chat features of the real backend: one per role (their session keys may collide).
#[derive(Debug, Clone)]
pub struct ChatBridge {
    host: Arc<ChatFeature>,
    viewer: Arc<ChatFeature>,
}

impl Default for ChatBridge {
    fn default() -> Self {
        Self {
            host: ChatFeature::new(),
            viewer: ChatFeature::new(),
        }
    }
}

impl ChatBridge {
    /// Registers the host-side chat for every incoming session.
    pub fn register_host(&self, cfg: &mut HostConfig) {
        cfg.features.push(self.host.clone());
    }

    /// Registers the viewer-side chat for every outgoing session.
    pub fn register_viewer(&self, cfg: &mut ViewerConfig) {
        cfg.features.push(self.viewer.clone());
    }

    /// Sends in a host session (`key` = the host's session id).
    pub fn send_host(&self, key: u64, text: &str) -> Result<ChatLine, ChatSendError> {
        Ok(ChatLine::from(&self.host.send(key, text)?))
    }

    /// Sends in a viewer session (`key` = `ViewerSession::feature_key`).
    pub fn send_viewer(&self, key: u64, text: &str) -> Result<ChatLine, ChatSendError> {
        Ok(ChatLine::from(&self.viewer.send(key, text)?))
    }
}

/// The UI event for a chat [`FeatureEvent`] of `session`; `None` if it is not chat.
pub fn backend_event(session: SessionId, event: &FeatureEvent) -> Option<BackendEvent> {
    if event.feature != CHAT_FEATURE {
        return None;
    }
    Some(match event.payload::<ChatEvent>()? {
        ChatEvent::Received(e) => BackendEvent::ChatReceived {
            session,
            line: ChatLine::from(e),
        },
        ChatEvent::Availability(allowed) => BackendEvent::ChatAvailability {
            session,
            allowed: *allowed,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_events_map_to_backend_events() {
        let entry = ChatEntry {
            id: 3,
            author: ChatAuthor::Peer,
            text: "مرحبا PC-01".into(),
            sent_unix_ms: 1,
            local_unix_ms: 2,
        };
        let ev = FeatureEvent::new(CHAT_FEATURE, 7, ChatEvent::Received(entry));
        match backend_event(SessionId(9), &ev) {
            Some(BackendEvent::ChatReceived { session, line }) => {
                assert_eq!(session, SessionId(9));
                assert_eq!(
                    line,
                    ChatLine {
                        id: 3,
                        mine: false,
                        text: "مرحبا PC-01".into(),
                        at_unix_ms: 2
                    }
                );
            }
            other => panic!("{other:?}"),
        }
        let ev = FeatureEvent::new(CHAT_FEATURE, 7, ChatEvent::Availability(false));
        assert!(matches!(
            backend_event(SessionId(9), &ev),
            Some(BackendEvent::ChatAvailability { allowed: false, .. })
        ));
        let other = FeatureEvent::new("files", 7, ChatEvent::Availability(true));
        assert!(backend_event(SessionId(9), &other).is_none());
    }

    #[test]
    fn rejections_map_to_send_errors() {
        assert_eq!(
            ChatSendError::from(ChatRejected::TooLong { chars: 9, max: 4 }),
            ChatSendError::TooLong { max: 4 }
        );
        assert_eq!(
            ChatSendError::from(ChatRejected::NoSession),
            ChatSendError::Unavailable
        );
        assert_eq!(max_chat_chars(), 4000);
    }
}
