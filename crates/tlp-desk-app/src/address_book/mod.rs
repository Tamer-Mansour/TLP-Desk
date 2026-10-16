//! Address book: saved devices with aliases (Arabic welcome), groups, tags, notes, last known
//! addresses, MAC for Wake-on-LAN, last seen / last connected and a reference to the pinned key.
//!
//! * [`AddressBook`]: the in-memory list and every operation on it (pure, no I/O).
//! * [`Store`]: the versioned JSON file `%APPDATA%\TLP-Desk\address_book.json` (atomic
//!   writes, corruption recovery, import / export).
//! * [`Query`]: Arabic-aware, diacritics-insensitive search.
//! * [`wol`]: picks the broadcast targets for a Wake-on-LAN magic packet.
//!
//! Online status is not stored: it comes live from LAN discovery
//! ([`crate::backend::BackendEvent::Devices`]); the book only records *when* a device was
//! last seen.

mod entry;
mod search;
mod store;
pub mod wol;

use std::cmp::Ordering;
use std::net::IpAddr;

pub use entry::{now, parse_tags, Entry, MacAddr, MacSource, UnixTime, MAX_IPS};
pub use search::{fold, Query};
pub use store::{
    export_file, from_json, import_file, to_json, LoadReport, Store, FILE_NAME, SCHEMA_VERSION,
};

use crate::backend::{DeviceId, DiscoveredDevice, PeerInfo};

/// `last_seen` is refreshed at most this often (seconds), so a device that stays online does
/// not rewrite the file on every discovery tick.
pub const SEEN_RESOLUTION: u64 = 300;

/// List order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortBy {
    /// Alphabetical by the shown name (folded, so harakat and case do not matter).
    #[default]
    Name,
    /// Most recently seen first (online devices count as seen now).
    LastSeen,
}

/// Which saved devices to list.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum GroupFilter {
    /// Every device.
    #[default]
    All,
    /// Favorites only.
    Favorites,
    /// Devices in this group.
    Group(String),
}

/// Result of [`AddressBook::merge`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MergeSummary {
    /// Devices that were not in the book before.
    pub added: usize,
    /// Devices that were updated from the imported data.
    pub updated: usize,
}

/// The saved devices, at most one entry per device ID.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddressBook {
    entries: Vec<Entry>,
}

impl AddressBook {
    /// All entries in insertion order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The entry for `id`.
    pub fn get(&self, id: DeviceId) -> Option<&Entry> {
        self.entries.iter().find(|e| e.device_id == id)
    }

    fn get_mut(&mut self, id: DeviceId) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.device_id == id)
    }

    /// Whether `id` is saved.
    pub fn contains(&self, id: DeviceId) -> bool {
        self.get(id).is_some()
    }

    /// Insert `entry`, replacing an entry with the same ID.
    pub fn upsert(&mut self, entry: Entry) {
        match self.get_mut(entry.device_id) {
            Some(e) => *e = entry,
            None => self.entries.push(entry),
        }
    }

    /// Remove `id`; returns the removed entry.
    pub fn remove(&mut self, id: DeviceId) -> Option<Entry> {
        let i = self.entries.iter().position(|e| e.device_id == id)?;
        Some(self.entries.remove(i))
    }

    /// Flip the favorite flag. Returns whether `id` exists.
    pub fn toggle_favorite(&mut self, id: DeviceId) -> bool {
        match self.get_mut(id) {
            Some(e) => {
                e.favorite = !e.favorite;
                true
            }
            None => false,
        }
    }

    /// Distinct non-empty group names, sorted (folded).
    pub fn groups(&self) -> Vec<String> {
        let mut groups: Vec<String> = Vec::new();
        for e in &self.entries {
            let g = e.group.trim();
            if !g.is_empty() && !groups.iter().any(|x| fold(x) == fold(g)) {
                groups.push(g.to_owned());
            }
        }
        groups.sort_by_cached_key(|g| fold(g));
        groups
    }

    /// Save a device (from the discovered list or after a connection). An existing entry
    /// keeps the user's alias, group, notes and MAC; its name and address are refreshed.
    pub fn save_device(&mut self, id: DeviceId, name: &str, ip: Option<IpAddr>, now: UnixTime) {
        if self.get(id).is_none() {
            self.entries.push(Entry::new(id));
        }
        if let Some(e) = self.get_mut(id) {
            if !name.trim().is_empty() {
                e.name = name.to_owned();
            }
            if let Some(ip) = ip {
                e.push_ip(ip);
            }
            e.last_seen = Some(now);
            e.sanitize();
        }
    }

    /// Update saved devices from a discovery snapshot: name, address and (at most every
    /// [`SEEN_RESOLUTION`] seconds) last seen. Unsaved devices are ignored. Returns whether
    /// anything worth saving changed.
    pub fn observe(&mut self, devices: &[DiscoveredDevice], now: UnixTime) -> bool {
        let mut changed = false;
        for d in devices.iter().filter(|d| d.online) {
            let Some(e) = self.get_mut(d.device_id) else {
                continue;
            };
            if !d.name.trim().is_empty() && e.name != d.name {
                e.name = d.name.clone();
                e.sanitize();
                changed = true;
            }
            if e.last_ips.first() != Some(&d.address) {
                e.push_ip(d.address);
                changed = true;
            }
            let stale = e
                .last_seen
                .is_none_or(|t| now.saturating_sub(t) >= SEEN_RESOLUTION);
            if stale {
                e.last_seen = Some(now);
                changed = true;
            }
        }
        changed
    }

    /// Record a successful connection to a saved device (name, address, time, learned MAC,
    /// pinned key reference). Returns `false` if the device is not saved.
    pub fn record_connection(
        &mut self,
        peer: &PeerInfo,
        mac: Option<MacAddr>,
        fingerprint: Option<String>,
        now: UnixTime,
    ) -> bool {
        let Some(e) = self.get_mut(peer.device_id) else {
            return false;
        };
        if !peer.name.trim().is_empty() {
            e.name = peer.name.clone();
        }
        e.push_ip(peer.address);
        e.last_connected = Some(now);
        e.last_seen = Some(now);
        if let Some(mac) = mac {
            e.learn_mac(mac);
        }
        if fingerprint.is_some() {
            e.pinned_fingerprint = fingerprint;
        }
        e.sanitize();
        true
    }

    /// Store a MAC learned for `id` (ignored for unsaved devices and manual MACs).
    /// Returns whether the entry changed.
    pub fn learn_mac(&mut self, id: DeviceId, mac: MacAddr) -> bool {
        self.get_mut(id).is_some_and(|e| e.learn_mac(mac))
    }

    /// Merge an imported book: new devices are added; for known ones, non-empty imported
    /// labels win, timestamps keep the newest, addresses and tags are united, favorite is
    /// kept if either side has it.
    pub fn merge(&mut self, other: AddressBook) -> MergeSummary {
        let mut summary = MergeSummary::default();
        for mut imported in other.entries {
            imported.sanitize();
            let Some(e) = self.get_mut(imported.device_id) else {
                self.entries.push(imported);
                summary.added = summary.added.saturating_add(1);
                continue;
            };
            let before = e.clone();
            for (mine, theirs) in [
                (&mut e.alias, imported.alias),
                (&mut e.name, imported.name),
                (&mut e.group, imported.group),
                (&mut e.notes, imported.notes),
            ] {
                if !theirs.trim().is_empty() {
                    *mine = theirs;
                }
            }
            e.tags.extend(imported.tags);
            for ip in imported.last_ips.into_iter().rev() {
                if !e.last_ips.contains(&ip) {
                    e.last_ips.push(ip);
                }
            }
            if imported.mac.is_some()
                && (e.mac.is_none() || imported.mac_source == MacSource::Manual)
            {
                e.mac = imported.mac;
                e.mac_source = imported.mac_source;
            }
            e.last_seen = e.last_seen.max(imported.last_seen);
            e.last_connected = e.last_connected.max(imported.last_connected);
            if imported.pinned_fingerprint.is_some() {
                e.pinned_fingerprint = imported.pinned_fingerprint;
            }
            e.favorite |= imported.favorite;
            e.sanitize();
            if *e != before {
                summary.updated = summary.updated.saturating_add(1);
            }
        }
        summary
    }

    /// The entries to list: filtered by `filter` and `query`, favorites first, then `sort`.
    /// `online` tells whether discovery currently sees a device.
    pub fn view(
        &self,
        query: &Query,
        filter: &GroupFilter,
        sort: SortBy,
        online: impl Fn(DeviceId) -> bool,
    ) -> Vec<&Entry> {
        let mut list: Vec<(&Entry, String, u64)> = self
            .entries
            .iter()
            .filter(|e| match filter {
                GroupFilter::All => true,
                GroupFilter::Favorites => e.favorite,
                GroupFilter::Group(g) => fold(e.group.trim()) == fold(g.trim()),
            })
            .filter(|e| query.matches(e))
            .map(|e| {
                let seen = if online(e.device_id) {
                    u64::MAX
                } else {
                    e.last_seen.unwrap_or(0)
                };
                (e, fold(&e.display_name()), seen)
            })
            .collect();
        list.sort_by(|a, b| {
            b.0.favorite
                .cmp(&a.0.favorite)
                .then_with(|| match sort {
                    SortBy::Name => Ordering::Equal,
                    SortBy::LastSeen => b.2.cmp(&a.2),
                })
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.0.device_id.cmp(&b.0.device_id))
        });
        list.into_iter().map(|(e, _, _)| e).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: u32, name: &str, ip: &str) -> DiscoveredDevice {
        DiscoveredDevice {
            device_id: DeviceId(id),
            name: name.into(),
            address: ip.parse().unwrap(),
            online: true,
            app_version: None,
        }
    }

    fn named(id: u32, alias: &str) -> Entry {
        let mut e = Entry::new(DeviceId(id));
        e.alias = alias.into();
        e
    }

    #[test]
    fn save_device_keeps_user_labels() {
        let mut book = AddressBook::default();
        book.save_device(DeviceId(1), "PC-1", "10.0.0.1".parse().ok(), 100);
        let mut e = book.get(DeviceId(1)).unwrap().clone();
        e.alias = "جهاز سارة".into();
        e.group = "البيت".into();
        book.upsert(e);
        book.save_device(DeviceId(1), "PC-1-renamed", "10.0.0.2".parse().ok(), 200);
        let e = book.get(DeviceId(1)).unwrap();
        assert_eq!(e.alias, "جهاز سارة");
        assert_eq!(e.group, "البيت");
        assert_eq!(e.name, "PC-1-renamed");
        assert_eq!(e.last_ips[0].to_string(), "10.0.0.2");
        assert_eq!(book.entries().len(), 1);
    }

    #[test]
    fn observe_updates_only_saved_devices_and_throttles_last_seen() {
        let mut book = AddressBook::default();
        book.save_device(DeviceId(1), "a", None, 0);
        let list = [dev(1, "a", "10.0.0.1"), dev(2, "b", "10.0.0.2")];
        assert!(book.observe(&list, 1000));
        assert!(!book.contains(DeviceId(2)));
        assert_eq!(book.get(DeviceId(1)).unwrap().last_seen, Some(1000));
        // Same snapshot shortly after: nothing to save.
        assert!(!book.observe(&list, 1010));
        assert!(book.observe(&list, 1000 + SEEN_RESOLUTION));
        // Offline entries in the snapshot are ignored.
        let mut off = dev(1, "renamed", "10.0.0.9");
        off.online = false;
        assert!(!book.observe(&[off], 5000));
    }

    #[test]
    fn record_connection_learns_mac_and_fingerprint() {
        let mut book = AddressBook::default();
        let peer = PeerInfo {
            device_id: DeviceId(5),
            name: "مكتب".into(),
            address: "192.168.0.5".parse().unwrap(),
        };
        let mac = MacAddr([0, 1, 2, 3, 4, 5]);
        assert!(!book.record_connection(&peer, Some(mac), None, 10));
        book.save_device(DeviceId(5), "", None, 1);
        assert!(book.record_connection(&peer, Some(mac), Some("ab12 cd34".into()), 10));
        let e = book.get(DeviceId(5)).unwrap();
        assert_eq!(e.mac, Some(mac));
        assert_eq!(e.last_connected, Some(10));
        assert_eq!(e.pinned_fingerprint.as_deref(), Some("ab12 cd34"));
        assert_eq!(e.name, "مكتب");
    }

    #[test]
    fn view_puts_favorites_first_then_sorts() {
        let mut book = AddressBook::default();
        let mut b = named(2, "بيت");
        b.last_seen = Some(50);
        let mut a = named(1, "Alpha");
        a.last_seen = Some(10);
        let mut z = named(3, "zulu");
        z.favorite = true;
        z.last_seen = Some(1);
        let mut m = named(4, "mike");
        m.group = "Lab".into();
        for e in [b, a, z, m] {
            book.upsert(e);
        }
        let ids = |v: Vec<&Entry>| v.iter().map(|e| e.device_id.0).collect::<Vec<_>>();
        let q = Query::default();
        assert_eq!(
            ids(book.view(&q, &GroupFilter::All, SortBy::Name, |_| false)),
            [3, 1, 4, 2]
        );
        assert_eq!(
            ids(book.view(&q, &GroupFilter::All, SortBy::LastSeen, |_| false)),
            [3, 2, 1, 4]
        );
        // Online devices count as seen now.
        assert_eq!(
            ids(book.view(&q, &GroupFilter::All, SortBy::LastSeen, |id| id
                == DeviceId(4))),
            [3, 4, 2, 1]
        );
        assert_eq!(
            ids(book.view(&q, &GroupFilter::Favorites, SortBy::Name, |_| false)),
            [3]
        );
        assert_eq!(
            ids(
                book.view(&q, &GroupFilter::Group("lab".into()), SortBy::Name, |_| {
                    false
                })
            ),
            [4]
        );
        assert_eq!(
            ids(book.view(
                &Query::new("بيت"),
                &GroupFilter::All,
                SortBy::Name,
                |_| false
            )),
            [2]
        );
    }

    #[test]
    fn groups_are_unique_and_sorted() {
        let mut book = AddressBook::default();
        for (id, g) in [
            (1, "Lab"),
            (2, "lab"),
            (3, ""),
            (4, "المكتب"),
            (5, "Accounts"),
        ] {
            let mut e = Entry::new(DeviceId(id));
            e.group = g.into();
            book.upsert(e);
        }
        assert_eq!(book.groups(), vec!["Accounts", "Lab", "المكتب"]);
    }

    #[test]
    fn merge_adds_new_and_updates_known() {
        let mut book = AddressBook::default();
        let mut mine = named(1, "قديم");
        mine.last_seen = Some(100);
        mine.tags = vec!["a".into()];
        book.upsert(mine);
        let mut other = AddressBook::default();
        let mut theirs = named(1, "جديد");
        theirs.last_seen = Some(50);
        theirs.tags = vec!["a".into(), "b".into()];
        theirs.favorite = true;
        other.upsert(theirs);
        other.upsert(named(2, "two"));
        let s = book.merge(other.clone());
        assert_eq!(
            s,
            MergeSummary {
                added: 1,
                updated: 1
            }
        );
        let e = book.get(DeviceId(1)).unwrap();
        assert_eq!(e.alias, "جديد");
        assert_eq!(e.last_seen, Some(100));
        assert_eq!(e.tags, vec!["a".to_owned(), "b".into()]);
        assert!(e.favorite);
        // Merging the same data again changes nothing.
        assert_eq!(book.merge(other), MergeSummary::default());
    }

    #[test]
    fn remove_and_toggle_favorite() {
        let mut book = AddressBook::default();
        book.upsert(named(1, "x"));
        assert!(book.toggle_favorite(DeviceId(1)));
        assert!(book.get(DeviceId(1)).unwrap().favorite);
        assert!(!book.toggle_favorite(DeviceId(9)));
        assert_eq!(
            book.remove(DeviceId(1)).map(|e| e.device_id),
            Some(DeviceId(1))
        );
        assert!(book.remove(DeviceId(1)).is_none());
    }
}
