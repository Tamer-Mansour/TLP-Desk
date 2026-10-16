/// Errors from the crypto crate.
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("key generation or certificate error: {0}")]
    Cert(#[from] rcgen::Error),
    #[error("certificate could not be parsed: {0}")]
    BadCertificate(String),
    #[error("invalid device ID: {0}")]
    BadDeviceId(String),
    #[error("invalid fingerprint: {0}")]
    BadFingerprint(String),
    #[error("malformed known-hosts line {line}: {reason}")]
    KnownHostsFormat { line: usize, reason: String },
    #[error("password hashing failed: {0}")]
    PasswordHash(String),
    #[error("the OS random number generator failed: {0}")]
    Random(String),
}
