//! Identity keys, device ID, cert pinning, SPAKE2 password auth.
//!
//! * [`Identity`]: the long-term per-install ECDSA P-256 key pair plus a self-signed X.509
//!   certificate (regenerated from the key on every load) used for QUIC/TLS 1.3.
//! * [`Fingerprint`]: SHA-256 of the DER `SubjectPublicKeyInfo`. Stable for the life of the key.
//! * [`DeviceId`]: the 9-digit number derived from the fingerprint (what users type).
//! * [`OneTimeCode`]: rotating 6-digit code from the OS CSPRNG.
//! * [`pake`]: SPAKE2 with channel binding and explicit key confirmation.
//! * [`RateLimiter`]: 5 attempts/min per peer then a 10-minute lockout (pure logic).
//! * [`KnownHosts`]: trust-on-first-use store of peer fingerprints.
//! * [`PasswordVerifier`]: Argon2id-derived secret for the unattended password (stored at rest).
//! * [`PasswordStore`] / [`password_strength`]: the verifier file and the Settings strength meter.
//! * [`TicketStore`]: single-use reconnect tickets for restart-and-reconnect (hashes only).
#![forbid(unsafe_code)]
// Never-crash set (docs/CODE_STYLE.md, "Lints"): this crate verifies peer-supplied keys,
// messages and files from disk.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable,
    clippy::indexing_slicing
)]

mod code;
mod device_id;
mod error;
mod identity;
mod known_hosts;
pub mod pake;
mod password;
mod password_store;
mod ratelimit;
mod storage;
mod ticket;

pub use code::{OneTimeCode, MAX_FAILURES_PER_CODE};
pub use device_id::{DeviceId, Fingerprint};
pub use error::CryptoError;
pub use identity::{fingerprint_of_cert, Identity, KEY_FILE};
pub use known_hosts::{HostKey, KnownHosts, TrustCheck, KNOWN_HOSTS_FILE};
pub use password::PasswordVerifier;
pub use password_store::{
    password_strength, verifiers_equal, PasswordStore, PasswordStrength, MIN_PASSWORD_CHARS,
    PASSWORD_FILE,
};
pub use ratelimit::{RateLimitConfig, RateLimited, RateLimiter};
pub use storage::{default_dir, harden_dir};
pub use ticket::{
    now_unix_ms, IssuedTicket, TicketRejected, TicketStore, DEFAULT_TICKET_TTL, TICKETS_FILE,
    TICKET_LEN,
};
