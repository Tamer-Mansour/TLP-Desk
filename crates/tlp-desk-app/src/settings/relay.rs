//! Relay settings (TLP-relay): the user's own relay server for devices that are not on
//! this network. Nothing is contacted unless `address` is set; registering this device
//! with the relay (so others can reach it) is a separate opt-in.

use serde::{Deserialize, Serialize};
use tlp_desk_net::relay::RelayConfig;

/// Relay server settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RelaySettings {
    /// `host` or `host:port` of the relay (`tlp-desk-relay`); empty = no relay.
    pub address: String,
    /// The relay's shared token (stored as typed; the relay itself only needs the key
    /// derived from it, but the user must be able to read it back to set up a second PC).
    pub token: String,
    /// Register this device with the relay so devices on other networks can connect to it.
    pub reachable: bool,
}

impl RelaySettings {
    /// Trims the fields; a relay without a token is treated as not configured.
    pub fn sanitize(&mut self) {
        self.address = self.address.trim().to_owned();
        self.token = self.token.trim().to_owned();
        if self.address.is_empty() || self.token.is_empty() {
            self.reachable = false;
        }
    }

    /// Both fields set.
    pub fn is_configured(&self) -> bool {
        !self.address.trim().is_empty() && !self.token.trim().is_empty()
    }

    /// The network-layer config, if configured and valid.
    pub fn config(&self) -> Option<RelayConfig> {
        if !self.is_configured() {
            return None;
        }
        RelayConfig::new(&self.address, &self.token).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconfigured_relay_cannot_be_reachable() {
        let mut s = RelaySettings {
            address: "  ".into(),
            token: "x".into(),
            reachable: true,
        };
        s.sanitize();
        assert!(!s.reachable);
        assert!(s.config().is_none());
        let s = RelaySettings {
            address: " relay.example:47402 ".into(),
            token: "secret".into(),
            reachable: true,
        };
        assert!(s.is_configured());
        assert_eq!(s.config().unwrap().addr, "relay.example:47402");
    }

    #[test]
    fn old_settings_files_load_without_a_relay() {
        let s: RelaySettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, RelaySettings::default());
        assert!(!s.is_configured());
    }
}
