//! Fail-closed checks of a downloaded release: the minisign (ed25519) signature of
//! `SHA256SUMS.txt`, then the SHA-256 of the installer against the signed manifest.

use std::io::Read;
use std::path::Path;

use sha2::{Digest as _, Sha256};

use super::UpdateError;

/// Verifies `minisig` (minisign text) over `manifest` with the base64 public key `pubkey`.
/// Only prehashed ("ED") signatures are accepted.
pub fn verify_manifest(manifest: &[u8], minisig: &str, pubkey: &str) -> Result<(), UpdateError> {
    let pk = minisign_verify::PublicKey::from_base64(pubkey.trim())
        .map_err(|_| UpdateError::BadSignature)?;
    let sig = minisign_verify::Signature::decode(minisig).map_err(|_| UpdateError::BadSignature)?;
    pk.verify(manifest, &sig, false)
        .map_err(|_| UpdateError::BadSignature)
}

/// The SHA-256 listed for `file_name` in a `sha256sum`-style manifest (`<hex>  <name>`).
pub fn expected_hash(manifest: &str, file_name: &str) -> Result<[u8; 32], UpdateError> {
    for line in manifest.lines() {
        let mut parts = line.split_whitespace();
        let (Some(hex), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        // `sha256sum -b` marks binary files with a leading `*`.
        if name.trim_start_matches('*') == file_name {
            return parse_hex32(hex).ok_or(UpdateError::NotInManifest);
        }
    }
    Err(UpdateError::NotInManifest)
}

fn parse_hex32(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 || !hex.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        let pair = hex.get(i.checked_mul(2)?..i.checked_mul(2)?.checked_add(2)?)?;
        *b = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

/// SHA-256 of a file on disk.
pub fn sha256_file(path: &Path) -> Result<[u8; 32], UpdateError> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(buf.get(..n).unwrap_or_default());
    }
    Ok(h.finalize().into())
}

/// Checks `file` against the signed manifest; on any mismatch the file is deleted.
pub fn verify_file(
    file: &Path,
    file_name: &str,
    manifest: &[u8],
    minisig: &str,
    pubkey: &str,
) -> Result<(), UpdateError> {
    let result = (|| {
        verify_manifest(manifest, minisig, pubkey)?;
        let text = std::str::from_utf8(manifest).map_err(|_| UpdateError::NotInManifest)?;
        let want = expected_hash(text, file_name)?;
        if sha256_file(file)? != want {
            return Err(UpdateError::HashMismatch);
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(file);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::super::sign::SecretKey;
    use super::*;

    fn key(n: u8) -> SecretKey {
        SecretKey::from_parts([n; 8], [n.wrapping_add(1); 32])
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn good_signature_verifies_and_tampering_fails() {
        let k = key(7);
        let manifest = b"abc  TLP-Desk-1.0.0-setup.exe\n";
        let sig = k.sign(manifest, "TLP-Desk 1.0.0");
        verify_manifest(manifest, &sig, &k.public_key_base64()).unwrap();
        // Changed manifest.
        let bad = b"abX  TLP-Desk-1.0.0-setup.exe\n";
        assert!(matches!(
            verify_manifest(bad, &sig, &k.public_key_base64()),
            Err(UpdateError::BadSignature)
        ));
        // Other key (same id, different secret) and other key id.
        let other = SecretKey::from_parts([7; 8], [99; 32]);
        assert!(verify_manifest(manifest, &sig, &other.public_key_base64()).is_err());
        assert!(verify_manifest(manifest, &sig, &key(8).public_key_base64()).is_err());
        // Tampered trusted comment.
        let forged = sig.replace("TLP-Desk 1.0.0", "TLP-Desk 9.9.9");
        assert!(verify_manifest(manifest, &forged, &k.public_key_base64()).is_err());
        // Garbage.
        assert!(verify_manifest(manifest, "", &k.public_key_base64()).is_err());
        assert!(verify_manifest(manifest, &sig, "not base64").is_err());
    }

    #[test]
    fn embedded_public_key_parses() {
        minisign_verify::PublicKey::from_base64(super::super::pubkey::RELEASE_PUBLIC_KEY).unwrap();
    }

    #[test]
    fn secret_key_file_round_trips() {
        let k = key(3);
        let back = SecretKey::decode(&k.encode()).unwrap();
        assert_eq!(back.public_key_base64(), k.public_key_base64());
        let line = k.encode().lines().nth(1).unwrap().to_owned();
        assert!(SecretKey::decode(&line).is_some());
        assert!(SecretKey::decode("untrusted comment: x\nAAAA").is_none());
    }

    #[test]
    fn manifest_lookup() {
        let h = [0xabu8; 32];
        let m = format!(
            "{}  TLP-Desk-1.0.0-win-x64-portable.zip\n{}  TLP-Desk-1.0.0-setup.exe\n",
            hex(&[1u8; 32]),
            hex(&h)
        );
        assert_eq!(expected_hash(&m, "TLP-Desk-1.0.0-setup.exe").unwrap(), h);
        assert_eq!(
            expected_hash(&format!("{} *a.exe", hex(&h)), "a.exe").unwrap(),
            h
        );
        assert!(matches!(
            expected_hash(&m, "TLP-Desk-2.0.0-setup.exe"),
            Err(UpdateError::NotInManifest)
        ));
        assert!(expected_hash("zz  a.exe", "a.exe").is_err());
        assert!(expected_hash(&format!("{}  a.exe", "é".repeat(32)), "a.exe").is_err());
    }

    #[test]
    fn verify_file_checks_hash_and_deletes_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let k = key(5);
        let path = dir.path().join("TLP-Desk-1.0.0-setup.exe");
        std::fs::write(&path, b"installer bytes").unwrap();
        let good = hex(&Sha256::digest(b"installer bytes"));
        let manifest = format!("{good}  TLP-Desk-1.0.0-setup.exe\n");
        let sig = k.sign(manifest.as_bytes(), "t");
        let pk = k.public_key_base64();
        verify_file(
            &path,
            "TLP-Desk-1.0.0-setup.exe",
            manifest.as_bytes(),
            &sig,
            &pk,
        )
        .unwrap();
        assert!(path.exists());

        // Correctly signed manifest, but the file was swapped.
        std::fs::write(&path, b"evil bytes").unwrap();
        assert!(matches!(
            verify_file(
                &path,
                "TLP-Desk-1.0.0-setup.exe",
                manifest.as_bytes(),
                &sig,
                &pk
            ),
            Err(UpdateError::HashMismatch)
        ));
        assert!(!path.exists(), "unverified installer must be deleted");

        // Hash matches but the manifest signature does not.
        std::fs::write(&path, b"installer bytes").unwrap();
        let sig_other = key(6).sign(manifest.as_bytes(), "t");
        assert!(matches!(
            verify_file(
                &path,
                "TLP-Desk-1.0.0-setup.exe",
                manifest.as_bytes(),
                &sig_other,
                &pk
            ),
            Err(UpdateError::BadSignature)
        ));
        assert!(!path.exists());
    }
}
