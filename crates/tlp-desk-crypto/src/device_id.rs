use std::fmt;
use std::str::FromStr;

use sha2::{Digest, Sha256};

use crate::CryptoError;

/// SHA-256 of a peer's DER-encoded `SubjectPublicKeyInfo`.
///
/// This is the full-strength key pin. The [`DeviceId`] is derived from it but only carries
/// ~30 bits, so whenever a fingerprint is known (known-hosts store) it should be pinned instead.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fingerprint(pub [u8; 32]);

impl Fingerprint {
    /// Fingerprint of a DER `SubjectPublicKeyInfo`.
    pub fn of_spki(spki_der: &[u8]) -> Self {
        Self(Sha256::digest(spki_der).into())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Device ID derived from this fingerprint.
    pub fn device_id(&self) -> DeviceId {
        DeviceId::from_fingerprint(self)
    }

    /// Lowercase hex, 64 characters.
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Parses 64 hex characters.
    pub fn from_hex(s: &str) -> Result<Self, CryptoError> {
        let s = s.trim();
        if s.len() != 64 || !s.is_ascii() {
            return Err(CryptoError::BadFingerprint(s.to_owned()));
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
                .map_err(|_| CryptoError::BadFingerprint(s.to_owned()))?;
        }
        Ok(Self(out))
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({})", self.to_hex())
    }
}

impl fmt::Display for Fingerprint {
    /// Colon-separated groups of 4 hex chars, e.g. `ab12:cd34:...` (for "key changed" warnings).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, pair) in self.0.chunks(2).enumerate() {
            if i > 0 {
                f.write_str(":")?;
            }
            write!(f, "{:02x}{:02x}", pair[0], pair[1])?;
        }
        Ok(())
    }
}

/// 9-digit device ID (`100_000_000..=999_999_999`) derived from a key [`Fingerprint`].
///
/// Derivation: `u64::from_be_bytes(fingerprint[0..8]) % 900_000_000 + 100_000_000`.
/// Display form groups digits: `123 456 789`. Fits in the `u32` used by `Hello::device_id`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId(u32);

impl DeviceId {
    pub const MIN: u32 = 100_000_000;
    pub const MAX: u32 = 999_999_999;

    /// Wraps a raw ID; `None` unless it has exactly 9 digits.
    pub fn new(raw: u32) -> Option<Self> {
        (Self::MIN..=Self::MAX).contains(&raw).then_some(Self(raw))
    }

    pub fn from_fingerprint(fp: &Fingerprint) -> Self {
        let mut first = [0u8; 8];
        first.copy_from_slice(&fp.0[..8]);
        let v = u64::from_be_bytes(first) % 900_000_000 + 100_000_000;
        // v < 1_000_000_000 < u32::MAX
        Self(v as u32)
    }

    pub fn as_u32(self) -> u32 {
        self.0
    }
}

impl fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceId({self})")
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v = self.0;
        write!(
            f,
            "{:03} {:03} {:03}",
            v / 1_000_000,
            (v / 1000) % 1000,
            v % 1000
        )
    }
}

impl FromStr for DeviceId {
    type Err = CryptoError;

    /// Accepts `123456789`, `123 456 789` or `123-456-789`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let digits: String = s
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .collect();
        if digits.len() != 9 || !digits.chars().all(|c| c.is_ascii_digit()) {
            return Err(CryptoError::BadDeviceId(s.to_owned()));
        }
        let raw: u32 = digits
            .parse()
            .map_err(|_| CryptoError::BadDeviceId(s.to_owned()))?;
        Self::new(raw).ok_or_else(|| CryptoError::BadDeviceId(s.to_owned()))
    }
}

impl From<DeviceId> for u32 {
    fn from(id: DeviceId) -> u32 {
        id.0
    }
}

/// SHA-256 helper used across the crate.
pub(crate) fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_be_bytes());
        h.update(p);
    }
    h.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_always_nine_digits() {
        for seed in 0u32..2000 {
            let fp = Fingerprint(sha256(&[&seed.to_le_bytes()]));
            let id = fp.device_id();
            assert!((DeviceId::MIN..=DeviceId::MAX).contains(&id.as_u32()));
            assert_eq!(id.as_u32().to_string().len(), 9);
        }
        // extremes of the first 8 bytes
        let lo = Fingerprint([0u8; 32]).device_id();
        let hi = Fingerprint([0xFF; 32]).device_id();
        assert_eq!(lo.as_u32(), 100_000_000);
        assert!(hi.as_u32() <= DeviceId::MAX);
    }

    #[test]
    fn device_id_derivation_is_stable() {
        // Known-answer test: pins the derivation formula so IDs never change across versions.
        let mut fp = [0u8; 32];
        fp[..8].copy_from_slice(&0x0123_4567_89ab_cdefu64.to_be_bytes());
        let id = Fingerprint(fp).device_id();
        assert_eq!(
            id.as_u32() as u64,
            0x0123_4567_89ab_cdefu64 % 900_000_000 + 100_000_000
        );
        assert_eq!(id, Fingerprint(fp).device_id());
    }

    #[test]
    fn device_id_display_and_parse() {
        let id = DeviceId::new(123_456_789).unwrap();
        assert_eq!(id.to_string(), "123 456 789");
        assert_eq!("123456789".parse::<DeviceId>().unwrap(), id);
        assert_eq!("123 456 789".parse::<DeviceId>().unwrap(), id);
        assert_eq!("123-456-789".parse::<DeviceId>().unwrap(), id);
        assert_eq!(
            DeviceId::new(100_000_007).unwrap().to_string(),
            "100 000 007"
        );
        assert!("12345678".parse::<DeviceId>().is_err());
        assert!("012345678".parse::<DeviceId>().is_err());
        assert!("12345678a".parse::<DeviceId>().is_err());
        assert!(DeviceId::new(99_999_999).is_none());
        assert!(DeviceId::new(1_000_000_000).is_none());
    }

    #[test]
    fn fingerprint_hex_roundtrip() {
        let fp = Fingerprint(sha256(&[b"x"]));
        assert_eq!(Fingerprint::from_hex(&fp.to_hex()).unwrap(), fp);
        assert!(Fingerprint::from_hex("zz").is_err());
        assert_eq!(fp.to_string().len(), 64 + 15);
    }
}
