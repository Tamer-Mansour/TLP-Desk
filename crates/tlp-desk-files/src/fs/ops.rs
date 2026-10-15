//! The file-system operations themselves (blocking; the feature runs them on tokio's blocking
//! pool). Every path is validated by [`super::path`] and checked against the host's allowed
//! roots first; every OS error is mapped to an [`FsError`].

use std::cmp::Ordering;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tlp_desk_proto::{FsEntry, FsError, FsOp, FsReply};

use super::path::{self, CheckedPath};
use super::{roots, FsConfig};

/// Most entries one `ListDir` answer carries. The v4 protocol has no continuation, so a larger
/// directory is cut here (the first entries in directory order, then sorted); this keeps the
/// answer far below the 16 MiB frame limit even with 255-character names.
pub const MAX_LIST_ENTRIES: usize = 50_000;
/// Budget for the names in one `ListDir` answer (UTF-8 bytes), for the same reason.
pub const MAX_LIST_NAME_BYTES: usize = 8 * 1024 * 1024;

/// Runs one operation for the peer under `cfg`.
///
/// # Errors
///
/// The [`FsError`] to send back: `InvalidPath` for a rejected path or one outside the allowed
/// roots, otherwise the mapped OS error.
pub fn execute(op: &FsOp, cfg: &FsConfig) -> Result<FsReply, FsError> {
    match op {
        FsOp::Roots => Ok(FsReply::Roots(roots::list(cfg))),
        FsOp::ListDir { path } => list_dir(&checked(path, cfg)?).map(FsReply::Entries),
        FsOp::Stat { path } => stat(&checked(path, cfg)?).map(FsReply::Entry),
        FsOp::MakeDir { path } => make_dir(&checked(path, cfg)?).map(|()| FsReply::Done),
        FsOp::Rename { from, to } => {
            rename(&checked(from, cfg)?, &checked(to, cfg)?).map(|()| FsReply::Done)
        }
        FsOp::Delete { path, recursive } => {
            delete(&checked(path, cfg)?, *recursive).map(|()| FsReply::Done)
        }
    }
}

/// Validates `raw` and checks it against the allowed roots, also after resolving links
/// (a junction inside an allowed folder must not lead outside it).
fn checked(raw: &str, cfg: &FsConfig) -> Result<CheckedPath, FsError> {
    let p = path::parse(raw)?;
    if cfg.allowed_roots.is_empty() {
        return Ok(p);
    }
    if !path::within_roots(p.as_path(), &cfg.allowed_roots) {
        return Err(FsError::PermissionDenied);
    }
    if let Some(real) = resolve_existing(p.as_path()) {
        let roots: Vec<PathBuf> = cfg
            .allowed_roots
            .iter()
            .map(|r| std::fs::canonicalize(r).map_or_else(|_| r.clone(), strip_verbatim))
            .collect();
        if !path::within_roots(&real, &roots) {
            return Err(FsError::PermissionDenied);
        }
    }
    Ok(p)
}

/// Canonical form of the deepest existing ancestor of `p` (links resolved), without the
/// `\\?\` prefix `canonicalize` adds on Windows.
fn resolve_existing(p: &Path) -> Option<PathBuf> {
    p.ancestors()
        .find_map(|a| std::fs::canonicalize(a).ok())
        .map(strip_verbatim)
}

fn strip_verbatim(p: PathBuf) -> PathBuf {
    match p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => p,
    }
}

/// Entries of a directory: folders first, then by name (case-insensitive, then exact).
fn list_dir(dir: &CheckedPath) -> Result<Vec<FsEntry>, FsError> {
    let rd = std::fs::read_dir(dir.as_path()).map_err(|e| map_io(&e))?;
    let mut out = Vec::new();
    let mut name_bytes = 0usize;
    let mut skipped = 0usize;
    for entry in rd {
        let Ok(entry) = entry else {
            skipped = skipped.saturating_add(1);
            continue;
        };
        // A name that is not valid Unicode (an unpaired UTF-16 surrogate on NTFS) cannot be
        // represented in the protocol's UTF-8 strings; it is left out rather than mangled.
        let Ok(name) = entry.file_name().into_string() else {
            skipped = skipped.saturating_add(1);
            continue;
        };
        let Ok(md) = entry.metadata() else {
            skipped = skipped.saturating_add(1);
            continue;
        };
        name_bytes = name_bytes.saturating_add(name.len());
        if out.len() >= MAX_LIST_ENTRIES || name_bytes > MAX_LIST_NAME_BYTES {
            tracing::warn!(dir = %dir.as_path().display(), shown = out.len(), "directory listing truncated");
            break;
        }
        out.push(entry_from(name, &md));
    }
    if skipped > 0 {
        tracing::debug!(dir = %dir.as_path().display(), skipped, "entries left out of a listing");
    }
    sort_entries(&mut out);
    Ok(out)
}

/// Folders first, then case-insensitive name order (ties broken by the exact name, so the
/// order is total and stable).
pub fn sort_entries(entries: &mut [FsEntry]) {
    entries.sort_by_cached_key(|e| (!e.is_dir, e.name.to_lowercase(), e.name.clone()));
}

fn stat(p: &CheckedPath) -> Result<FsEntry, FsError> {
    let md = std::fs::metadata(p.as_path()).map_err(|e| map_io(&e))?;
    let name = p.as_path().file_name().map_or_else(
        || p.as_path().to_string_lossy().into_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    Ok(entry_from(name, &md))
}

fn make_dir(p: &CheckedPath) -> Result<(), FsError> {
    if std::fs::symlink_metadata(p.as_path()).is_ok() {
        return Err(FsError::AlreadyExists);
    }
    std::fs::create_dir_all(p.as_path()).map_err(|e| map_io(&e))
}

fn rename(from: &CheckedPath, to: &CheckedPath) -> Result<(), FsError> {
    if from.is_root() || to.is_root() {
        return Err(FsError::InvalidPath);
    }
    std::fs::symlink_metadata(from.as_path()).map_err(|e| map_io(&e))?;
    // `std::fs::rename` replaces an existing file on Windows; a remote rename must never
    // overwrite silently. A case-only rename (`a.txt` -> `A.txt`) is the same entry.
    let same_entry = path::within_roots(from.as_path(), &[to.as_path().to_path_buf()])
        && path::within_roots(to.as_path(), &[from.as_path().to_path_buf()]);
    if !same_entry && std::fs::symlink_metadata(to.as_path()).is_ok() {
        return Err(FsError::AlreadyExists);
    }
    if to.as_path().starts_with(from.as_path()) && !same_entry {
        // Moving a folder into itself.
        return Err(FsError::InvalidPath);
    }
    std::fs::rename(from.as_path(), to.as_path()).map_err(|e| map_io(&e))
}

fn delete(p: &CheckedPath, recursive: bool) -> Result<(), FsError> {
    if p.is_root() {
        return Err(FsError::InvalidPath);
    }
    let md = std::fs::symlink_metadata(p.as_path()).map_err(|e| map_io(&e))?;
    let result = if md.is_dir() || is_dir_link(&md) {
        // A link to a folder is removed as a link; `remove_dir_all` never follows links.
        if recursive && !is_dir_link(&md) {
            std::fs::remove_dir_all(p.as_path())
        } else {
            std::fs::remove_dir(p.as_path())
        }
    } else {
        std::fs::remove_file(p.as_path())
    };
    result.map_err(|e| map_io(&e))
}

#[cfg(windows)]
fn is_dir_link(md: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::FileTypeExt;
    md.file_type().is_symlink_dir()
}

#[cfg(not(windows))]
fn is_dir_link(_md: &std::fs::Metadata) -> bool {
    false
}

fn entry_from(name: String, md: &std::fs::Metadata) -> FsEntry {
    let is_dir = md.is_dir() || is_dir_link(md);
    let hidden = is_hidden(&name, md);
    FsEntry {
        size: if is_dir { 0 } else { md.len() },
        modified_unix_ms: md.modified().map_or(0, unix_ms),
        readonly: md.permissions().readonly(),
        hidden,
        is_dir,
        name,
    }
}

#[cfg(windows)]
fn is_hidden(_name: &str, md: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    md.file_attributes() & (HIDDEN | SYSTEM) != 0
}

#[cfg(not(windows))]
fn is_hidden(name: &str, _md: &std::fs::Metadata) -> bool {
    name.starts_with('.')
}

/// Milliseconds since the Unix epoch, negative before 1970, saturating.
pub fn unix_ms(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
        Err(e) => i64::try_from(e.duration().as_millis()).map_or(i64::MIN, |v| -v),
    }
}

/// Maps an OS error to the protocol's [`FsError`].
pub fn map_io(e: &io::Error) -> FsError {
    #[cfg(windows)]
    if let Some(code) = e.raw_os_error() {
        // Win32 codes std does not give a specific kind for (or maps too coarsely).
        match code {
            32 | 33 => return FsError::Busy,     // sharing / lock violation
            39 | 112 => return FsError::NoSpace, // handle disk full / disk full
            145 => return FsError::NotEmpty,     // directory not empty
            123 | 161 | 267 => return FsError::InvalidPath, // bad name / bad path / not a dir
            _ => {}
        }
    }
    match e.kind() {
        io::ErrorKind::NotFound => FsError::NotFound,
        io::ErrorKind::PermissionDenied => FsError::PermissionDenied,
        io::ErrorKind::AlreadyExists => FsError::AlreadyExists,
        io::ErrorKind::DirectoryNotEmpty => FsError::NotEmpty,
        io::ErrorKind::StorageFull => FsError::NoSpace,
        io::ErrorKind::ResourceBusy => FsError::Busy,
        io::ErrorKind::InvalidFilename | io::ErrorKind::NotADirectory => FsError::InvalidPath,
        _ => FsError::Other(e.to_string()),
    }
}

/// Orders two entries like [`sort_entries`] (for callers sorting their own lists).
pub fn entry_order(a: &FsEntry, b: &FsEntry) -> Ordering {
    (!a.is_dir)
        .cmp(&!b.is_dir)
        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        .then_with(|| a.name.cmp(&b.name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn s(p: &Path) -> String {
        p.to_str().unwrap().to_owned()
    }

    fn run(op: FsOp) -> Result<FsReply, FsError> {
        execute(&op, &FsConfig::default())
    }

    #[test]
    fn list_stat_mkdir_rename_delete_with_arabic_names() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let dir = root.join("مجلد المشروع");
        let file = root.join("ملف تجريبي 2024.txt");
        std::fs::write(&file, "مرحبا").unwrap();
        assert_eq!(run(FsOp::MakeDir { path: s(&dir) }), Ok(FsReply::Done));
        assert_eq!(
            run(FsOp::MakeDir { path: s(&dir) }),
            Err(FsError::AlreadyExists)
        );
        let Ok(FsReply::Entries(list)) = run(FsOp::ListDir { path: s(root) }) else {
            panic!("no listing");
        };
        let names: Vec<_> = list.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["مجلد المشروع", "ملف تجريبي 2024.txt"]);
        assert!(list[0].is_dir && !list[1].is_dir);
        assert_eq!(list[1].size, "مرحبا".len() as u64);
        assert!(list[1].modified_unix_ms > 1_600_000_000_000);

        let Ok(FsReply::Entry(e)) = run(FsOp::Stat { path: s(&file) }) else {
            panic!("no stat");
        };
        assert_eq!(e.name, "ملف تجريبي 2024.txt");

        let moved = dir.join("نسخة.txt");
        assert_eq!(
            run(FsOp::Rename {
                from: s(&file),
                to: s(&moved)
            }),
            Ok(FsReply::Done)
        );
        assert!(moved.exists() && !file.exists());

        // Not empty without `recursive`, gone with it.
        assert_eq!(
            run(FsOp::Delete {
                path: s(&dir),
                recursive: false
            }),
            Err(FsError::NotEmpty)
        );
        assert_eq!(
            run(FsOp::Delete {
                path: s(&dir),
                recursive: true
            }),
            Ok(FsReply::Done)
        );
        assert!(!dir.exists());
        assert_eq!(run(FsOp::Stat { path: s(&dir) }), Err(FsError::NotFound));
    }

    #[test]
    fn rename_never_overwrites_but_allows_case_change() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.txt");
        let b = tmp.path().join("b.txt");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        assert_eq!(
            run(FsOp::Rename {
                from: s(&a),
                to: s(&b)
            }),
            Err(FsError::AlreadyExists)
        );
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "b");
        #[cfg(windows)]
        {
            let upper = tmp.path().join("A.txt");
            assert_eq!(
                run(FsOp::Rename {
                    from: s(&a),
                    to: s(&upper)
                }),
                Ok(FsReply::Done)
            );
            let names: Vec<_> = std::fs::read_dir(tmp.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            assert!(names.contains(&"A.txt".to_owned()), "{names:?}");
        }
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        assert_eq!(
            run(FsOp::Rename {
                from: s(&sub),
                to: s(&sub.join("inner"))
            }),
            Err(FsError::InvalidPath)
        );
    }

    #[test]
    fn roots_cannot_be_deleted_or_renamed_and_bad_paths_fail() {
        #[cfg(windows)]
        let root = r"C:\";
        #[cfg(not(windows))]
        let root = "/";
        assert_eq!(
            run(FsOp::Delete {
                path: root.into(),
                recursive: true
            }),
            Err(FsError::InvalidPath)
        );
        assert_eq!(
            run(FsOp::ListDir {
                path: "relative".into()
            }),
            Err(FsError::InvalidPath)
        );
    }

    #[test]
    fn allowed_roots_confine_every_operation() {
        let tmp = tempfile::tempdir().unwrap();
        let inside = tmp.path().join("inside");
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&inside).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let cfg = FsConfig {
            allowed_roots: vec![inside.clone()],
        };
        assert!(execute(&FsOp::ListDir { path: s(&inside) }, &cfg).is_ok());
        assert_eq!(
            execute(&FsOp::ListDir { path: s(&outside) }, &cfg),
            Err(FsError::PermissionDenied)
        );
        assert_eq!(
            execute(
                &FsOp::Rename {
                    from: s(&inside),
                    to: s(&outside.join("x"))
                },
                &cfg
            ),
            Err(FsError::PermissionDenied)
        );
        let Ok(FsReply::Roots(r)) = execute(&FsOp::Roots, &cfg) else {
            panic!("no roots");
        };
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].path, s(&inside));
    }

    #[test]
    fn listing_ten_thousand_entries_is_fast_and_sorted() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..10_000u32 {
            let name = if i % 2 == 0 {
                format!("ملف {i:05}.txt")
            } else {
                format!("File {i:05}.bin")
            };
            std::fs::File::create(tmp.path().join(name)).unwrap();
        }
        for i in 0..10u32 {
            std::fs::create_dir(tmp.path().join(format!("z-dir {i}"))).unwrap();
        }
        let start = Instant::now();
        let Ok(FsReply::Entries(list)) = run(FsOp::ListDir {
            path: s(tmp.path()),
        }) else {
            panic!("no listing");
        };
        let took = start.elapsed();
        assert_eq!(list.len(), 10_010);
        assert!(list[..10].iter().all(|e| e.is_dir));
        assert!(list
            .windows(2)
            .all(|w| entry_order(&w[0], &w[1]) != Ordering::Greater));
        assert!(took < Duration::from_secs(1), "listing took {took:?}");
    }

    #[test]
    fn unix_ms_handles_both_sides_of_the_epoch() {
        assert_eq!(unix_ms(UNIX_EPOCH + Duration::from_millis(1500)), 1500);
        assert_eq!(unix_ms(UNIX_EPOCH - Duration::from_millis(1500)), -1500);
    }

    #[test]
    fn io_errors_map_to_fs_errors() {
        assert_eq!(
            map_io(&io::Error::from(io::ErrorKind::NotFound)),
            FsError::NotFound
        );
        assert_eq!(
            map_io(&io::Error::from(io::ErrorKind::PermissionDenied)),
            FsError::PermissionDenied
        );
        #[cfg(windows)]
        {
            assert_eq!(map_io(&io::Error::from_raw_os_error(32)), FsError::Busy);
            assert_eq!(map_io(&io::Error::from_raw_os_error(112)), FsError::NoSpace);
            assert_eq!(
                map_io(&io::Error::from_raw_os_error(145)),
                FsError::NotEmpty
            );
        }
    }
}
