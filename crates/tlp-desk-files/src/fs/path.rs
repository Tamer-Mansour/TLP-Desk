//! Validation of paths that arrive from the peer.
//!
//! Every path in an [`FsOp`](tlp_desk_proto::FsOp) is hostile. [`parse`] accepts only an
//! absolute, local, normalized path and rejects anything that could reach something other than
//! the plain file the user sees in Explorer:
//!
//! * `.` / `..` components (traversal), empty paths, NUL and control characters;
//! * NTFS alternate data streams (`file.txt:secret`), wildcards and the other characters
//!   Windows forbids in names (`< > : " | ? *`);
//! * DOS device names (`CON`, `NUL`, `COM1`, `LPT1.txt`, ...), the device namespace
//!   (`\\.\`), UNC / network paths (`\\server\share`, `\\?\UNC\`) and relative or
//!   drive-relative paths (`C:foo`);
//! * names ending in a dot or a space, which Win32 silently strips (so `a.txt.` would alias
//!   `a.txt`), and names longer than 255 UTF-16 units.
//!
//! A verbatim drive prefix (`\\?\C:\...`) is accepted and dropped; `/` is accepted as a
//! separator. The result is rebuilt from its checked components (`C:\dir\name`), so the OS
//! sees exactly what was validated. Long paths need no prefix: Rust's `std::fs` adds `\\?\`
//! itself when a path exceeds `MAX_PATH`. Arabic and every other Unicode name pass through
//! unchanged (UTF-8 here, UTF-16 at the OS boundary, lossless both ways).

use std::path::{Path, PathBuf};

use tlp_desk_proto::FsError;

/// Longest path accepted, in UTF-16 code units (the NTFS limit).
pub const MAX_PATH_UNITS: usize = 32_767;
/// Longest single name, in UTF-16 code units.
pub const MAX_NAME_UNITS: usize = 255;

/// A path that passed [`parse`]: absolute, local, normalized, free of devices and streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedPath(PathBuf);

impl CheckedPath {
    /// The validated path.
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Whether this is a drive or file-system root (`C:\`, `/`): never renamed or deleted.
    pub fn is_root(&self) -> bool {
        self.0.parent().is_none()
    }
}

/// Validates a path received from the peer. See the module docs for the rules.
///
/// # Errors
///
/// [`FsError::InvalidPath`] for anything that is not a plain absolute local path.
pub fn parse(raw: &str) -> Result<CheckedPath, FsError> {
    if raw.is_empty() || raw.encode_utf16().count() > MAX_PATH_UNITS {
        return Err(FsError::InvalidPath);
    }
    if raw.chars().any(char::is_control) {
        return Err(FsError::InvalidPath);
    }
    platform::parse(raw).map(CheckedPath)
}

/// Validates one file or folder name (no separators), as used for "new folder" and "rename"
/// in the UI before it builds a full path. Same rules as a component of [`parse`].
///
/// # Errors
///
/// [`FsError::InvalidPath`] if the name is empty, `.`/`..`, contains a separator or a
/// forbidden character, is a device name, or is too long.
pub fn check_name(name: &str) -> Result<(), FsError> {
    if name.contains(['/', '\\']) || name.chars().any(char::is_control) {
        return Err(FsError::InvalidPath);
    }
    platform::check_component(name)
}

/// Whether `path` lies inside one of `roots` (component-wise; case-insensitive on Windows).
/// An empty `roots` allows everything.
pub fn within_roots(path: &Path, roots: &[PathBuf]) -> bool {
    roots.is_empty() || roots.iter().any(|r| starts_with(path, r))
}

fn starts_with(path: &Path, root: &Path) -> bool {
    let mut p = path.components();
    for rc in root.components() {
        match p.next() {
            Some(pc) if platform::same_component(pc.as_os_str(), rc.as_os_str()) => {}
            _ => return false,
        }
    }
    true
}

/// Shared component check (both platforms): not empty, not `.`/`..`, not too long.
fn check_common(name: &str) -> Result<(), FsError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.encode_utf16().count() > MAX_NAME_UNITS
    {
        return Err(FsError::InvalidPath);
    }
    Ok(())
}

#[cfg(windows)]
mod platform {
    use std::ffi::OsStr;
    use std::path::PathBuf;

    use tlp_desk_proto::FsError;

    /// Characters Windows forbids in names; `:` also blocks alternate data streams.
    const FORBIDDEN: &[char] = &['<', '>', ':', '"', '|', '?', '*'];
    /// DOS device names, reserved in every directory and with any extension.
    const DEVICES: &[&str] = &["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];

    pub(super) fn parse(raw: &str) -> Result<PathBuf, FsError> {
        let s = raw.replace('/', "\\");
        // A verbatim drive path is fine (it only lifts MAX_PATH); every other `\\` form is a
        // device, a network share or a verbatim UNC path.
        let s = match s.strip_prefix(r"\\?\") {
            Some(rest) => rest.to_owned(),
            None => s,
        };
        let mut chars = s.chars();
        let (Some(letter), Some(':')) = (chars.next(), chars.next()) else {
            return Err(FsError::InvalidPath);
        };
        if !letter.is_ascii_alphabetic() {
            return Err(FsError::InvalidPath);
        }
        let rest = chars.as_str();
        // `C:` alone means the drive root; `C:foo` is drive-relative (depends on a hidden
        // per-drive current directory) and is refused.
        if !rest.is_empty() && !rest.starts_with('\\') {
            return Err(FsError::InvalidPath);
        }
        let mut out = PathBuf::from(format!("{}:\\", letter.to_ascii_uppercase()));
        for comp in rest.split('\\').filter(|c| !c.is_empty()) {
            check_component(comp)?;
            out.push(comp);
        }
        Ok(out)
    }

    pub(super) fn check_component(name: &str) -> Result<(), FsError> {
        super::check_common(name)?;
        if name.contains(FORBIDDEN) || name.ends_with(['.', ' ']) {
            return Err(FsError::InvalidPath);
        }
        // `NUL.txt` and `com1 .log` are devices too: compare the part before the first dot,
        // without trailing spaces.
        let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
        let upper = stem.to_ascii_uppercase();
        if DEVICES.contains(&upper.as_str()) || is_port(&upper) {
            return Err(FsError::InvalidPath);
        }
        Ok(())
    }

    /// `COM0`-`COM9`, `LPT0`-`LPT9`, including the superscript digits Windows also accepts.
    fn is_port(upper: &str) -> bool {
        let Some(rest) = upper
            .strip_prefix("COM")
            .or_else(|| upper.strip_prefix("LPT"))
        else {
            return false;
        };
        let mut c = rest.chars();
        matches!((c.next(), c.next()), (Some(d), None) if d.is_ascii_digit() || matches!(d, '¹' | '²' | '³'))
    }

    pub(super) fn same_component(a: &OsStr, b: &OsStr) -> bool {
        match (a.to_str(), b.to_str()) {
            // NTFS compares names case-insensitively; `to_lowercase` is a close, safe
            // approximation of its upcase table for an allow-list check.
            (Some(a), Some(b)) => {
                a.trim_end_matches('\\').to_lowercase() == b.trim_end_matches('\\').to_lowercase()
            }
            _ => a == b,
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use std::ffi::OsStr;
    use std::path::PathBuf;

    use tlp_desk_proto::FsError;

    pub(super) fn parse(raw: &str) -> Result<PathBuf, FsError> {
        let Some(rest) = raw.strip_prefix('/') else {
            return Err(FsError::InvalidPath);
        };
        let mut out = PathBuf::from("/");
        for comp in rest.split('/').filter(|c| !c.is_empty()) {
            check_component(comp)?;
            out.push(comp);
        }
        Ok(out)
    }

    pub(super) fn check_component(name: &str) -> Result<(), FsError> {
        super::check_common(name)
    }

    pub(super) fn same_component(a: &OsStr, b: &OsStr) -> bool {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[cfg(windows)]
    #[test]
    fn accepts_plain_absolute_paths_and_normalizes() {
        let ok = |s: &str| parse(s).unwrap().as_path().to_path_buf();
        assert_eq!(ok("c:"), PathBuf::from(r"C:\"));
        assert_eq!(ok(r"C:\"), PathBuf::from(r"C:\"));
        assert_eq!(ok("C:/Users//x/"), PathBuf::from(r"C:\Users\x"));
        assert_eq!(ok(r"\\?\D:\data\a.txt"), PathBuf::from(r"D:\data\a.txt"));
        // Arabic, mixed scripts and digits pass through byte-exact.
        let ar = r"C:\Users\مستخدم\سطح المكتب\ملف تجريبي 2024.txt";
        assert_eq!(ok(ar), PathBuf::from(ar));
        assert!(parse(r"C:\").unwrap().is_root());
        assert!(!parse(r"C:\a").unwrap().is_root());
    }

    #[cfg(windows)]
    #[test]
    fn rejects_traversal_devices_streams_and_shares() {
        for bad in [
            "",
            "relative\\path",
            "C:foo",
            r"C:\a\..\b",
            r"C:\a\.\b",
            r"C:\..",
            r"C:\file.txt:secret",
            r"C:\file.txt::$DATA",
            r"C:\a\NUL",
            r"C:\a\nul.txt",
            r"C:\a\COM1",
            r"C:\a\lpt9.log",
            r"C:\a\com¹",
            r"C:\a\CON .txt",
            r"\\.\PhysicalDrive0",
            r"\\server\share\x",
            r"\\?\UNC\server\share",
            r"\\?\GLOBALROOT\Device",
            r"C:\a\b.",
            r"C:\a\b ",
            r"C:\a\wild*",
            r"C:\a\q?",
            "C:\\a\\nul\0byte",
            "C:\\a\\tab\there",
            "1:\\x",
            r"\Windows",
        ] {
            assert_eq!(parse(bad), Err(FsError::InvalidPath), "{bad:?}");
        }
        assert_eq!(
            parse(&format!(r"C:\{}", "x".repeat(256))),
            Err(FsError::InvalidPath)
        );
        assert!(parse(&format!(r"C:\{}", "x".repeat(255))).is_ok());
        // Ordinary names that only look special are fine.
        for ok in [
            r"C:\a\CONSOLE",
            r"C:\a\com10",
            r"C:\a\.hidden",
            r"C:\a\nul_x",
        ] {
            assert!(parse(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn names_are_checked_like_components() {
        assert!(check_name("مجلد جديد").is_ok());
        assert!(check_name("report (1).pdf").is_ok());
        for bad in ["", ".", "..", "a/b", "a\\b", "x\u{7}"] {
            assert_eq!(check_name(bad), Err(FsError::InvalidPath), "{bad:?}");
        }
        #[cfg(windows)]
        for bad in ["aux", "a:b", "trailing.", "x*"] {
            assert_eq!(check_name(bad), Err(FsError::InvalidPath), "{bad:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn roots_allow_list_is_component_wise_and_case_insensitive() {
        let roots = vec![PathBuf::from(r"C:\Users\Public"), PathBuf::from(r"D:\")];
        let p = |s: &str| parse(s).unwrap();
        assert!(within_roots(p(r"c:\users\public\x").as_path(), &roots));
        assert!(within_roots(p(r"C:\Users\Public").as_path(), &roots));
        assert!(within_roots(p(r"D:\anything").as_path(), &roots));
        assert!(!within_roots(p(r"C:\Users\PublicX").as_path(), &roots));
        assert!(!within_roots(p(r"C:\Users").as_path(), &roots));
        assert!(within_roots(p(r"E:\x").as_path(), &[]));
    }

    /// Any component a random generator can build out of dangerous pieces.
    fn hostile_component() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("..".to_owned()),
            Just(".".to_owned()),
            "[a-zA-Z\u{0627}-\u{064A}]{1,8}:[a-zA-Z$]{0,6}",
            "(CON|PRN|AUX|NUL|COM[0-9]|LPT[0-9])(\\.[a-z]{1,3})?",
            "[a-z]{1,6}[. ]",
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        /// Never panics, and whatever it accepts is absolute and free of `..`/`.`.
        #[test]
        fn never_panics_and_accepted_paths_are_clean(raw in "\\PC{0,64}") {
            if let Ok(p) = parse(&raw) {
                prop_assert!(p.as_path().is_absolute());
                for c in p.as_path().components() {
                    prop_assert!(!matches!(c, std::path::Component::ParentDir | std::path::Component::CurDir));
                }
            }
        }

        /// A hostile component anywhere in an otherwise valid path is rejected.
        #[cfg(windows)]
        #[test]
        fn hostile_components_are_rejected(
            before in proptest::collection::vec("[a-z\u{0627}-\u{064A}]{1,6}", 0..3),
            bad in hostile_component(),
            after in proptest::collection::vec("[a-z]{1,6}", 0..3),
            sep in prop_oneof![Just("\\"), Just("/")],
        ) {
            let mut parts = vec!["C:".to_owned()];
            parts.extend(before);
            parts.push(bad);
            parts.extend(after);
            let raw = parts.join(sep);
            prop_assert_eq!(parse(&raw), Err(FsError::InvalidPath), "{}", raw);
        }

        /// Plain Arabic/Latin names always round-trip exactly.
        #[cfg(windows)]
        #[test]
        fn plain_names_round_trip(
            names in proptest::collection::vec("[a-zA-Z0-9\u{0621}-\u{064A}_\\-]([a-zA-Z0-9\u{0621}-\u{064A} _\\-]{0,10}[a-zA-Z0-9\u{0621}-\u{064A}_\\-])?", 1..5),
        ) {
            let raw = format!("C:\\{}", names.join("\\"));
            // A generated name may happen to be a device name; skip those.
            prop_assume!(names.iter().all(|n| check_name(n).is_ok()));
            let p = parse(&raw).unwrap();
            prop_assert_eq!(p.as_path().to_str(), Some(raw.as_str()));
        }
    }
}
