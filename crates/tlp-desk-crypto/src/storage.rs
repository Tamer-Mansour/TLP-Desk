use std::fs;
use std::path::{Path, PathBuf};

const APP_DIR: &str = "tlp-desk";

/// Default directory for identity and trust files.
///
/// Windows: `%ProgramData%\tlp-desk` when it exists-and-is-writable or can be created, else
/// `%LOCALAPPDATA%\tlp-desk` (attended, non-admin runs). Other OSes: `/var/lib/tlp-desk` if
/// writable, else `$XDG_DATA_HOME/tlp-desk` or `~/.local/share/tlp-desk`.
///
/// The returned directory may not exist yet; [`crate::Identity::load_or_generate`] creates it.
pub fn default_dir() -> PathBuf {
    for candidate in candidates() {
        if is_writable(&candidate) {
            return candidate;
        }
    }
    // Last resort: current directory.
    PathBuf::from(APP_DIR)
}

#[cfg(windows)]
fn candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = std::env::var_os("ProgramData") {
        v.push(PathBuf::from(p).join(APP_DIR));
    }
    if let Some(p) = std::env::var_os("LOCALAPPDATA") {
        v.push(PathBuf::from(p).join(APP_DIR));
    }
    v
}

#[cfg(not(windows))]
fn candidates() -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from("/var/lib").join(APP_DIR)];
    if let Some(p) = std::env::var_os("XDG_DATA_HOME") {
        v.push(PathBuf::from(p).join(APP_DIR));
    } else if let Some(h) = std::env::var_os("HOME") {
        v.push(PathBuf::from(h).join(".local/share").join(APP_DIR));
    }
    v
}

/// True if `dir` exists (or its parent allows creating it) and a file can be written in it.
/// Does not leave the directory behind if it had to create it and the probe failed.
fn is_writable(dir: &Path) -> bool {
    let existed = dir.exists();
    if !existed && fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".probe-{}", std::process::id()));
    let ok = fs::write(&probe, b"probe").is_ok() && fs::remove_file(&probe).is_ok();
    // Also require that an existing identity (if any) is readable by us.
    let key = dir.join(crate::identity::KEY_FILE);
    let key_ok = !key.exists() || fs::read(&key).is_ok();
    if !existed {
        // Leave creation (and ACL hardening) to Identity::save.
        let _ = fs::remove_dir(dir);
    }
    ok && key_ok
}

/// Restricts a freshly created storage directory so other local users cannot read the key.
///
/// Windows: removes inherited ACEs and grants Full Control only to `SYSTEM` (S-1-5-18),
/// `Administrators` (S-1-5-32-544) and `OWNER RIGHTS` (S-1-3-4, i.e. whoever created the
/// directory/files: the service, or the attended user when it falls back to a per-user path).
/// Implemented with the built-in `icacls.exe` to stay free of `unsafe`. Failures are logged,
/// not fatal. Unix: `chmod 700`.
pub fn harden_dir(dir: &Path) {
    #[cfg(windows)]
    {
        let status = std::process::Command::new("icacls")
            .arg(dir)
            .args([
                "/inheritance:r",
                "/grant:r",
                "*S-1-5-18:(OI)(CI)F",
                "*S-1-5-32-544:(OI)(CI)F",
                "*S-1-3-4:(OI)(CI)F",
                "/Q",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => tracing::debug!(dir = %dir.display(), "storage ACL hardened"),
            other => {
                tracing::warn!(dir = %dir.display(), ?other, "could not harden storage ACL")
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = fs::set_permissions(dir, fs::Permissions::from_mode(0o700)) {
            tracing::warn!(dir = %dir.display(), %e, "could not chmod storage dir");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_dir_ends_with_app_name() {
        let d = default_dir();
        assert!(d.ends_with(APP_DIR), "{}", d.display());
    }

    #[test]
    fn writable_probe() {
        let dir = tempfile::tempdir().unwrap();
        assert!(is_writable(dir.path()));
        let fresh = dir.path().join("fresh");
        assert!(is_writable(&fresh));
        assert!(!fresh.exists(), "probe must not leave a new dir behind");
    }

    #[test]
    fn hardened_dir_is_still_usable_by_creator() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("hardened");
        let id = crate::Identity::load_or_generate(&sub).unwrap();
        let again = crate::Identity::load(&sub).unwrap().unwrap();
        assert_eq!(id.fingerprint(), again.fingerprint());
    }
}
