use std::fmt;
use std::fs;
use std::io::Write;
use std::path::Path;

use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_ECDSA_P256_SHA256};

use crate::{CryptoError, DeviceId, Fingerprint};

/// File name of the PKCS#8 PEM private key inside the identity directory.
pub const KEY_FILE: &str = "identity.key.pem";

/// The long-term identity of this install.
///
/// Algorithm: **ECDSA P-256 / SHA-256** (`rcgen` + `ring`). Chosen over Ed25519 because it is
/// the most widely supported TLS 1.3 signature scheme. Only the private key is persisted; the
/// self-signed certificate is regenerated from it on each load. Pinning uses the public key
/// (SPKI), never the certificate bytes, so regenerating the certificate does not change the
/// [`Fingerprint`] or [`DeviceId`].
pub struct Identity {
    key_pkcs8_der: Vec<u8>,
    cert_der: Vec<u8>,
    fingerprint: Fingerprint,
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the private key.
        f.debug_struct("Identity")
            .field("device_id", &self.device_id())
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl Identity {
    /// Generates a fresh, unsaved identity.
    pub fn generate() -> Result<Self, CryptoError> {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
        Self::from_key(key)
    }

    /// Loads the identity from `dir`, or generates and saves a new one if none exists.
    ///
    /// If `dir` does not exist it is created and, on Windows, its ACL is restricted with
    /// [`crate::harden_dir`].
    pub fn load_or_generate(dir: &Path) -> Result<Self, CryptoError> {
        if let Some(id) = Self::load(dir)? {
            return Ok(id);
        }
        let id = Self::generate()?;
        id.save(dir)?;
        tracing::info!(device_id = %id.device_id(), dir = %dir.display(), "generated new identity");
        Ok(id)
    }

    /// Loads the identity from `dir`; `Ok(None)` if no key file exists.
    pub fn load(dir: &Path) -> Result<Option<Self>, CryptoError> {
        let path = dir.join(KEY_FILE);
        let pem = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let key = KeyPair::from_pem(&pem)?;
        Self::from_key(key).map(Some)
    }

    /// Writes the private key to `dir/identity.key.pem` (atomically via a temp file + rename).
    pub fn save(&self, dir: &Path) -> Result<(), CryptoError> {
        if !dir.exists() {
            fs::create_dir_all(dir)?;
            crate::harden_dir(dir);
        }
        let key = KeyPair::try_from(self.key_pkcs8_der.as_slice())?;
        let pem = key.serialize_pem();
        let tmp = dir.join(format!("{KEY_FILE}.tmp"));
        {
            let mut opts = fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts.open(&tmp)?;
            f.write_all(pem.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, dir.join(KEY_FILE))?;
        Ok(())
    }

    fn from_key(key: KeyPair) -> Result<Self, CryptoError> {
        let fingerprint = Fingerprint::of_spki(&key.public_key_der());
        let mut params = CertificateParams::new(vec!["tlp-desk".to_owned()])?;
        let mut dn = DistinguishedName::new();
        dn.push(
            DnType::CommonName,
            format!("tlp-desk {}", fingerprint.device_id()),
        );
        params.distinguished_name = dn;
        let cert = params.self_signed(&key)?;
        Ok(Self {
            key_pkcs8_der: key.serialize_der(),
            cert_der: cert.der().to_vec(),
            fingerprint,
        })
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    pub fn device_id(&self) -> DeviceId {
        self.fingerprint.device_id()
    }

    /// DER X.509 certificate to present in TLS.
    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    /// PKCS#8 DER private key for TLS. Handle with care.
    pub fn private_key_pkcs8_der(&self) -> &[u8] {
        &self.key_pkcs8_der
    }
}

/// Fingerprint (SHA-256 of the SPKI) of a DER X.509 certificate received from a peer.
pub fn fingerprint_of_cert(cert_der: &[u8]) -> Result<Fingerprint, CryptoError> {
    let (_, cert) = x509_parser::parse_x509_certificate(cert_der)
        .map_err(|e| CryptoError::BadCertificate(e.to_string()))?;
    Ok(Fingerprint::of_spki(cert.tbs_certificate.subject_pki.raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cert_fingerprint_matches_identity() {
        let id = Identity::generate().unwrap();
        assert_eq!(
            fingerprint_of_cert(id.cert_der()).unwrap(),
            id.fingerprint()
        );
    }

    #[test]
    fn persisted_identity_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("nested");
        let a = Identity::load_or_generate(&sub).unwrap();
        let b = Identity::load_or_generate(&sub).unwrap();
        assert_eq!(a.device_id(), b.device_id());
        assert_eq!(a.fingerprint(), b.fingerprint());
        // certificate is regenerated but still pins to the same key
        assert_eq!(fingerprint_of_cert(b.cert_der()).unwrap(), a.fingerprint());
        let c = Identity::generate().unwrap();
        assert_ne!(a.fingerprint(), c.fingerprint());
    }

    #[test]
    fn load_missing_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Identity::load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn debug_does_not_leak_key() {
        let id = Identity::generate().unwrap();
        let s = format!("{id:?}");
        assert!(s.contains("device_id"));
        assert!(!s.contains("key_pkcs8"));
    }
}
