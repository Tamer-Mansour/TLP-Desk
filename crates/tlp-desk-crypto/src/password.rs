use std::fmt;

use argon2::{Algorithm, Argon2, Params, Version};

use crate::{CryptoError, Fingerprint};

/// Argon2id-derived secret for the permanent unattended-access password.
///
/// `verifier = Argon2id(m = 19 MiB, t = 2, p = 1, password, salt = "tlp-desk/pw/v2" || host_fp)`.
///
/// The host stores only this value at rest (never the password). The viewer derives the same
/// value from the typed password and the host fingerprint it observed in TLS, and both feed it
/// into SPAKE2 ([`crate::pake`]) as the secret. Salting with the host fingerprint makes the
/// verifier unique per host. Note: SPAKE2 is a balanced PAKE, so a stolen verifier file is
/// password-equivalent for *this* host; keep the storage ACL tight.
#[derive(Clone, PartialEq, Eq)]
pub struct PasswordVerifier([u8; 32]);

impl fmt::Debug for PasswordVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PasswordVerifier(..)")
    }
}

impl PasswordVerifier {
    pub fn derive(password: &str, host_fp: &Fingerprint) -> Result<Self, CryptoError> {
        let params = Params::new(19 * 1024, 2, 1, Some(32))
            .map_err(|e| CryptoError::PasswordHash(e.to_string()))?;
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let mut salt = b"tlp-desk/pw/v2".to_vec();
        salt.extend_from_slice(host_fp.as_bytes());
        let mut out = [0u8; 32];
        argon
            .hash_password_into(password.as_bytes(), &salt, &mut out)
            .map_err(|e| CryptoError::PasswordHash(e.to_string()))?;
        Ok(Self(out))
    }

    /// Bytes to pass as the SPAKE2 secret.
    pub fn as_secret(&self) -> &[u8] {
        &self.0
    }

    /// Hex form for storage.
    pub fn to_hex(&self) -> String {
        Fingerprint(self.0).to_hex()
    }

    pub fn from_hex(s: &str) -> Result<Self, CryptoError> {
        Ok(Self(Fingerprint::from_hex(s)?.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_verifier_is_deterministic_and_host_bound() {
        let fp = Fingerprint([3; 32]);
        let a = PasswordVerifier::derive("hunter2", &fp).unwrap();
        let b = PasswordVerifier::derive("hunter2", &fp).unwrap();
        assert_eq!(a, b);
        assert_ne!(a, PasswordVerifier::derive("hunter3", &fp).unwrap());
        assert_ne!(
            a,
            PasswordVerifier::derive("hunter2", &Fingerprint([4; 32])).unwrap()
        );
        assert_eq!(PasswordVerifier::from_hex(&a.to_hex()).unwrap(), a);
    }
}
