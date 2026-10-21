//! Remote devices (TLP-relay): the devices the user reaches through their own relay server,
//! kept apart from the LAN address book. Each entry is a device ID with the user's name for
//! it, when it was last reached, and a small picture of its screen taken during the last
//! session (the card's background on the Remote devices page).
//!
//! * [`RemoteBook`] is the pure list (unit-tested without files).
//! * [`Store`] is the file: `remote_devices.json` next to the settings, thumbnails as PNG in
//!   `remote_thumbs/<id>.png`.
//! * [`thumb`] shrinks a decoded frame to thumbnail size.
//!
//! Sessions through the relay are capped at [`RELAY_CAP`] (1080p; the host's own frame-rate
//! cap, 60 fps by default, applies): a relay forwards every packet over the internet, and a
//! 4K stream there buys nothing but stalls.

mod store;
pub mod thumb;

use serde::{Deserialize, Serialize};

pub use store::{LoadReport, Store, FILE_NAME, THUMBS_DIR};
pub use thumb::Thumb;

use crate::address_book::{id_serde, UnixTime, MAX_LABEL_CHARS};
use crate::backend::DeviceId;

/// Largest picture streamed through a relay (width, height).
pub const RELAY_CAP: (u32, u32) = (1920, 1080);
/// Most entries kept (and loaded from a file).
pub const MAX_ENTRIES: usize = 2_000;

/// One remote device.
///
/// Every field has a default so older and newer files load; unknown fields are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteEntry {
    /// The 9-digit device ID (stored as a 9-digit string).
    #[serde(with = "id_serde")]
    pub device_id: DeviceId,
    /// The user's own name for the device (may be Arabic). Empty = show the ID.
    pub alias: String,
    /// The name the device announced in the last session.
    pub name: String,
    /// When this entry was created.
    pub added: UnixTime,
    /// Last successful connection from this PC through the relay.
    pub last_connected: Option<UnixTime>,
    /// A thumbnail of its screen exists (`remote_thumbs/<id>.png`); taken this long ago.
    pub thumb_taken: Option<UnixTime>,
}

impl Default for RemoteEntry {
    fn default() -> Self {
        Self::new(DeviceId(0), 0)
    }
}

impl RemoteEntry {
    /// An empty entry for `device_id`, created at `now`.
    pub fn new(device_id: DeviceId, now: UnixTime) -> Self {
        Self {
            device_id,
            alias: String::new(),
            name: String::new(),
            added: now,
            last_connected: None,
            thumb_taken: None,
        }
    }

    /// The name shown on the card: the alias, else the announced name, else the ID.
    pub fn display_name(&self) -> String {
        if !self.alias.trim().is_empty() {
            self.alias.trim().to_owned()
        } else if !self.name.trim().is_empty() {
            self.name.trim().to_owned()
        } else {
            self.device_id.to_string()
        }
    }

    /// Caps the strings (hand-edited files, hostile names from the wire).
    pub fn sanitize(&mut self) {
        self.alias = clip(self.alias.trim(), MAX_LABEL_CHARS);
        self.name = clip(self.name.trim(), MAX_LABEL_CHARS);
    }
}

fn clip(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// The remote devices, newest connection first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteBook {
    entries: Vec<RemoteEntry>,
}

impl RemoteBook {
    /// Entries in display order: most recently connected first, then newest added.
    pub fn entries(&self) -> &[RemoteEntry] {
        &self.entries
    }

    /// The entry for `id`.
    pub fn get(&self, id: DeviceId) -> Option<&RemoteEntry> {
        self.entries.iter().find(|e| e.device_id == id)
    }

    /// Whether `id` is listed.
    pub fn contains(&self, id: DeviceId) -> bool {
        self.get(id).is_some()
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// No entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Insert or replace an entry (capped at [`MAX_ENTRIES`]; the oldest entry goes).
    pub fn upsert(&mut self, mut entry: RemoteEntry) {
        entry.sanitize();
        self.entries.retain(|e| e.device_id != entry.device_id);
        self.entries.push(entry);
        while self.entries.len() > MAX_ENTRIES {
            // The least recently used entry: oldest connection, else oldest added.
            if let Some((i, _)) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| (e.last_connected.unwrap_or(0), e.added))
            {
                self.entries.remove(i);
            }
        }
        self.sort();
    }

    /// Remove `id`; returns the entry if it was listed.
    pub fn remove(&mut self, id: DeviceId) -> Option<RemoteEntry> {
        let i = self.entries.iter().position(|e| e.device_id == id)?;
        Some(self.entries.remove(i))
    }

    /// A session through the relay reached `id` (announced as `name`) at `now`: the entry is
    /// created if it is new, and its last connection and name are updated.
    pub fn record_connection(&mut self, id: DeviceId, name: &str, now: UnixTime) {
        let mut e = self
            .get(id)
            .cloned()
            .unwrap_or_else(|| RemoteEntry::new(id, now));
        e.last_connected = Some(now);
        if !name.trim().is_empty() {
            e.name = name.trim().to_owned();
        }
        self.upsert(e);
    }

    /// A thumbnail of `id`'s screen was stored at `now`. Returns `false` if `id` is unknown.
    pub fn record_thumb(&mut self, id: DeviceId, now: UnixTime) -> bool {
        let Some(e) = self.entries.iter_mut().find(|e| e.device_id == id) else {
            return false;
        };
        e.thumb_taken = Some(now);
        true
    }

    fn sort(&mut self) {
        self.entries.sort_by(|a, b| {
            b.last_connected
                .cmp(&a.last_connected)
                .then(b.added.cmp(&a.added))
                .then(a.device_id.0.cmp(&b.device_id.0))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_connection_creates_updates_and_orders() {
        let mut book = RemoteBook::default();
        book.record_connection(DeviceId(123_456_789), "Laptop", 100);
        book.record_connection(DeviceId(987_654_321), "", 50);
        assert_eq!(book.len(), 2);
        assert_eq!(book.entries()[0].device_id, DeviceId(123_456_789));
        book.record_connection(DeviceId(987_654_321), "حاسوب ليان", 200);
        assert_eq!(book.entries()[0].device_id, DeviceId(987_654_321));
        assert_eq!(book.entries()[0].name, "حاسوب ليان");
        assert_eq!(book.entries()[0].display_name(), "حاسوب ليان");
        let mut e = book.get(DeviceId(987_654_321)).cloned().unwrap();
        e.alias = "  بيت ليان  ".into();
        book.upsert(e);
        assert_eq!(book.get(DeviceId(987_654_321)).unwrap().alias, "بيت ليان");
        assert!(book.record_thumb(DeviceId(987_654_321), 201));
        assert!(!book.record_thumb(DeviceId(1), 201));
        assert!(book.remove(DeviceId(123_456_789)).is_some());
        assert!(book.remove(DeviceId(123_456_789)).is_none());
        assert_eq!(book.len(), 1);
    }

    #[test]
    fn upsert_caps_the_list_and_strings() {
        let mut book = RemoteBook::default();
        for i in 0..(MAX_ENTRIES as u32 + 5) {
            let mut e = RemoteEntry::new(DeviceId(i + 1), u64::from(i));
            e.alias = "x".repeat(MAX_LABEL_CHARS + 10);
            book.upsert(e);
        }
        assert_eq!(book.len(), MAX_ENTRIES);
        assert!(!book.contains(DeviceId(1)), "the oldest entry went");
        assert!(book.contains(DeviceId(MAX_ENTRIES as u32 + 5)));
        assert_eq!(book.entries()[0].alias.chars().count(), MAX_LABEL_CHARS);
    }

    #[test]
    fn entry_json_round_trips_with_a_string_id() {
        let mut e = RemoteEntry::new(DeviceId(5), 7);
        e.alias = "مكتب".into();
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"000000005\""));
        let back: RemoteEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, e);
        let old: RemoteEntry = serde_json::from_str(r#"{"device_id":"123456789"}"#).unwrap();
        assert_eq!(old.device_id, DeviceId(123_456_789));
        assert!(old.thumb_taken.is_none());
    }
}
