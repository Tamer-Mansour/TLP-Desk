use std::fs;
use std::path::{Path, PathBuf};

/// Storage folder name. On Windows the product name `TLP-Desk`; paths there are
/// case-insensitive, so this is the same folder older builds created as `tlp-desk` and the
/// existing identity key (= device ID) is kept. Other OSes keep the conventional lowercase name.
#[cfg(windows)]
const APP_DIR: &str = "TLP-Desk";
#[cfg(not(windows))]
const APP_DIR: &str = "tlp-desk";

/// Default directory for identity and trust files.
///
/// Windows: `%ProgramData%\TLP-Desk` when it exists-and-is-writable or can be created, else
/// `%LOCALAPPDATA%\TLP-Desk` (attended, non-admin runs). macOS: `~/Library/Application Support/TLP-Desk`. Other OSes: `/var/lib/tlp-desk` if
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

/// `~/Library/Application Support/TLP-Desk` for a home directory (macOS convention).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn macos_dir(home: &Path) -> PathBuf {
    home.join("Library")
        .join("Application Support")
        .join(APP_DIR)
}

#[cfg(target_os = "macos")]
fn candidates() -> Vec<PathBuf> {
    std::env::var_os("HOME")
        .map(|h| vec![macos_dir(Path::new(&h))])
        .unwrap_or_default()
}

#[cfg(not(any(windows, target_os = "macos")))]
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

/// Creates `dir` if needed and (re)applies the private ACL ([`harden_dir`]) **every time**,
/// not only when the directory is new. Called before every secret is written (identity key,
/// permanent password, reconnect tickets): a directory that already existed with a weaker
/// ACL (created by an older build, by hand, or by another user first) is repaired instead of
/// trusted (audit F1).
///
/// # Errors
/// The directory cannot be created. A failed ACL change is logged, not returned.
pub fn ensure_private_dir(dir: &Path) -> Result<(), crate::CryptoError> {
    if !dir.as_os_str().is_empty() {
        fs::create_dir_all(dir)?;
        harden_dir(dir);
    }
    Ok(())
}

/// Secures the machine-wide data directory on service start (audit F1).
///
/// `%ProgramData%` lets every user create sub-folders, so a non-admin user could create
/// `%ProgramData%\TLP-Desk` before TLP-Desk is installed; as its owner they would keep
/// control of the ACL (and, through `OWNER RIGHTS`, of the files). The service (SYSTEM) runs
/// this unconditionally, so the check is "verify by repair":
///
/// 1. `takeown /A`: the owner of the folder becomes `Administrators`;
/// 2. `icacls /setowner Administrators /T`: the same for everything inside;
/// 3. `icacls /reset /T`: every explicit ACE someone added is dropped, children only inherit;
/// 4. [`harden_dir`]: inheritance off, `SYSTEM` + `Administrators` (+ `OWNER RIGHTS`) only.
///
/// The installer creates the folder with the same rights (`[Dirs]` in `TLP-Desk.iss`).
/// Unix: [`harden_dir`] only.
///
/// # Errors
/// The directory cannot be created. Failed ACL steps are logged, not returned.
pub fn secure_service_dir(dir: &Path) -> Result<(), crate::CryptoError> {
    fs::create_dir_all(dir)?;
    #[cfg(windows)]
    for (program, args) in service_dir_commands(dir) {
        run_quiet(program, &args, dir);
    }
    harden_dir(dir);
    Ok(())
}

/// The ownership and ACL reset steps of [`secure_service_dir`] before [`harden_dir`], as
/// `(program, arguments)`. Pure, so the order and the SIDs are tested.
#[cfg_attr(not(windows), allow(dead_code))]
fn service_dir_commands(dir: &Path) -> Vec<(&'static str, Vec<std::ffi::OsString>)> {
    let d = || dir.as_os_str().to_owned();
    let s = |v: &str| std::ffi::OsString::from(v);
    vec![
        ("takeown", vec![s("/F"), d(), s("/A")]),
        (
            "icacls",
            vec![
                d(),
                s("/setowner"),
                s(ADMINISTRATORS),
                s("/T"),
                s("/C"),
                s("/Q"),
            ],
        ),
        ("icacls", vec![d(), s("/reset"), s("/T"), s("/C"), s("/Q")]),
    ]
}

/// `BUILTIN\Administrators` in `icacls` SID syntax.
#[cfg_attr(not(windows), allow(dead_code))]
const ADMINISTRATORS: &str = "*S-1-5-32-544";

/// Arguments of the `icacls` call of [`harden_dir`].
#[cfg_attr(not(windows), allow(dead_code))]
fn harden_args(dir: &Path) -> Vec<std::ffi::OsString> {
    let mut v = vec![dir.as_os_str().to_owned()];
    v.extend(
        [
            "/inheritance:r",
            "/grant:r",
            "*S-1-5-18:(OI)(CI)F",
            "*S-1-5-32-544:(OI)(CI)F",
            "*S-1-3-4:(OI)(CI)F",
            "/Q",
        ]
        .map(std::ffi::OsString::from),
    );
    v
}

/// Runs a built-in Windows tool without a console window or output; logs a failure.
#[cfg(windows)]
fn run_quiet(program: &str, args: &[std::ffi::OsString], dir: &Path) -> bool {
    use std::os::windows::process::CommandExt as _;
    /// `CREATE_NO_WINDOW`: no console flashes up when the GUI calls this.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let status = std::process::Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => true,
        other => {
            tracing::warn!(dir = %dir.display(), program, ?other, "storage ACL step failed");
            false
        }
    }
}

/// Restricts a storage directory so other local users cannot read the key. Idempotent; see
/// [`ensure_private_dir`] (before every secret write) and [`secure_service_dir`] (service
/// start, also fixes the owner).
///
/// Windows: removes inherited ACEs and grants Full Control only to `SYSTEM` (S-1-5-18),
/// `Administrators` (S-1-5-32-544) and `OWNER RIGHTS` (S-1-3-4, i.e. whoever created the
/// directory/files: the service, or the attended user when it falls back to a per-user path).
/// Implemented with the built-in `icacls.exe` to stay free of `unsafe`. Failures are logged,
/// not fatal. Unix: `chmod 700`.
pub fn harden_dir(dir: &Path) {
    #[cfg(windows)]
    if run_quiet("icacls", &harden_args(dir), dir) {
        tracing::debug!(dir = %dir.display(), "storage ACL hardened");
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
    fn macos_dir_is_under_application_support() {
        let home = Path::new("/Users/\u{645}\u{62d}\u{645}\u{62f}");
        let d = macos_dir(home);
        // APP_DIR is lowercase off Windows (the folder name older builds used).
        assert!(d.ends_with(Path::new("Library/Application Support").join(APP_DIR)));
        assert!(d.starts_with(home));
    }

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

    /// The service-start repair takes ownership for Administrators first (so it may rewrite
    /// the ACL), then fixes the owner of everything inside, then drops explicit ACEs.
    #[test]
    fn service_dir_repair_steps_are_ordered_and_admin_owned() {
        let dir = Path::new(r"C:\ProgramData\TLP-Desk");
        let steps = service_dir_commands(dir);
        let progs: Vec<&str> = steps.iter().map(|(p, _)| *p).collect();
        assert_eq!(progs, ["takeown", "icacls", "icacls"]);
        let args = |i: usize| -> Vec<String> {
            steps[i]
                .1
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(args(0), ["/F", r"C:\ProgramData\TLP-Desk", "/A"]);
        assert_eq!(args(1)[0], r"C:\ProgramData\TLP-Desk");
        assert!(args(1).contains(&"/setowner".to_owned()));
        assert!(args(1).contains(&"*S-1-5-32-544".to_owned()));
        assert!(args(1).contains(&"/T".to_owned()));
        assert!(args(2).contains(&"/reset".to_owned()));
        assert!(args(2).contains(&"/T".to_owned()));
        // The final hardening grants nobody but SYSTEM, Administrators and OWNER RIGHTS.
        let h: Vec<String> = harden_args(dir)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let grants: Vec<&String> = h.iter().filter(|a| a.starts_with("*S-")).collect();
        assert_eq!(
            grants,
            [
                "*S-1-5-18:(OI)(CI)F",
                "*S-1-5-32-544:(OI)(CI)F",
                "*S-1-3-4:(OI)(CI)F"
            ]
        );
        assert!(h.contains(&"/inheritance:r".to_owned()));
    }

    /// Regression test for audit F1: a directory that already exists with inherited (weak)
    /// ACEs is hardened when a secret is written into it, not only when it is created.
    #[cfg(windows)]
    #[test]
    fn existing_dir_is_rehardened_before_a_secret_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("مفاتيح");
        fs::create_dir(&existing).unwrap();
        let inherited = |p: &Path| {
            let out = std::process::Command::new("icacls")
                .arg(p)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).contains("(I)")
        };
        assert!(
            inherited(&existing),
            "a fresh temp sub-folder inherits ACEs"
        );
        crate::Identity::generate()
            .unwrap()
            .save(&existing)
            .unwrap();
        assert!(!inherited(&existing), "identity save must re-harden");

        let other = dir.path().join("tickets");
        fs::create_dir(&other).unwrap();
        crate::ticket::write_private(&other.join("t"), b"x").unwrap();
        assert!(!inherited(&other), "write_private must re-harden");
        // Idempotent and still usable by the creator.
        ensure_private_dir(&other).unwrap();
        assert_eq!(fs::read(other.join("t")).unwrap(), b"x");
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
