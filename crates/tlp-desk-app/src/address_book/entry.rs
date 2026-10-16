//! One saved device and the small value types it is made of.

use std::fmt;
use std::net::IpAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::backend::DeviceId;

/// Most recent IP addresses remembered per device (newest first).
pub const MAX_IPS: usize = 4;
/// Longest alias / group / tag kept, in characters.
pub const MAX_LABEL_CHARS: usize = 64;
/// Longest note kept, in characters.
pub const MAX_NOTES_CHARS: usize = 2000;
/// Most tags kept per device.
pub const MAX_TAGS: usize = 16;

/// Seconds since the Unix epoch (UTC).
pub type UnixTime = u64;

/// The current time as [`UnixTime`] (0 if the clock is before 1970).
pub fn now() -> UnixTime {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A 48-bit Ethernet hardware address. Stored as `aa:bb:cc:dd:ee:ff`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MacAddr(pub [u8; 6]);

impl MacAddr {
    /// Parses `aa:bb:cc:dd:ee:ff`, `aa-bb-cc-dd-ee-ff` or `aabbccddeeff` (Arabic-Indic digits
    /// are accepted too). Multicast and all-zero addresses are rejected: they cannot be woken.
    pub fn parse(s: &str) -> Option<Self> {
        let western = crate::i18n::format::to_western_digits(s.trim());
        let mac = tlp_desk_net::parse_mac(&western)?;
        let first = mac.first().copied().unwrap_or(0);
        (first & 1 == 0 && mac.iter().any(|b| *b != 0)).then_some(Self(mac))
    }
}

impl fmt::Display for MacAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&tlp_desk_net::format_mac(self.0))
    }
}

impl Serialize for MacAddr {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for MacAddr {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).ok_or_else(|| serde::de::Error::custom("invalid MAC address"))
    }
}

/// Where a stored MAC address came from. A manually entered MAC is never overwritten by one
/// learned from the neighbour cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MacSource {
    /// Read from the OS neighbour (ARP) cache while the device was on.
    #[default]
    Learned,
    /// Typed by the user.
    Manual,
}

/// A saved device.
///
/// Every field has a default so older and newer files load; unknown fields are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    /// The 9-digit device ID (stored as a 9-digit string).
    #[serde(with = "id_serde")]
    pub device_id: DeviceId,
    /// The user's own name for the device (may be Arabic). Empty = use [`Entry::name`].
    pub alias: String,
    /// The name the device last announced on the LAN.
    pub name: String,
    /// Group the device is listed under. Empty = no group.
    pub group: String,
    /// Free-form tags.
    pub tags: Vec<String>,
    /// Free-form notes.
    pub notes: String,
    /// Last known IP addresses, newest first (at most [`MAX_IPS`]).
    pub last_ips: Vec<IpAddr>,
    /// Hardware address for Wake-on-LAN.
    pub mac: Option<MacAddr>,
    /// Where [`Entry::mac`] came from.
    pub mac_source: MacSource,
    /// Last time discovery saw the device online.
    pub last_seen: Option<UnixTime>,
    /// Last successful connection from this PC.
    pub last_connected: Option<UnixTime>,
    /// Short fingerprint of the key pinned for this device in the known-hosts store (display
    /// reference only; the known-hosts file stays the source of truth for pinning).
    pub pinned_fingerprint: Option<String>,
    /// Listed first.
    pub favorite: bool,
}

impl Default for Entry {
    fn default() -> Self {
        Self::new(DeviceId(0))
    }
}

impl Entry {
    /// An empty entry for `device_id`.
    pub fn new(device_id: DeviceId) -> Self {
        Self {
            device_id,
            alias: String::new(),
            name: String::new(),
            group: String::new(),
            tags: Vec::new(),
            notes: String::new(),
            last_ips: Vec::new(),
            mac: None,
            mac_source: MacSource::default(),
            last_seen: None,
            last_connected: None,
            pinned_fingerprint: None,
            favorite: false,
        }
    }

    /// The name shown in lists: the alias, else the announced name, else the ID.
    pub fn display_name(&self) -> String {
        if !self.alias.trim().is_empty() {
            self.alias.trim().to_owned()
        } else if !self.name.trim().is_empty() {
            self.name.trim().to_owned()
        } else {
            self.device_id.to_string()
        }
    }

    /// Remember `ip` as the newest address (deduplicated, capped).
    pub fn push_ip(&mut self, ip: IpAddr) {
        self.last_ips.retain(|i| *i != ip);
        self.last_ips.insert(0, ip);
        self.last_ips.truncate(MAX_IPS);
    }

    /// Store a MAC learned from the neighbour cache, unless the user typed one.
    /// Returns whether the entry changed.
    pub fn learn_mac(&mut self, mac: MacAddr) -> bool {
        if self.mac_source == MacSource::Manual && self.mac.is_some() {
            return false;
        }
        if self.mac == Some(mac) && self.mac_source == MacSource::Learned {
            return false;
        }
        self.mac = Some(mac);
        self.mac_source = MacSource::Learned;
        true
    }

    /// Clamp every field to its limits and drop control characters, so a hand-edited or
    /// imported file cannot blow up the UI. Bidi marks, ZWJ and ZWNJ are kept: they are part
    /// of correct Arabic text.
    pub fn sanitize(&mut self) {
        self.alias = clean_label(&self.alias);
        self.name = clean_label(&self.name);
        self.group = clean_label(&self.group);
        let mut tags: Vec<String> = Vec::new();
        for t in self.tags.drain(..) {
            let t = clean_label(&t);
            if !t.is_empty() && !tags.contains(&t) && tags.len() < MAX_TAGS {
                tags.push(t);
            }
        }
        self.tags = tags;
        self.notes = clean_text(&self.notes, MAX_NOTES_CHARS, true);
        let mut ips = Vec::new();
        for ip in self.last_ips.drain(..) {
            if !ips.contains(&ip) && ips.len() < MAX_IPS {
                ips.push(ip);
            }
        }
        self.last_ips = ips;
        if self.mac.is_none() {
            self.mac_source = MacSource::Learned;
        }
        if let Some(fp) = &self.pinned_fingerprint {
            let fp = clean_label(fp);
            self.pinned_fingerprint = (!fp.is_empty()).then_some(fp);
        }
    }
}

/// Parse a comma / Arabic-comma separated tag list as typed in the edit dialog.
pub fn parse_tags(s: &str) -> Vec<String> {
    s.split([',', '،', ';', '؛'])
        .map(clean_label)
        .filter(|t| !t.is_empty())
        .collect()
}

fn clean_label(s: &str) -> String {
    clean_text(s, MAX_LABEL_CHARS, false).trim().to_owned()
}

fn clean_text(s: &str, max_chars: usize, keep_newlines: bool) -> String {
    s.chars()
        .filter(|c| !c.is_control() || (keep_newlines && (*c == '\n' || *c == '\t')))
        .take(max_chars)
        .collect()
}

/// Serializes [`DeviceId`] as a 9-digit string; accepts a string (spaces / dashes allowed)
/// or a number.
mod id_serde {
    use serde::{de, Deserialize, Deserializer, Serializer};

    use crate::backend::DeviceId;

    pub fn serialize<S: Serializer>(id: &DeviceId, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(&format_args!("{:09}", id.0))
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Text(String),
        Number(u64),
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DeviceId, D::Error> {
        let id = match Raw::deserialize(d)? {
            Raw::Text(s) => DeviceId::parse(&crate::i18n::format::to_western_digits(&s)),
            Raw::Number(n) => u32::try_from(n)
                .ok()
                .filter(|n| *n <= DeviceId::MAX)
                .map(DeviceId),
        };
        id.ok_or_else(|| de::Error::custom("device_id must have 9 digits"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_prefers_alias_then_name_then_id() {
        let mut e = Entry::new(DeviceId(123_456_789));
        assert_eq!(e.display_name(), "123 456 789");
        e.name = "OFFICE-PC".into();
        assert_eq!(e.display_name(), "OFFICE-PC");
        e.alias = " حاسوب المكتب ".into();
        assert_eq!(e.display_name(), "حاسوب المكتب");
    }

    #[test]
    fn ips_are_newest_first_deduplicated_and_capped() {
        let mut e = Entry::new(DeviceId(1));
        for last in [1u8, 2, 3, 2, 4, 5] {
            e.push_ip(IpAddr::from([192, 168, 1, last]));
        }
        let got: Vec<String> = e.last_ips.iter().map(ToString::to_string).collect();
        assert_eq!(
            got,
            ["192.168.1.5", "192.168.1.4", "192.168.1.2", "192.168.1.3"]
        );
    }

    #[test]
    fn manual_mac_is_never_overwritten_by_a_learned_one() {
        let mut e = Entry::new(DeviceId(1));
        let a = MacAddr([0, 1, 2, 3, 4, 5]);
        let b = MacAddr([0, 9, 9, 9, 9, 9]);
        assert!(e.learn_mac(a));
        assert!(!e.learn_mac(a));
        assert!(e.learn_mac(b));
        e.mac_source = MacSource::Manual;
        assert!(!e.learn_mac(a));
        assert_eq!(e.mac, Some(b));
    }

    #[test]
    fn mac_parsing_accepts_common_forms_and_rejects_unwakeable() {
        let mac = MacAddr([0x00, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e]);
        let want = Some(mac);
        assert_eq!(MacAddr::parse("00:1A:2B:3C:4D:5E"), want);
        assert_eq!(MacAddr::parse(" 00-1a-2b-3c-4d-5e "), want);
        assert_eq!(MacAddr::parse("001a2b3c4d5e"), want);
        // Arabic-Indic digits typed in an Arabic UI.
        assert_eq!(MacAddr::parse("٠٠:1a:2b:3c:4d:5e"), want);
        assert_eq!(MacAddr::parse("00:00:00:00:00:00"), None);
        assert_eq!(MacAddr::parse("ff:ff:ff:ff:ff:ff"), None);
        assert_eq!(MacAddr::parse("01:00:5e:00:00:fb"), None);
        assert_eq!(MacAddr::parse("00:1a:2b"), None);
        assert_eq!(mac.to_string(), "00:1a:2b:3c:4d:5e");
    }

    #[test]
    fn sanitize_clamps_and_keeps_arabic_marks() {
        let mut e = Entry::new(DeviceId(1));
        e.alias = format!("جهاز\u{200F}\u{200C}أحمد\u{0}{}", "x".repeat(200));
        e.tags = vec!["عمل".into(), "عمل".into(), " ".into(), "\u{7}".into()];
        e.notes = "سطر\nسطر".repeat(2000);
        e.sanitize();
        assert!(e.alias.starts_with("جهاز\u{200F}\u{200C}أحمد"));
        assert!(!e.alias.contains('\u{0}'));
        assert_eq!(e.alias.chars().count(), MAX_LABEL_CHARS);
        assert_eq!(e.tags, vec!["عمل".to_owned()]);
        assert_eq!(e.notes.chars().count(), MAX_NOTES_CHARS);
        assert!(e.notes.contains('\n'));
    }

    #[test]
    fn tags_split_on_latin_and_arabic_separators() {
        assert_eq!(
            parse_tags("عمل، بيت, lab ;  ؛"),
            vec!["عمل".to_owned(), "بيت".into(), "lab".into()]
        );
    }

    #[test]
    fn device_id_serializes_as_nine_digits_and_reads_numbers_too() {
        let e = Entry::new(DeviceId(7));
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["device_id"], "000000007");
        let from_num: Entry = serde_json::from_str(r#"{"device_id":123456789}"#).unwrap();
        assert_eq!(from_num.device_id, DeviceId(123_456_789));
        let from_text: Entry = serde_json::from_str(r#"{"device_id":"١٢٣ ٤٥٦ ٧٨٩"}"#).unwrap();
        assert_eq!(from_text.device_id, DeviceId(123_456_789));
        assert!(serde_json::from_str::<Entry>(r#"{"device_id":"12"}"#).is_err());
        assert!(serde_json::from_str::<Entry>(r#"{"device_id":4000000000}"#).is_err());
    }
}
