//! Path rules for transfers: validating relative paths from the peer, building them from
//! local names, joining them under a destination, and conflict names.
//!
//! The receiver never trusts `TransferItem::rel_path`: it must be relative, `/`-separated and
//! free of anything Windows could interpret as leaving the destination or naming something
//! other than a plain file (`..`, drive letters, `\`, `:` alternate data streams, device names
//! such as `CON`, names that Windows silently trims). Arabic and any other Unicode is fine.

use std::path::{Path, PathBuf};

use super::error::PathIssue;

/// Maximum nesting of a relative path (directory levels).
pub const MAX_DEPTH: usize = 64;
/// Maximum length of a relative path in UTF-8 bytes.
pub const MAX_REL_PATH_BYTES: usize = 4096;
/// Maximum length of one component in UTF-16 code units (NTFS limit).
const MAX_COMPONENT_UTF16: usize = 255;
/// Room kept below [`MAX_COMPONENT_UTF16`] for the partial-file suffix and ` (n)`.
const SUFFIX_ROOM: usize = 24;

/// Checks a relative transfer path and returns its components.
///
/// # Errors
/// The first [`PathIssue`] found.
pub fn validate_rel_path(rel: &str) -> Result<Vec<&str>, PathIssue> {
    if rel.is_empty() {
        return Err(PathIssue::Empty);
    }
    if rel.len() > MAX_REL_PATH_BYTES {
        return Err(PathIssue::TooLong);
    }
    if rel.starts_with('/') {
        return Err(PathIssue::Absolute);
    }
    let comps: Vec<&str> = rel.split('/').collect();
    if comps.len() > MAX_DEPTH {
        return Err(PathIssue::TooDeep);
    }
    for c in &comps {
        validate_component(c)?;
    }
    Ok(comps)
}

/// Checks one file or directory name.
///
/// # Errors
/// The first [`PathIssue`] found.
pub fn validate_component(c: &str) -> Result<(), PathIssue> {
    match c {
        "" => return Err(PathIssue::Empty),
        "." => return Err(PathIssue::CurDir),
        ".." => return Err(PathIssue::ParentRef),
        _ => {}
    }
    if c.encode_utf16().count() > MAX_COMPONENT_UTF16 - SUFFIX_ROOM {
        return Err(PathIssue::TooLong);
    }
    for ch in c.chars() {
        match ch {
            '\\' => return Err(PathIssue::Backslash),
            ':' => return Err(PathIssue::Colon),
            '/' => return Err(PathIssue::Absolute),
            '<' | '>' | '"' | '|' | '?' | '*' => return Err(PathIssue::InvalidChar),
            c if (c as u32) < 0x20 || c == '\u{7f}' => return Err(PathIssue::InvalidChar),
            _ => {}
        }
    }
    if c.ends_with('.') || c.ends_with(' ') {
        return Err(PathIssue::TrailingDotOrSpace);
    }
    if is_reserved_name(c) {
        return Err(PathIssue::Reserved);
    }
    Ok(())
}

/// Windows device names, which open a device instead of a file even with an extension
/// (`NUL.txt`) and in any directory.
fn is_reserved_name(c: &str) -> bool {
    let stem = c.split('.').next().unwrap_or(c).trim_end_matches(' ');
    let upper = stem.to_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$" => true,
        _ => {
            let mut chars = upper.chars();
            let prefix: String = chars.by_ref().take(3).collect();
            let rest: String = chars.collect();
            (prefix == "COM" || prefix == "LPT")
                && matches!(
                    rest.as_str(),
                    "0" | "1"
                        | "2"
                        | "3"
                        | "4"
                        | "5"
                        | "6"
                        | "7"
                        | "8"
                        | "9"
                        | "\u{b9}"
                        | "\u{b2}"
                        | "\u{b3}"
                )
        }
    }
}

/// Joins validated components under `root` (no `..`, so the result stays lexically inside).
pub fn join_under(root: &Path, comps: &[&str]) -> PathBuf {
    let mut p = root.to_path_buf();
    for c in comps {
        p.push(c);
    }
    p
}

/// `name (n).ext` for the n-th conflict candidate (`n >= 2`). A leading dot is part of the
/// name, not an extension (`.config (2)`).
pub fn conflict_name(name: &str, n: u32) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => {
            let (stem, ext) = name.split_at(i);
            format!("{stem} ({n}){ext}")
        }
        _ => format!("{name} ({n})"),
    }
}

/// An absolute local path in the form Windows accepts beyond `MAX_PATH` (`\\?\C:\...`,
/// `\\?\UNC\server\share\...`). Relative or already verbatim paths are returned unchanged.
pub fn long_path(p: &Path) -> PathBuf {
    if !cfg!(windows) {
        return p.to_path_buf();
    }
    let Some(s) = p.to_str() else {
        return p.to_path_buf();
    };
    if s.starts_with(r"\\?\") || s.starts_with(r"\\.\") || !p.is_absolute() {
        return p.to_path_buf();
    }
    let s = s.replace('/', "\\");
    match s.strip_prefix(r"\\") {
        Some(unc) => PathBuf::from(format!(r"\\?\UNC\{unc}")),
        None => PathBuf::from(format!(r"\\?\{s}")),
    }
}

/// The everyday form of a path for display (strips `\\?\` / `\\?\UNC\`).
pub fn display_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.into_owned()
    }
}

/// Whether two names are the same the way the local file system compares them
/// (case-insensitive on Windows).
pub fn same_name(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.to_lowercase() == b.to_lowercase()
    } else {
        a == b
    }
}

/// Key used to detect duplicate items in one offer (Windows paths are case-insensitive).
pub fn dedup_key(rel: &str) -> String {
    if cfg!(windows) {
        rel.to_lowercase()
    } else {
        rel.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::path::Component;

    #[test]
    fn accepts_normal_and_arabic_paths() {
        assert_eq!(validate_rel_path("a/b/c.txt").unwrap(), ["a", "b", "c.txt"]);
        let ar = "مجلد عربي/ملف تجريبي 2.txt";
        assert_eq!(validate_rel_path(ar).unwrap().len(), 2);
        assert!(validate_rel_path(".config/x").is_ok());
        assert!(validate_rel_path("CONSOLE.txt").is_ok());
        assert!(validate_rel_path("COM10").is_ok());
    }

    #[test]
    fn rejects_traversal_and_windows_specials() {
        let cases = [
            ("", PathIssue::Empty),
            ("../x", PathIssue::ParentRef),
            ("a/../../x", PathIssue::ParentRef),
            ("a/./b", PathIssue::CurDir),
            ("/etc/passwd", PathIssue::Absolute),
            ("a//b", PathIssue::Empty),
            ("a/", PathIssue::Empty),
            ("C:/Windows", PathIssue::Colon),
            ("C:", PathIssue::Colon),
            ("file.txt:stream", PathIssue::Colon),
            ("a\\..\\b", PathIssue::Backslash),
            ("\\\\server\\share", PathIssue::Backslash),
            ("CON", PathIssue::Reserved),
            ("nul.txt", PathIssue::Reserved),
            ("dir/Com1.log", PathIssue::Reserved),
            ("LPT9", PathIssue::Reserved),
            ("COM\u{b9}", PathIssue::Reserved),
            ("conin$", PathIssue::Reserved),
            ("name.", PathIssue::TrailingDotOrSpace),
            ("name ", PathIssue::TrailingDotOrSpace),
            ("a?b", PathIssue::InvalidChar),
            ("a\u{0}b", PathIssue::InvalidChar),
            ("tab\there", PathIssue::InvalidChar),
        ];
        for (p, want) in cases {
            assert_eq!(validate_rel_path(p), Err(want), "{p:?}");
        }
        let deep = vec!["d"; MAX_DEPTH + 1].join("/");
        assert_eq!(validate_rel_path(&deep), Err(PathIssue::TooDeep));
        let long = "ب".repeat(300);
        assert_eq!(validate_rel_path(&long), Err(PathIssue::TooLong));
    }

    #[test]
    fn conflict_names() {
        assert_eq!(conflict_name("a.txt", 2), "a (2).txt");
        assert_eq!(conflict_name("تقرير.pdf", 3), "تقرير (3).pdf");
        assert_eq!(conflict_name("folder", 2), "folder (2)");
        assert_eq!(conflict_name(".env", 2), ".env (2)");
        assert_eq!(conflict_name("a.tar.gz", 2), "a.tar (2).gz");
    }

    #[cfg(windows)]
    #[test]
    fn long_paths_get_the_verbatim_prefix() {
        assert_eq!(
            long_path(Path::new(r"C:\Users\مستخدم")),
            PathBuf::from(r"\\?\C:\Users\مستخدم")
        );
        assert_eq!(
            long_path(Path::new(r"\\srv\share\a")),
            PathBuf::from(r"\\?\UNC\srv\share\a")
        );
        assert_eq!(
            long_path(Path::new(r"\\?\C:\x")),
            PathBuf::from(r"\\?\C:\x")
        );
        assert_eq!(display_path(Path::new(r"\\?\C:\x")), r"C:\x");
        assert_eq!(display_path(Path::new(r"\\?\UNC\s\x")), r"\\s\x");
    }

    proptest! {
        /// Whatever the peer sends, an accepted path joins to something inside the root.
        #[test]
        fn accepted_paths_stay_inside_the_root(s in "\\PC{0,80}") {
            if let Ok(comps) = validate_rel_path(&s) {
                let root = Path::new("root");
                let joined = join_under(root, &comps);
                prop_assert!(joined.starts_with(root));
                for c in joined.strip_prefix(root).unwrap().components() {
                    prop_assert!(matches!(c, Component::Normal(_)), "{:?}", c);
                }
                prop_assert!(!s.contains(':') && !s.contains('\\'));
            }
        }

        /// Paths built from separators, dots and specials are never accepted with `..`.
        #[test]
        fn traversal_mixes_are_rejected(
            parts in proptest::collection::vec(
                prop_oneof![
                    Just("..".to_string()), Just(".".to_string()), Just("".to_string()),
                    Just("a".to_string()), Just("ملف".to_string()), Just("c:".to_string()),
                    Just("x\\..".to_string()), Just("NUL".to_string()),
                ],
                1..8,
            )
        ) {
            let s = parts.join("/");
            let bad = parts.iter().any(|p| {
                p == ".." || p == "." || p.is_empty() || p.contains(':') || p.contains('\\')
                    || p == "NUL"
            });
            prop_assert_eq!(validate_rel_path(&s).is_err(), bad);
        }

        /// Valid single names (Arabic included) survive the round trip to the wire form.
        #[test]
        fn valid_names_roundtrip(name in "[a-zA-Z0-9\u{0600}-\u{06FF}_][a-zA-Z0-9\u{0600}-\u{06FF} _-]{0,30}[a-zA-Z0-9\u{0600}-\u{06FF}_]") {
            prop_assume!(validate_component(&name).is_ok());
            let wire = format!("{name}/{name}");
            prop_assert_eq!(validate_rel_path(&wire).unwrap(), vec![name.as_str(), name.as_str()]);
        }
    }
}
