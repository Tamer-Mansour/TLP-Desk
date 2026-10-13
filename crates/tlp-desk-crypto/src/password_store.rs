//! At-rest storage and strength rating of the permanent unattended-access password.
//!
//! Only the Argon2id [`PasswordVerifier`] is stored (hex, one line) in the host's data
//! directory (`%ProgramData%\TLP-Desk\` with the SYSTEM + Administrators ACL when the process
//! may write there, else the per-user fallback of [`crate::default_dir`]). The password itself
//! is never written anywhere.

use std::fs;
use std::path::{Path, PathBuf};

use crate::ticket::write_private;
use crate::{CryptoError, Fingerprint, PasswordVerifier};

/// File name of the stored verifier inside the data directory.
pub const PASSWORD_FILE: &str = "unattended_password";

/// Shortest password accepted for unattended access, in characters (not bytes, so Arabic
/// passwords are measured fairly).
pub const MIN_PASSWORD_CHARS: usize = 8;

/// Where the unattended-access verifier lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordStore {
    path: PathBuf,
}

impl PasswordStore {
    /// The store in data directory `dir` (the file need not exist yet).
    pub fn new(dir: &Path) -> Self {
        Self {
            path: dir.join(PASSWORD_FILE),
        }
    }

    /// Path of the verifier file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The stored verifier; `None` if unattended password access is not set up.
    ///
    /// # Errors
    /// The file exists but cannot be read or does not hold a verifier.
    pub fn load(&self) -> Result<Option<PasswordVerifier>, CryptoError> {
        match fs::read_to_string(&self.path) {
            Ok(s) => PasswordVerifier::from_hex(s.trim()).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Stores `verifier` (atomically), creating and hardening the directory if needed.
    ///
    /// # Errors
    /// The directory or file cannot be written (e.g. not elevated and no fallback).
    pub fn save(&self, verifier: &PasswordVerifier) -> Result<(), CryptoError> {
        write_private(&self.path, format!("{}\n", verifier.to_hex()).as_bytes())
    }

    /// Removes the stored verifier (disables password access). Missing file is fine.
    ///
    /// # Errors
    /// The file exists but cannot be deleted.
    pub fn clear(&self) -> Result<(), CryptoError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Whether `password` matches the stored verifier for host `host_fp` (constant-time
    /// comparison). `false` when nothing is stored. Slow on purpose (Argon2id): call it off the
    /// UI thread.
    ///
    /// # Errors
    /// Reading the store or hashing failed.
    pub fn check(&self, password: &str, host_fp: &Fingerprint) -> Result<bool, CryptoError> {
        let Some(stored) = self.load()? else {
            return Ok(false);
        };
        let typed = PasswordVerifier::derive(password, host_fp)?;
        Ok(verifiers_equal(&stored, &typed))
    }
}

/// Constant-time equality of two verifiers.
pub fn verifiers_equal(a: &PasswordVerifier, b: &PasswordVerifier) -> bool {
    let (a, b) = (a.as_secret(), b.as_secret());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Rough strength of a candidate password, for the meter in Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PasswordStrength {
    /// Shorter than [`MIN_PASSWORD_CHARS`]: refused.
    TooShort,
    /// Long enough but easy to guess (one character class, a common password, repeats).
    Weak,
    /// Acceptable.
    Fair,
    /// Long and varied.
    Strong,
}

/// A handful of passwords that are always weak whatever their length.
const COMMON: &[&str] = &[
    "password",
    "password1",
    "12345678",
    "123456789",
    "1234567890",
    "qwertyuiop",
    "qwerty123",
    "11111111",
    "00000000",
    "iloveyou",
    "admin123",
    "letmein1",
    "tlp-desk",
    "tlpdesk1",
];

/// Rates `password`. Length is counted in characters; Arabic and other non-ASCII letters count
/// as their own character class.
pub fn password_strength(password: &str) -> PasswordStrength {
    let chars: Vec<char> = password.chars().collect();
    if chars.len() < MIN_PASSWORD_CHARS {
        return PasswordStrength::TooShort;
    }
    let lower = password.to_lowercase();
    if COMMON.contains(&lower.as_str()) {
        return PasswordStrength::Weak;
    }
    let mut distinct = chars.clone();
    distinct.sort_unstable();
    distinct.dedup();
    if distinct.len() <= 2 {
        return PasswordStrength::Weak;
    }
    let classes = [
        chars.iter().any(|c| c.is_ascii_lowercase()),
        chars.iter().any(|c| c.is_ascii_uppercase()),
        chars.iter().any(char::is_ascii_digit),
        chars
            .iter()
            .any(|c| c.is_ascii_punctuation() || c.is_whitespace()),
        chars.iter().any(|c| !c.is_ascii() && c.is_alphabetic()),
        chars.iter().any(|c| !c.is_ascii() && c.is_numeric()),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    match (chars.len(), classes) {
        (n, c) if n >= 14 && c >= 2 => PasswordStrength::Strong,
        (n, c) if n >= 10 && c >= 3 => PasswordStrength::Strong,
        (_, c) if c >= 2 => PasswordStrength::Fair,
        (n, _) if n >= 16 && distinct.len() >= 8 => PasswordStrength::Fair,
        _ => PasswordStrength::Weak,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_roundtrip_check_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let store = PasswordStore::new(&dir.path().join("TLP-Desk"));
        let fp = Fingerprint([5; 32]);
        assert_eq!(store.load().unwrap(), None);
        assert!(!store.check("anything", &fp).unwrap());
        // Arabic password, mixed with digits and Latin.
        let pw = "كلمة سر 2026 Desk";
        let v = PasswordVerifier::derive(pw, &fp).unwrap();
        store.save(&v).unwrap();
        assert_eq!(store.load().unwrap(), Some(v));
        assert!(store.check(pw, &fp).unwrap());
        assert!(!store.check("كلمة سر 2027 Desk", &fp).unwrap());
        // the password itself is never on disk
        let text = fs::read_to_string(store.path()).unwrap();
        assert!(!text.contains("2026"));
        store.clear().unwrap();
        store.clear().unwrap();
        assert_eq!(store.load().unwrap(), None);
    }

    #[test]
    fn corrupt_store_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let store = PasswordStore::new(dir.path());
        fs::write(store.path(), "not hex at all").unwrap();
        assert!(store.load().is_err());
    }

    #[test]
    fn strength_meter() {
        use PasswordStrength::*;
        assert_eq!(password_strength(""), TooShort);
        assert_eq!(password_strength("abc123"), TooShort);
        // 7 Arabic letters are 14 bytes but still too short
        assert_eq!(password_strength("كلمةسرّ"), TooShort);
        assert_eq!(password_strength("password"), Weak);
        assert_eq!(password_strength("PASSWORD"), Weak);
        assert_eq!(password_strength("aaaaaaaaaaaa"), Weak);
        assert_eq!(password_strength("abcdefgh"), Weak);
        assert_eq!(password_strength("abcdefg1"), Fair);
        assert_eq!(password_strength("مكتب الإدارة 7"), Strong);
        assert_eq!(password_strength("Tr0ub4dor&3x"), Strong);
        assert_eq!(password_strength("correct horse battery"), Strong);
        assert!(password_strength("كلمةسرطويلة") >= Weak);
    }

    #[test]
    fn verifier_comparison() {
        let fp = Fingerprint([1; 32]);
        let a = PasswordVerifier::derive("one two three", &fp).unwrap();
        let b = PasswordVerifier::derive("one two three", &fp).unwrap();
        let c = PasswordVerifier::derive("one two four", &fp).unwrap();
        assert!(verifiers_equal(&a, &b));
        assert!(!verifiers_equal(&a, &c));
    }
}
