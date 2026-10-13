use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::{CryptoError, DeviceId, Fingerprint};

/// Default file name of the known-hosts store inside the storage directory.
pub const KNOWN_HOSTS_FILE: &str = "known_hosts";

/// How a remembered peer was addressed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HostKey {
    /// Dialed by 9-digit device ID.
    Id(DeviceId),
    /// Dialed by IP or hostname (TOFU). Store the string the user typed / the IP.
    Addr(String),
}

impl HostKey {
    fn encode(&self) -> String {
        match self {
            HostKey::Id(id) => format!("id:{}", id.as_u32()),
            HostKey::Addr(a) => format!("addr:{a}"),
        }
    }

    fn decode(s: &str) -> Option<Self> {
        if let Some(rest) = s.strip_prefix("id:") {
            rest.parse().ok().map(HostKey::Id)
        } else {
            s.strip_prefix("addr:").map(|a| HostKey::Addr(a.to_owned()))
        }
    }
}

/// Result of checking an observed fingerprint against the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustCheck {
    /// Never seen: trust on first use, then [`KnownHosts::remember`].
    Unknown,
    /// Same key as before.
    Trusted,
    /// The key changed since last time: warn loudly, do not auto-trust.
    Changed { previous: Fingerprint },
}

/// Trust-on-first-use store mapping a [`HostKey`] to the full peer [`Fingerprint`].
///
/// File format: one entry per line, `<key> <64-hex fingerprint>`; `#` comments allowed.
#[derive(Debug, Default)]
pub struct KnownHosts {
    path: Option<PathBuf>,
    entries: BTreeMap<HostKey, Fingerprint>,
}

impl KnownHosts {
    /// Store that is never written to disk.
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Loads `path` (missing file = empty store). [`KnownHosts::save`] writes back to it.
    pub fn load(path: &Path) -> Result<Self, CryptoError> {
        let mut store = Self {
            path: Some(path.to_owned()),
            entries: BTreeMap::new(),
        };
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(store),
            Err(e) => return Err(e.into()),
        };
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let bad = |reason: &str| CryptoError::KnownHostsFormat {
                line: i + 1,
                reason: reason.to_owned(),
            };
            let (k, fp) = line
                .rsplit_once(' ')
                .ok_or_else(|| bad("missing fingerprint"))?;
            let key = HostKey::decode(k.trim()).ok_or_else(|| bad("bad key"))?;
            let fp = Fingerprint::from_hex(fp).map_err(|_| bad("bad fingerprint"))?;
            store.entries.insert(key, fp);
        }
        Ok(store)
    }

    pub fn check(&self, key: &HostKey, observed: &Fingerprint) -> TrustCheck {
        match self.entries.get(key) {
            None => TrustCheck::Unknown,
            Some(fp) if fp == observed => TrustCheck::Trusted,
            Some(fp) => TrustCheck::Changed { previous: *fp },
        }
    }

    /// Full fingerprint remembered for a device ID, to pin it at full strength.
    pub fn fingerprint_for(&self, key: &HostKey) -> Option<Fingerprint> {
        self.entries.get(key).copied()
    }

    /// Records (or replaces, after the user confirmed a key change) a trusted fingerprint.
    pub fn remember(&mut self, key: HostKey, fp: Fingerprint) {
        self.entries.insert(key, fp);
    }

    pub fn forget(&mut self, key: &HostKey) {
        self.entries.remove(key);
    }

    pub fn iter(&self) -> impl Iterator<Item = (&HostKey, &Fingerprint)> {
        self.entries.iter()
    }

    /// Writes the store back to the file it was loaded from (no-op for in-memory stores).
    pub fn save(&self) -> Result<(), CryptoError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            writeln!(f, "# tlp-desk known hosts: <key> <sha256 of public key>")?;
            for (k, fp) in &self.entries {
                writeln!(f, "{} {}", k.encode(), fp.to_hex())?;
            }
            f.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_hosts_tofu_and_change_detection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(KNOWN_HOSTS_FILE);
        let mut kh = KnownHosts::load(&path).unwrap();
        let a = Fingerprint([1; 32]);
        let b = Fingerprint([2; 32]);
        let key = HostKey::Addr("192.168.1.20".into());
        assert_eq!(kh.check(&key, &a), TrustCheck::Unknown);
        kh.remember(key.clone(), a);
        kh.remember(HostKey::Id(a.device_id()), a);
        kh.save().unwrap();

        let kh = KnownHosts::load(&path).unwrap();
        assert_eq!(kh.check(&key, &a), TrustCheck::Trusted);
        assert_eq!(kh.check(&key, &b), TrustCheck::Changed { previous: a });
        assert_eq!(kh.fingerprint_for(&HostKey::Id(a.device_id())), Some(a));
    }

    #[test]
    fn known_hosts_rejects_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kh");
        fs::write(&path, "id:123 nothex\n").unwrap();
        assert!(KnownHosts::load(&path).is_err());
    }
}
