//! Per-session chat history (bounded) and duplicate detection for the peer's message ids.

use std::collections::{HashSet, VecDeque};

use super::ChatEntry;

/// How many of the peer's most recent message ids are remembered for de-duplication.
const SEEN_IDS: usize = 1024;

/// In-memory history of one session. Nothing is persisted.
#[derive(Debug)]
pub(crate) struct ChatLog {
    entries: VecDeque<ChatEntry>,
    max: usize,
    seen: HashSet<u64>,
    seen_order: VecDeque<u64>,
}

impl ChatLog {
    pub(crate) fn new(max: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            max,
            seen: HashSet::new(),
            seen_order: VecDeque::new(),
        }
    }

    /// Records that the peer's message `id` arrived. `false` if it was seen before (a
    /// duplicate to ignore).
    pub(crate) fn first_sight(&mut self, id: u64) -> bool {
        if !self.seen.insert(id) {
            return false;
        }
        self.seen_order.push_back(id);
        while self.seen_order.len() > SEEN_IDS {
            if let Some(old) = self.seen_order.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }

    /// Appends an entry, dropping the oldest beyond the limit.
    pub(crate) fn push(&mut self, entry: ChatEntry) {
        if self.max == 0 {
            return;
        }
        self.entries.push_back(entry);
        while self.entries.len() > self.max {
            self.entries.pop_front();
        }
    }

    /// Oldest first.
    pub(crate) fn entries(&self) -> Vec<ChatEntry> {
        self.entries.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::ChatAuthor;

    fn entry(id: u64) -> ChatEntry {
        ChatEntry {
            id,
            author: ChatAuthor::Peer,
            text: format!("رسالة {id}"),
            sent_unix_ms: 0,
            local_unix_ms: 0,
        }
    }

    #[test]
    fn history_is_bounded_oldest_first() {
        let mut log = ChatLog::new(3);
        for id in 1..=5 {
            log.push(entry(id));
        }
        let ids: Vec<u64> = log.entries().iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![3, 4, 5]);
        let mut none = ChatLog::new(0);
        none.push(entry(1));
        assert!(none.entries().is_empty());
    }

    #[test]
    fn duplicates_are_detected_within_the_window() {
        let mut log = ChatLog::new(10);
        assert!(log.first_sight(7));
        assert!(!log.first_sight(7));
        for id in 100..100 + SEEN_IDS as u64 {
            assert!(log.first_sight(id));
        }
        // 7 fell out of the window: memory stays bounded.
        assert!(log.first_sight(7));
        assert!(log.seen.len() <= SEEN_IDS);
    }
}
