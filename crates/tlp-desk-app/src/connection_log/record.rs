//! One line of the connection log.

use serde::{Deserialize, Serialize};

use crate::settings::PermissionSet;

/// Longest device name kept in the log (peer names are self-reported, so they are capped).
pub const MAX_NAME_CHARS: usize = 128;
/// Longest error detail kept in the log.
pub const MAX_DETAIL_CHARS: usize = 300;

/// Who connected to whom.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// A viewer connected to this device.
    #[default]
    Incoming,
    /// This device viewed another one.
    Outgoing,
}

/// How the session was authorized.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogAuth {
    /// The host user clicked Accept.
    AcceptPrompt,
    /// The rotating 6-digit code.
    OneTimeCode,
    /// The unattended-access password.
    Password,
    /// Written by a newer build with a method this build does not know.
    #[default]
    #[serde(other)]
    Unknown,
}

/// How a session (or attempt) ended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogEnd {
    /// The user on this device ended it.
    ClosedHere,
    /// The other side ended it.
    ClosedRemotely,
    /// The host revoked the permission to view.
    PermissionRevoked,
    /// The connection dropped or failed.
    Error,
    /// TLP-Desk was closed while the session ran.
    AppExit,
    /// Incoming request declined by the host user.
    Declined,
    /// Nobody answered the Accept prompt in time.
    TimedOut,
    /// Wrong one-time code or password.
    WrongCredentials,
    /// Too many attempts from this address.
    RateLimited,
    /// Refused by settings (code access off, address not allowed, ...).
    NotAllowed,
    /// Written by a newer build with a reason this build does not know.
    #[default]
    #[serde(other)]
    Unknown,
}

impl LogEnd {
    /// Whether this entry is a refused attempt rather than a session that ran.
    pub fn is_refusal(self) -> bool {
        matches!(
            self,
            Self::Declined
                | Self::TimedOut
                | Self::WrongCredentials
                | Self::RateLimited
                | Self::NotAllowed
        )
    }
}

/// One connection-log record (one JSON line).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogEntry {
    /// Start time, milliseconds since the Unix epoch (UTC).
    pub started_ms: u64,
    /// How long the session lasted, in milliseconds (0 for refused attempts).
    pub duration_ms: u64,
    pub direction: Direction,
    /// The peer's 9-digit device ID (authenticated by its key).
    pub device_id: u32,
    /// The peer's self-reported name (may be Arabic), capped at [`MAX_NAME_CHARS`].
    pub name: String,
    /// Hex SHA-256 fingerprint of the peer's key.
    pub fingerprint: String,
    /// The peer's IP address.
    pub address: String,
    pub auth: LogAuth,
    /// Every permission the session held at some point.
    pub permissions: PermissionSet,
    pub end: LogEnd,
    /// Error text for [`LogEnd::Error`], capped at [`MAX_DETAIL_CHARS`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Cut `s` to at most `max` characters (never inside a character).
pub fn cap_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

impl LogEntry {
    /// Enforce the length caps (peer-controlled text never grows the log without bound).
    pub fn capped(mut self) -> Self {
        self.name = cap_chars(&self.name, MAX_NAME_CHARS);
        self.fingerprint = cap_chars(&self.fingerprint, 64);
        self.address = cap_chars(&self.address, 64);
        self.detail = self.detail.map(|d| cap_chars(&d, MAX_DETAIL_CHARS));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tlp_desk_proto::Permissions;

    #[test]
    fn entry_roundtrips_with_arabic_name() {
        let e = LogEntry {
            started_ms: 1_760_000_000_000,
            duration_ms: 65_000,
            direction: Direction::Outgoing,
            device_id: 123_456_789,
            name: "حاسوب \"المكتب\"\nسطر".into(),
            fingerprint: "ab".repeat(32),
            address: "192.168.1.20".into(),
            auth: LogAuth::OneTimeCode,
            permissions: PermissionSet(Permissions::VIEW | Permissions::FILES),
            end: LogEnd::ClosedRemotely,
            detail: None,
        };
        let line = serde_json::to_string(&e).unwrap();
        assert!(!line.contains('\n'), "one record is one line");
        assert_eq!(serde_json::from_str::<LogEntry>(&line).unwrap(), e);
    }

    #[test]
    fn unknown_values_from_newer_builds_still_parse() {
        let e: LogEntry = serde_json::from_str(
            r#"{"direction":"incoming","auth":"smart_card","end":"meteor","extra":1}"#,
        )
        .unwrap();
        assert_eq!(e.auth, LogAuth::Unknown);
        assert_eq!(e.end, LogEnd::Unknown);
    }

    #[test]
    fn caps_are_char_safe() {
        let e = LogEntry {
            name: "م".repeat(500),
            detail: Some("x".repeat(1000)),
            ..LogEntry::default()
        }
        .capped();
        assert_eq!(e.name.chars().count(), MAX_NAME_CHARS);
        assert_eq!(e.detail.map(|d| d.len()), Some(MAX_DETAIL_CHARS));
        assert!(LogEnd::Declined.is_refusal());
        assert!(!LogEnd::AppExit.is_refusal());
    }
}
