//! Chat between the viewer and the host user (`Message::ChatMessage`, permission `CHAT`).
//!
//! [`ChatFeature`] is one [`Feature`] that works for both roles: register one instance in
//! `HostConfig::features` and **another** in `ViewerConfig::features` (session keys of the two
//! sides may collide, so never share one instance between them).
//!
//! * The UI sends with [`ChatFeature::send`] (non-blocking) and gets the peer's messages as
//!   [`ChatEvent::Received`] through `HostEvent::Feature` / `ViewerEvent::Feature`
//!   (`FeatureEvent::feature == CHAT_FEATURE`).
//! * Gated by `Permissions::CHAT` on both sides, live: when the host revokes it, sending fails
//!   with [`ChatRejected::NotAllowed`], incoming messages are dropped and the UI gets
//!   [`ChatEvent::Availability`]`(false)`. (The host core also drops a viewer's chat messages
//!   without `CHAT` before they reach this feature.)
//! * Hostile input: messages longer than [`ChatLimits::max_chars`], blank ones and repeated ids
//!   are dropped; a token bucket limits how fast the peer may write ([`ChatLimits::peer_burst`])
//!   and how fast we may ([`ChatLimits::burst`], [`ChatRejected::RateLimited`]).
//! * History is kept in memory per session ([`ChatFeature::history`]) and forgotten when the
//!   session ends. Text is never logged.

mod limits;
mod log;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tlp_desk_proto::{Message, Permissions};
use tokio::sync::{mpsc, watch};

pub use limits::ChatLimits;

use crate::{Feature, FeatureCtx};
use limits::TokenBucket;
use log::ChatLog;

/// [`Feature::name`] of the chat feature (`FeatureEvent::feature`).
pub const CHAT_FEATURE: &str = "chat";

/// Who wrote a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChatAuthor {
    /// The local user.
    Me,
    /// The other side of the session.
    Peer,
}

/// One chat message as kept in the history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatEntry {
    /// Id chosen by the author, unique per author and session.
    pub id: u64,
    pub author: ChatAuthor,
    /// Exactly as written (UTF-8; Arabic, RTL marks and joiners untouched).
    pub text: String,
    /// The author's clock when it was sent, in Unix milliseconds (the peer's clock may be off).
    pub sent_unix_ms: u64,
    /// Our clock when it was sent (mine) or received (theirs), in Unix milliseconds.
    pub local_unix_ms: u64,
}

/// What the chat feature reports to the UI (payload of a `FeatureEvent` named
/// [`CHAT_FEATURE`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEvent {
    /// A message from the peer.
    Received(ChatEntry),
    /// Whether chat is currently allowed in this session (sent once at session start and on
    /// every change of the `CHAT` permission).
    Availability(bool),
}

/// Why [`ChatFeature::send`] did not send a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRejected {
    /// No running session with that key (ended, or chat not registered for it).
    NoSession,
    /// The session does not hold `Permissions::CHAT` (the host revoked it).
    NotAllowed,
    /// Only whitespace.
    Empty,
    /// Longer than [`ChatLimits::max_chars`].
    TooLong { chars: usize, max: usize },
    /// Sending too fast; try again after `retry_after`.
    RateLimited { retry_after: Duration },
}

impl std::fmt::Display for ChatRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSession => f.write_str("no running session for this chat"),
            Self::NotAllowed => f.write_str("chat is not allowed in this session"),
            Self::Empty => f.write_str("the message is empty"),
            Self::TooLong { chars, max } => {
                write!(
                    f,
                    "message of {chars} characters exceeds the {max}-character limit"
                )
            }
            Self::RateLimited { retry_after } => {
                write!(
                    f,
                    "sending too fast; retry in {} ms",
                    retry_after.as_millis()
                )
            }
        }
    }
}

/// Chat for every session of one role. Cheap to share (`Arc`).
#[derive(Debug)]
pub struct ChatFeature {
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    limits: ChatLimits,
    sessions: Mutex<HashMap<u64, SessionChat>>,
}

#[derive(Debug)]
struct SessionChat {
    outbox: mpsc::UnboundedSender<Message>,
    permissions: watch::Receiver<Permissions>,
    next_id: u64,
    mine: TokenBucket,
    theirs: TokenBucket,
    /// Peer messages dropped by the rate limit since the last warning.
    dropped: u64,
    log: ChatLog,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

impl ChatFeature {
    /// Chat with the default [`ChatLimits`].
    pub fn new() -> Arc<Self> {
        Self::with_limits(ChatLimits::default())
    }

    /// Chat with custom limits (tests, settings).
    pub fn with_limits(limits: ChatLimits) -> Arc<Self> {
        Arc::new(Self {
            shared: Arc::new(Shared {
                limits,
                sessions: Mutex::default(),
            }),
        })
    }

    /// The limits in force.
    pub fn limits(&self) -> ChatLimits {
        self.shared.limits
    }

    /// Sends `text` to the peer of session `session_key` (non-blocking; the message is queued
    /// on the session's control stream). Returns the entry as added to the history.
    pub fn send(&self, session_key: u64, text: &str) -> Result<ChatEntry, ChatRejected> {
        let limits = self.shared.limits;
        let mut sessions = lock(&self.shared.sessions);
        let s = sessions
            .get_mut(&session_key)
            .ok_or(ChatRejected::NoSession)?;
        if !s.permissions.borrow().contains(Permissions::CHAT) {
            return Err(ChatRejected::NotAllowed);
        }
        if text.trim().is_empty() {
            return Err(ChatRejected::Empty);
        }
        let chars = text.chars().count();
        if chars > limits.max_chars {
            return Err(ChatRejected::TooLong {
                chars,
                max: limits.max_chars,
            });
        }
        s.mine
            .try_take(Instant::now())
            .map_err(|retry_after| ChatRejected::RateLimited { retry_after })?;
        let id = s.next_id;
        s.next_id = s.next_id.saturating_add(1);
        let now = now_unix_ms();
        let entry = ChatEntry {
            id,
            author: ChatAuthor::Me,
            text: text.to_owned(),
            sent_unix_ms: now,
            local_unix_ms: now,
        };
        let msg = Message::ChatMessage {
            id,
            text: entry.text.clone(),
            sent_unix_ms: now,
        };
        if s.outbox.send(msg).is_err() {
            sessions.remove(&session_key);
            return Err(ChatRejected::NoSession);
        }
        s.log.push(entry.clone());
        tracing::debug!(session = session_key, id, chars, "chat message queued");
        Ok(entry)
    }

    /// The session's messages, oldest first (empty for an unknown or ended session).
    pub fn history(&self, session_key: u64) -> Vec<ChatEntry> {
        lock(&self.shared.sessions)
            .get(&session_key)
            .map(|s| s.log.entries())
            .unwrap_or_default()
    }

    /// Whether chat is allowed in a running session right now (`None`: no such session).
    pub fn is_allowed(&self, session_key: u64) -> Option<bool> {
        lock(&self.shared.sessions)
            .get(&session_key)
            .map(|s| s.permissions.borrow().contains(Permissions::CHAT))
    }
}

impl Shared {
    /// Validates, de-duplicates and rate-limits a message from the peer. `Some(entry)` if it
    /// should be shown.
    fn receive(
        &self,
        session_key: u64,
        id: u64,
        text: String,
        sent_unix_ms: u64,
    ) -> Option<ChatEntry> {
        let limits = self.limits;
        let mut sessions = lock(&self.sessions);
        let s = sessions.get_mut(&session_key)?;
        if !s.permissions.borrow().contains(Permissions::CHAT) {
            tracing::debug!(
                session = session_key,
                "chat message dropped: CHAT not granted"
            );
            return None;
        }
        // Cheap byte check first (a char is at most 4 bytes), then the exact count.
        let max_bytes = limits.max_chars.saturating_mul(4);
        if text.len() > max_bytes || text.chars().count() > limits.max_chars {
            tracing::warn!(
                session = session_key,
                bytes = text.len(),
                "chat message dropped: longer than the limit"
            );
            return None;
        }
        if text.trim().is_empty() {
            tracing::debug!(session = session_key, "blank chat message dropped");
            return None;
        }
        if s.theirs.try_take(Instant::now()).is_err() {
            if s.dropped == 0 {
                tracing::warn!(
                    session = session_key,
                    "peer is sending chat messages too fast; dropping"
                );
            }
            s.dropped = s.dropped.saturating_add(1);
            return None;
        }
        if s.dropped > 0 {
            tracing::info!(
                session = session_key,
                dropped = s.dropped,
                "chat flood ended"
            );
            s.dropped = 0;
        }
        // Only accepted messages count as seen, so a retry after a rate-limit drop still
        // gets through.
        if !s.log.first_sight(id) {
            tracing::debug!(session = session_key, id, "duplicate chat message ignored");
            return None;
        }
        let entry = ChatEntry {
            id,
            author: ChatAuthor::Peer,
            text,
            sent_unix_ms,
            local_unix_ms: now_unix_ms(),
        };
        s.log.push(entry.clone());
        Some(entry)
    }
}

impl Feature for ChatFeature {
    fn name(&self) -> &'static str {
        CHAT_FEATURE
    }

    fn handles(&self, msg: &Message) -> bool {
        matches!(msg, Message::ChatMessage { .. })
    }

    fn start(&self, ctx: FeatureCtx, inbox: mpsc::UnboundedReceiver<Message>) {
        let (outbox, outbox_rx) = mpsc::unbounded_channel();
        let limits = self.shared.limits;
        let now = Instant::now();
        let chat = SessionChat {
            outbox,
            permissions: ctx.permissions.clone(),
            next_id: 1,
            mine: TokenBucket::new(limits.burst, limits.refill, now),
            theirs: TokenBucket::new(limits.peer_burst, limits.refill, now),
            dropped: 0,
            log: ChatLog::new(limits.max_history),
        };
        lock(&self.shared.sessions).insert(ctx.session_key, chat);
        tokio::spawn(run(self.shared.clone(), ctx, inbox, outbox_rx));
    }
}

/// One session's chat task: delivers the peer's messages, sends ours, reports permission
/// changes. Ends with the session (the inbox closes).
async fn run(
    shared: Arc<Shared>,
    ctx: FeatureCtx,
    mut inbox: mpsc::UnboundedReceiver<Message>,
    mut outbox: mpsc::UnboundedReceiver<Message>,
) {
    let key = ctx.session_key;
    let mut perms = ctx.permissions.clone();
    let mut allowed = perms.borrow_and_update().contains(Permissions::CHAT);
    ctx.emit(CHAT_FEATURE, ChatEvent::Availability(allowed));
    let mut perms_open = true;
    loop {
        tokio::select! {
            msg = inbox.recv() => match msg {
                Some(Message::ChatMessage { id, text, sent_unix_ms }) => {
                    if let Some(entry) = shared.receive(key, id, text, sent_unix_ms) {
                        ctx.emit(CHAT_FEATURE, ChatEvent::Received(entry));
                    }
                }
                Some(_) => tracing::debug!(session = key, "chat ignored a non-chat message"),
                None => break,
            },
            out = outbox.recv() => match out {
                Some(msg) => {
                    if let Err(e) = ctx.session.send(&msg).await {
                        tracing::debug!(session = key, %e, "chat send failed; session ending");
                        break;
                    }
                }
                None => break,
            },
            changed = perms.changed(), if perms_open => {
                if changed.is_err() {
                    perms_open = false;
                    continue;
                }
                let now = perms.borrow_and_update().contains(Permissions::CHAT);
                if now != allowed {
                    allowed = now;
                    tracing::info!(session = key, allowed, "chat permission changed");
                    ctx.emit(CHAT_FEATURE, ChatEvent::Availability(allowed));
                }
            }
        }
    }
    lock(&shared.sessions).remove(&key);
    tracing::debug!(session = key, "chat stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_session(
        feature: &ChatFeature,
        key: u64,
        perms: Permissions,
    ) -> (watch::Sender<Permissions>, mpsc::UnboundedReceiver<Message>) {
        let (perm_tx, perm_rx) = watch::channel(perms);
        let (outbox, outbox_rx) = mpsc::unbounded_channel();
        let limits = feature.shared.limits;
        let now = Instant::now();
        lock(&feature.shared.sessions).insert(
            key,
            SessionChat {
                outbox,
                permissions: perm_rx,
                next_id: 1,
                mine: TokenBucket::new(limits.burst, limits.refill, now),
                theirs: TokenBucket::new(limits.peer_burst, limits.refill, now),
                dropped: 0,
                log: ChatLog::new(limits.max_history),
            },
        );
        (perm_tx, outbox_rx)
    }

    #[test]
    fn send_validates_and_queues_exact_text() {
        let chat = ChatFeature::new();
        let (_perms, mut out) = test_session(&chat, 1, Permissions::CHAT);
        let text = "مرحبا \u{200F}TLP-Desk\u{200D} 123 ✓";
        let e = chat.send(1, text).unwrap();
        assert_eq!((e.id, e.author, e.text.as_str()), (1, ChatAuthor::Me, text));
        match out.try_recv().unwrap() {
            Message::ChatMessage { id, text: t, .. } => assert_eq!((id, t.as_str()), (1, text)),
            other => panic!("{other:?}"),
        }
        assert_eq!(chat.send(1, "  \n\t"), Err(ChatRejected::Empty));
        assert_eq!(chat.send(9, "hi"), Err(ChatRejected::NoSession));
        assert_eq!(chat.send(1, "ب").unwrap().id, 2);
        assert_eq!(chat.history(1).len(), 2);
        assert_eq!(chat.is_allowed(1), Some(true));
        assert_eq!(chat.is_allowed(9), None);
    }

    #[test]
    fn length_cap_counts_characters_not_bytes() {
        let chat = ChatFeature::new();
        let (_perms, _out) = test_session(&chat, 1, Permissions::CHAT);
        // 4000 Arabic letters are 8000 bytes: allowed.
        assert!(chat.send(1, &"ع".repeat(4000)).is_ok());
        assert_eq!(
            chat.send(1, &"ع".repeat(4001)),
            Err(ChatRejected::TooLong {
                chars: 4001,
                max: 4000
            })
        );
        let s = &chat.shared;
        assert!(s.receive(1, 1, "a".repeat(4000), 0).is_some());
        assert!(s.receive(1, 2, "a".repeat(4001), 0).is_none());
        assert!(s.receive(1, 3, "ع".repeat(4001), 0).is_none());
    }

    #[test]
    fn revoked_permission_blocks_both_directions() {
        let chat = ChatFeature::new();
        let (perms, _out) = test_session(&chat, 1, Permissions::CHAT);
        perms.send_replace(Permissions::VIEW);
        assert_eq!(chat.send(1, "hi"), Err(ChatRejected::NotAllowed));
        assert!(chat.shared.receive(1, 1, "hi".into(), 0).is_none());
        assert_eq!(chat.is_allowed(1), Some(false));
        perms.send_replace(Permissions::VIEW | Permissions::CHAT);
        assert!(chat.send(1, "hi").is_ok());
        assert!(chat.shared.receive(1, 1, "hi".into(), 0).is_some());
    }

    #[test]
    fn rate_limits_and_dedupe() {
        let chat = ChatFeature::with_limits(ChatLimits {
            burst: 2,
            peer_burst: 3,
            refill: Duration::from_secs(3600),
            ..ChatLimits::default()
        });
        let (_perms, _out) = test_session(&chat, 1, Permissions::CHAT);
        assert!(chat.send(1, "1").is_ok());
        assert!(chat.send(1, "2").is_ok());
        assert!(matches!(
            chat.send(1, "3"),
            Err(ChatRejected::RateLimited { retry_after }) if retry_after > Duration::from_secs(3000)
        ));
        let s = &chat.shared;
        assert!(s.receive(1, 10, "أ".into(), 0).is_some());
        // A duplicate id does not show (and is checked after the bucket).
        assert!(s.receive(1, 10, "أ".into(), 0).is_none());
        assert!(s.receive(1, 11, "ب".into(), 0).is_some());
        assert!(s.receive(1, 12, "ت".into(), 0).is_none(), "flood dropped");
        // History: 2 mine + 2 theirs, in order.
        let h: Vec<_> = chat.history(1).into_iter().map(|e| e.text).collect();
        assert_eq!(h, vec!["1", "2", "أ", "ب"]);
    }

    #[test]
    fn rejection_messages_are_lowercase() {
        for r in [
            ChatRejected::NoSession,
            ChatRejected::NotAllowed,
            ChatRejected::Empty,
            ChatRejected::TooLong { chars: 5, max: 4 },
            ChatRejected::RateLimited {
                retry_after: Duration::from_millis(5),
            },
        ] {
            let s = r.to_string();
            assert!(s.chars().next().is_some_and(char::is_lowercase), "{s}");
        }
    }
}
