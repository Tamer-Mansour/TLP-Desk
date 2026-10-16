//! The release signing public key (minisign format, ed25519).
//!
//! Installers are only run when `SHA256SUMS.txt.minisig` verifies against this key. The
//! matching secret key never enters the repository: it lives in the GitHub secret
//! `RELEASE_SIGNING_KEY` (used by `.github/workflows/release.yml`) and offline with the
//! maintainer. Rotating it means shipping a build with the new key *before* signing with it;
//! see `docs/src/dev/release.md`.

/// `base64("Ed" || key_id[8] || ed25519_public_key[32])`.
pub const RELEASE_PUBLIC_KEY: &str = "RWQrGdifdwDKZIGysrnA5Z0pZeQua9VynuqZ8Z98eEkGYtoYqJ+8UiV+";
