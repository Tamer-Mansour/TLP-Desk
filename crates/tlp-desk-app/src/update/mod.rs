//! In-app updates.
//!
//! Two independent paths, both started by the user and neither automatic by default:
//! - **LAN notice** (no internet): peers announce their app version in discovery
//!   (`tlp_desk_net::APP_VERSION`); [`version::newest_peer`] finds a newer one and the home
//!   screen offers a link to the releases page. Nothing is transferred.
//! - **GitHub check** (opt-in, the one internet exception in CLAUDE.md hard rule 2):
//!   [`check`] asks the GitHub Releases API for a newer release, [`download_and_verify`] fetches
//!   `TLP-Desk-<ver>-setup.exe`, `SHA256SUMS.txt` and `SHA256SUMS.txt.minisig` into
//!   `%TEMP%\TLP-Desk\update\`, verifies the manifest signature with [`pubkey::RELEASE_PUBLIC_KEY`]
//!   and the installer hash, and only then may [`install::launch_installer`] run it. Every
//!   failure is fail-closed.
//!
//! [`worker::UpdateWorker`] runs all of this on a background thread for the UI.

pub mod http;
pub mod install;
pub mod pubkey;
pub mod release;
#[cfg(test)]
pub(crate) mod sign;
pub mod verify;
pub mod version;
pub mod worker;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub use release::{Release, ReleaseFiles};
pub use worker::{UpdatePhase, UpdateWorker};

/// The public GitHub repository releases come from.
pub const REPO: &str = "Tamer-Mansour/TLP-Desk";
/// GitHub REST API base.
pub const API_BASE: &str = "https://api.github.com";
/// Releases page opened by "Download" links.
pub const RELEASES_PAGE: &str = "https://github.com/Tamer-Mansour/TLP-Desk/releases";

/// Largest installer accepted (bytes).
pub const MAX_SETUP_BYTES: u64 = 512 * 1024 * 1024;
/// Largest API response / manifest / signature accepted (bytes).
pub const MAX_SMALL_BYTES: u64 = 2 * 1024 * 1024;

/// Why an update step failed. Each variant has its own Ar/En message in the UI.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("network error: {0}")]
    Network(String),
    #[error("server answered http {0}")]
    Status(u16),
    #[error("refusing a non-https url")]
    InsecureUrl,
    #[error("response too large")]
    TooLarge,
    #[error("unexpected response from github")]
    BadResponse,
    #[error("release has no installer for this platform")]
    NoInstaller,
    #[error("release signature is invalid")]
    BadSignature,
    #[error("installer is not listed in the signed checksums")]
    NotInManifest,
    #[error("installer checksum does not match")]
    HashMismatch,
    #[error("cancelled")]
    Cancelled,
    #[error("could not start the installer: {0}")]
    Launch(String),
    #[error("file error: {0}")]
    Io(#[from] std::io::Error),
}

/// Where and how to check.
#[derive(Debug, Clone)]
pub struct CheckConfig {
    /// API base, normally [`API_BASE`] (tests point it at a loopback server).
    pub api_base: String,
    /// `owner/name`.
    pub repo: String,
    /// Offer pre-releases (`-rc.N`) too.
    pub include_prereleases: bool,
    /// Public key the manifest signature must verify against.
    pub public_key: String,
    /// Download folder.
    pub download_dir: PathBuf,
}

impl CheckConfig {
    /// The production configuration.
    pub fn github(include_prereleases: bool) -> Self {
        Self {
            api_base: API_BASE.to_owned(),
            repo: REPO.to_owned(),
            include_prereleases,
            public_key: pubkey::RELEASE_PUBLIC_KEY.to_owned(),
            download_dir: default_download_dir(),
        }
    }
}

/// `%TEMP%\TLP-Desk\update\`.
pub fn default_download_dir() -> PathBuf {
    std::env::temp_dir().join("TLP-Desk").join("update")
}

/// Asks GitHub for the newest release newer than `current`. `Ok(None)` = up to date.
pub fn check(
    client: &http::Client,
    cfg: &CheckConfig,
    current: &semver::Version,
) -> Result<Option<Release>, UpdateError> {
    let base = cfg.api_base.trim_end_matches('/');
    let url = if cfg.include_prereleases {
        format!("{base}/repos/{}/releases?per_page=30", cfg.repo)
    } else {
        format!("{base}/repos/{}/releases/latest", cfg.repo)
    };
    let body = match client.get_bytes(&url, MAX_SMALL_BYTES) {
        // `/releases/latest` is 404 while the repo has no stable release yet.
        Err(UpdateError::Status(404)) if !cfg.include_prereleases => return Ok(None),
        other => other?,
    };
    let list = release::parse_releases(&body).map_err(|_| UpdateError::BadResponse)?;
    Ok(release::pick_newest(list, current, cfg.include_prereleases))
}

/// Downloads the three release files and verifies them. Returns the path of the verified
/// installer; on any failure nothing runnable is left behind.
pub fn download_and_verify(
    client: &http::Client,
    cfg: &CheckConfig,
    rel: &Release,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<PathBuf, UpdateError> {
    let files = rel.files.as_ref().ok_or(UpdateError::NoInstaller)?;
    std::fs::create_dir_all(&cfg.download_dir)?;
    let manifest = client.get_bytes(&files.sums.url, MAX_SMALL_BYTES)?;
    let sig_bytes = client.get_bytes(&files.signature.url, MAX_SMALL_BYTES)?;
    let sig = String::from_utf8(sig_bytes).map_err(|_| UpdateError::BadSignature)?;
    // Check the signature before spending time on the big download.
    verify::verify_manifest(&manifest, &sig, &cfg.public_key)?;
    // Our own file name (never the server's), so the name is the version we compared.
    let name = release::setup_name(&rel.version);
    let text = std::str::from_utf8(&manifest).map_err(|_| UpdateError::NotInManifest)?;
    verify::expected_hash(text, &name)?;
    std::fs::write(cfg.download_dir.join(release::SUMS_NAME), &manifest)?;
    std::fs::write(cfg.download_dir.join(release::SIG_NAME), sig.as_bytes())?;
    let part = cfg.download_dir.join(format!("{name}.part"));
    client.download(&files.setup.url, &part, MAX_SETUP_BYTES, progress, cancel)?;
    verify::verify_file(&part, &name, &manifest, &sig, &cfg.public_key)?;
    let dest = cfg.download_dir.join(&name);
    let _ = std::fs::remove_file(&dest);
    std::fs::rename(&part, &dest)?;
    Ok(dest)
}

/// Re-verifies an installer right before running it (the file sits in a user-writable temp
/// folder between download and launch).
pub fn reverify(cfg: &CheckConfig, installer: &Path) -> Result<(), UpdateError> {
    let name = installer
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(UpdateError::NotInManifest)?;
    let manifest = std::fs::read(cfg.download_dir.join(release::SUMS_NAME))?;
    let sig = std::fs::read_to_string(cfg.download_dir.join(release::SIG_NAME))?;
    verify::verify_file(installer, name, &manifest, &sig, &cfg.public_key)
}
