//! Host-side settings: what incoming sessions are granted and who may connect.

use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tlp_desk_net::AllowList;
use tlp_desk_proto::Permissions;

/// One permission bit with its stable name in `settings.json` and the connection log.
pub const PERMISSION_NAMES: [(Permissions, &str); 12] = [
    (Permissions::VIEW, "view"),
    (Permissions::CONTROL, "control"),
    (Permissions::KEYBOARD, "keyboard"),
    (Permissions::CLIPBOARD, "clipboard"),
    (Permissions::FILES, "files"),
    (Permissions::AUDIO, "audio"),
    (Permissions::RESTART, "restart"),
    (Permissions::PRIVACY_MODE, "privacy_mode"),
    (Permissions::CHAT, "chat"),
    (Permissions::TUNNEL, "tunnel"),
    (Permissions::WHITEBOARD, "whiteboard"),
    (Permissions::SYSTEM, "system"),
];

/// A permission set persisted as a list of names (`["view", "control"]`), so the file stays
/// readable and survives new bits being added. Unknown names are ignored with a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PermissionSet(pub Permissions);

impl PermissionSet {
    /// Names of the bits that are set, in [`PERMISSION_NAMES`] order.
    pub fn names(self) -> Vec<&'static str> {
        PERMISSION_NAMES
            .iter()
            .filter(|(p, _)| self.0.contains(*p))
            .map(|(_, n)| *n)
            .collect()
    }

    /// Parse names; unknown ones are skipped and returned in the second value.
    pub fn from_names<S: AsRef<str>>(names: &[S]) -> (Self, Vec<String>) {
        let mut bits = Permissions::NONE;
        let mut unknown = Vec::new();
        for n in names {
            match PERMISSION_NAMES
                .iter()
                .find(|(_, name)| *name == n.as_ref())
            {
                Some((p, _)) => bits = bits.union(*p),
                None => unknown.push(n.as_ref().to_owned()),
            }
        }
        (Self(bits), unknown)
    }

    /// Set or clear one bit.
    pub fn set(&mut self, bit: Permissions, on: bool) {
        self.0 = if on {
            self.0.union(bit)
        } else {
            self.0.difference(bit)
        };
    }
}

impl Default for PermissionSet {
    fn default() -> Self {
        Self(tlp_desk_host::DEFAULT_PERMISSIONS)
    }
}

impl Serialize for PermissionSet {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.names().serialize(s)
    }
}

impl<'de> Deserialize<'de> for PermissionSet {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let names = Vec::<String>::deserialize(d)?;
        let (set, unknown) = Self::from_names(&names);
        if !unknown.is_empty() {
            tracing::warn!(?unknown, "ignoring unknown permission names in settings");
        }
        Ok(set)
    }
}

/// Bounds for [`HostSettings::accept_timeout_secs`].
pub const ACCEPT_TIMEOUT_RANGE: std::ops::RangeInclusive<u32> = 10..=600;
/// Bounds for [`HostSettings::local_input_grace_ms`] (0 disables local priority).
pub const GRACE_RANGE: std::ops::RangeInclusive<u32> = 0..=5000;

/// What incoming sessions get and who may connect to this device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostSettings {
    /// Granted to sessions authorized by Accept, the one-time code or the password. The host
    /// user can still revoke any of them live during a session.
    pub default_permissions: PermissionSet,
    /// Seconds before an unanswered Accept prompt declines the viewer.
    pub accept_timeout_secs: u32,
    /// Whether viewers may connect with the one-time code (otherwise Accept prompt only).
    pub allow_one_time_code: bool,
    /// CIDR ranges allowed to connect (validated with [`AllowList::parse`]).
    pub allow_list: Vec<String>,
    /// Milliseconds remote input pauses after the host user touches their own mouse or
    /// keyboard.
    pub local_input_grace_ms: u32,
    /// Green marker at the remote pointer on this screen.
    pub remote_pointer_marker: bool,
    /// Incoming connections on/off ("pause hosting" in the tray). Off refuses every peer
    /// before the handshake; connecting to other devices still works.
    pub incoming_enabled: bool,
}

impl Default for HostSettings {
    fn default() -> Self {
        Self {
            default_permissions: PermissionSet::default(),
            accept_timeout_secs: 60,
            allow_one_time_code: true,
            allow_list: default_allow_list(),
            local_input_grace_ms: 1000,
            remote_pointer_marker: true,
            incoming_enabled: true,
        }
    }
}

/// The default allow-list as editable strings: RFC 1918, link-local and loopback.
pub fn default_allow_list() -> Vec<String> {
    AllowList::lan_default()
        .ranges()
        .iter()
        .map(ToString::to_string)
        .collect()
}

impl HostSettings {
    /// Clamp out-of-range numbers, drop invalid CIDRs and never leave the allow-list empty
    /// (an empty list would silently lock everyone out; pausing is the explicit way to do
    /// that).
    pub fn sanitize(&mut self) {
        self.accept_timeout_secs = self
            .accept_timeout_secs
            .clamp(*ACCEPT_TIMEOUT_RANGE.start(), *ACCEPT_TIMEOUT_RANGE.end());
        self.local_input_grace_ms = self
            .local_input_grace_ms
            .clamp(*GRACE_RANGE.start(), *GRACE_RANGE.end());
        let before = self.allow_list.len();
        self.allow_list
            .retain(|c| AllowList::parse(&[c.as_str()]).is_ok());
        if self.allow_list.len() != before {
            tracing::warn!(
                dropped = before - self.allow_list.len(),
                "ignoring invalid allow-list entries in settings"
            );
        }
        if self.allow_list.is_empty() {
            self.allow_list = default_allow_list();
        }
        // Without VIEW a session cannot exist; keep it so the defaults stay usable.
        self.default_permissions.set(Permissions::VIEW, true);
    }

    /// The parsed allow-list (falls back to the LAN default if the stored list is invalid).
    pub fn allow_list(&self) -> AllowList {
        AllowList::parse(&self.allow_list).unwrap_or_default()
    }

    /// The list the endpoint enforces right now: empty (nobody) while hosting is paused.
    pub fn effective_allow_list(&self) -> AllowList {
        if self.incoming_enabled {
            self.allow_list()
        } else {
            AllowList::from_ranges(Vec::new())
        }
    }

    /// Accept-prompt timeout as a duration.
    pub fn accept_timeout(&self) -> Duration {
        Duration::from_secs(u64::from(self.accept_timeout_secs))
    }

    /// Local-input grace as a duration.
    pub fn local_input_grace(&self) -> Duration {
        Duration::from_millis(u64::from(self.local_input_grace_ms))
    }

    /// The live host options these settings describe.
    pub fn live_options(&self) -> tlp_desk_host::LiveOptions {
        tlp_desk_host::LiveOptions {
            default_permissions: self.default_permissions.0,
            accept_timeout: self.accept_timeout(),
            allow_one_time_code: self.allow_one_time_code,
            local_input_grace: self.local_input_grace(),
            remote_pointer_marker: self.remote_pointer_marker,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_set_serializes_as_names() {
        let s = PermissionSet(Permissions::VIEW | Permissions::FILES | Permissions::CHAT);
        assert_eq!(
            serde_json::to_string(&s).unwrap(),
            r#"["view","files","chat"]"#
        );
        let back: PermissionSet =
            serde_json::from_str(r#"["chat","view","teleport","files"]"#).unwrap();
        assert_eq!(back, s, "unknown names are ignored, order does not matter");
        let all = PermissionSet(Permissions::ALL);
        assert_eq!(all.names().len(), 12);
        assert_eq!(PermissionSet::from_names(&all.names()).0, all);
    }

    #[test]
    fn permission_set_set_and_clear() {
        let mut s = PermissionSet(Permissions::NONE);
        s.set(Permissions::AUDIO, true);
        s.set(Permissions::VIEW, true);
        s.set(Permissions::AUDIO, false);
        assert_eq!(s.0, Permissions::VIEW);
    }

    #[test]
    fn host_defaults_match_the_host_crate() {
        let h = HostSettings::default();
        let live = h.live_options();
        let host = tlp_desk_host::HostConfig::default();
        assert_eq!(live.default_permissions, host.default_permissions);
        assert_eq!(live.accept_timeout, host.accept_timeout);
        assert_eq!(live.allow_one_time_code, host.allow_one_time_code);
        assert_eq!(live.local_input_grace, host.local_input_grace);
        assert_eq!(live.remote_pointer_marker, host.remote_pointer_marker);
        assert_eq!(h.allow_list(), AllowList::lan_default());
    }

    #[test]
    fn sanitize_clamps_and_repairs() {
        let mut h = HostSettings {
            default_permissions: PermissionSet(Permissions::CONTROL),
            accept_timeout_secs: 0,
            local_input_grace_ms: 99_999,
            allow_list: vec![
                "not a cidr".into(),
                "192.168.7.0/24".into(),
                "10.0.0.0/40".into(),
            ],
            ..HostSettings::default()
        };
        h.sanitize();
        assert_eq!(h.accept_timeout_secs, 10);
        assert_eq!(h.local_input_grace_ms, 5000);
        assert_eq!(h.allow_list, vec!["192.168.7.0/24".to_string()]);
        assert!(h.default_permissions.0.contains(Permissions::VIEW));

        let mut empty = HostSettings {
            allow_list: vec!["garbage".into()],
            ..HostSettings::default()
        };
        empty.sanitize();
        assert_eq!(empty.allow_list, default_allow_list());
    }

    #[test]
    fn paused_hosting_allows_nobody() {
        let mut h = HostSettings::default();
        let lan: std::net::IpAddr = "192.168.1.20".parse().unwrap();
        assert!(h.effective_allow_list().is_allowed(lan));
        h.incoming_enabled = false;
        assert!(!h.effective_allow_list().is_allowed(lan));
        assert!(!h
            .effective_allow_list()
            .is_allowed("127.0.0.1".parse().unwrap()));
    }
}
