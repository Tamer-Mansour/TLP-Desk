//! Minisign-compatible signing for release manifests (test helper and the `release_sign`
//! example used by `scripts/sign-release.ps1`). Not part of the shipped app: the app only
//! verifies, with the public key in [`super::pubkey`] (`minisign-verify`).
//!
//! Formats (minisign, prehashed "ED" algorithm):
//! - public key: `base64("Ed" || key_id[8] || ed25519_pk[32])`
//! - signature file: untrusted comment line, `base64("ED" || key_id || sig(blake2b512(file)))`,
//!   `trusted comment: <text>`, `base64(sig(signature || text))`.
//! - secret key file (TLP-Desk's own, not minisign's encrypted format): a comment line and
//!   `base64(key_id[8] || ed25519_seed[32])`. Keep it secret; it is never committed.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use blake2::{Blake2b512, Digest as _};
use ed25519_dalek::{Signer as _, SigningKey};

/// A release signing key.
pub struct SecretKey {
    key_id: [u8; 8],
    signing: SigningKey,
}

impl SecretKey {
    /// Key from fixed bytes (tests use this for reproducible keys).
    pub fn from_parts(key_id: [u8; 8], seed: [u8; 32]) -> Self {
        Self {
            key_id,
            signing: SigningKey::from_bytes(&seed),
        }
    }

    /// Fresh random key from the OS RNG.
    pub fn generate() -> Result<Self, String> {
        let mut buf = [0u8; 40];
        getrandom::fill(&mut buf).map_err(|e| e.to_string())?;
        let mut key_id = [0u8; 8];
        let mut seed = [0u8; 32];
        key_id.copy_from_slice(buf.get(..8).ok_or("rng")?);
        seed.copy_from_slice(buf.get(8..).ok_or("rng")?);
        Ok(Self::from_parts(key_id, seed))
    }

    /// The secret key file contents.
    pub fn encode(&self) -> String {
        let mut raw = self.key_id.to_vec();
        raw.extend_from_slice(self.signing.as_bytes());
        format!(
            "untrusted comment: TLP-Desk release signing key - KEEP SECRET\n{}\n",
            B64.encode(raw)
        )
    }

    /// Reads [`Self::encode`] output, or just its base64 line (as stored in a CI secret).
    pub fn decode(text: &str) -> Option<Self> {
        let line = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("untrusted comment:"))?;
        let raw = B64.decode(line).ok()?;
        if raw.len() != 40 {
            return None;
        }
        let mut key_id = [0u8; 8];
        let mut seed = [0u8; 32];
        key_id.copy_from_slice(raw.get(..8)?);
        seed.copy_from_slice(raw.get(8..)?);
        Some(Self::from_parts(key_id, seed))
    }

    /// The public key line (what `update/pubkey.rs` embeds).
    pub fn public_key_base64(&self) -> String {
        let mut raw = b"Ed".to_vec();
        raw.extend_from_slice(&self.key_id);
        raw.extend_from_slice(self.signing.verifying_key().as_bytes());
        B64.encode(raw)
    }

    /// A minisign signature file for `data`.
    pub fn sign(&self, data: &[u8], trusted_comment: &str) -> String {
        let hash = Blake2b512::digest(data);
        let sig = self.signing.sign(&hash).to_bytes();
        let mut bin1 = b"ED".to_vec();
        bin1.extend_from_slice(&self.key_id);
        bin1.extend_from_slice(&sig);
        let tc: String = trusted_comment
            .chars()
            .filter(|c| *c != '\n' && *c != '\r')
            .collect();
        let mut global = sig.to_vec();
        global.extend_from_slice(tc.as_bytes());
        let gsig = self.signing.sign(&global).to_bytes();
        format!(
            "untrusted comment: signature from TLP-Desk release key\n{}\ntrusted comment: {tc}\n{}\n",
            B64.encode(bin1),
            B64.encode(gsig)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_keys_are_distinct_and_sign() {
        let a = SecretKey::generate().unwrap();
        let b = SecretKey::generate().unwrap();
        assert_ne!(a.public_key_base64(), b.public_key_base64());
        let sig = a.sign(b"x", "c");
        assert_eq!(sig.lines().count(), 4);
    }
}
