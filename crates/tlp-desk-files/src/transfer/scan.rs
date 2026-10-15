//! Turning a selection of local files and folders into transfer items (recursive, bounded),
//! and the host-side `Stat` / `ListDir` answers the viewer uses to list a remote selection.

use std::fs::Metadata;
use std::path::{Path, PathBuf};

use tlp_desk_proto::{FsEntry, FsError, FsOp, FsReply, TransferItem};

use super::disk::to_unix_ms;
use super::error::{FilesError, PathIssue};
use super::path::{dedup_key, display_path, long_path, validate_component, MAX_DEPTH};

/// Most entries returned for one directory listing (keeps the reply far below the frame limit).
pub const MAX_LIST_ENTRIES: usize = 100_000;

/// A local selection ready to be offered.
#[derive(Debug, Clone)]
pub struct LocalSelection {
    /// Folder shown as the source (parent of the first selected path).
    pub source_dir: String,
    /// Items in offer order: each directory before its contents.
    pub items: Vec<TransferItem>,
    /// Local path of each item (same index as `items`).
    pub paths: Vec<PathBuf>,
    /// Number of paths the user selected.
    pub top_level: u32,
}

/// Lists `selection` (files and folders) recursively. Directory symlinks and junctions are
/// not followed (they could loop or leave the folder); file symlinks are sent as files.
///
/// # Errors
/// Missing paths, unreadable folders, names that cannot be transferred (not Unicode, reserved
/// on Windows), duplicate top-level names, or more than `max_items` items.
pub fn collect_local(
    selection: &[PathBuf],
    max_items: usize,
) -> Result<LocalSelection, FilesError> {
    let mut out = LocalSelection {
        source_dir: String::new(),
        items: Vec::new(),
        paths: Vec::new(),
        top_level: 0,
    };
    let mut seen = std::collections::HashSet::new();
    for p in selection {
        let abs = std::path::absolute(p).map_err(|e| FilesError::io(p, e))?;
        let abs = long_path(&abs);
        let meta = std::fs::metadata(&abs).map_err(|e| FilesError::io(p, e))?;
        let name = name_of(&abs)?;
        if !seen.insert(dedup_key(&name)) {
            return Err(FilesError::InvalidPath {
                path: display_path(p),
                issue: PathIssue::Duplicate,
            });
        }
        if out.source_dir.is_empty() {
            out.source_dir = abs.parent().map(display_path).unwrap_or_default();
        }
        out.top_level += 1;
        push(&mut out, &name, &abs, &meta, max_items)?;
        if meta.is_dir() {
            walk(&mut out, &abs, &name, max_items)?;
        }
    }
    Ok(out)
}

fn name_of(p: &Path) -> Result<String, FilesError> {
    let bad = |issue| FilesError::InvalidPath {
        path: display_path(p),
        issue,
    };
    let name = p
        .file_name()
        .ok_or_else(|| bad(PathIssue::Empty))?
        .to_str()
        .ok_or_else(|| bad(PathIssue::NotUnicode))?;
    validate_component(name).map_err(bad)?;
    Ok(name.to_string())
}

fn push(
    out: &mut LocalSelection,
    rel: &str,
    path: &Path,
    meta: &Metadata,
    max_items: usize,
) -> Result<(), FilesError> {
    if out.items.len() >= max_items {
        return Err(FilesError::TooManyItems {
            count: out.items.len() + 1,
            max: max_items,
        });
    }
    out.items.push(TransferItem {
        rel_path: rel.to_string(),
        size: if meta.is_dir() { 0 } else { meta.len() },
        modified_unix_ms: meta.modified().map(to_unix_ms).unwrap_or(0),
        is_dir: meta.is_dir(),
    });
    out.paths.push(path.to_path_buf());
    Ok(())
}

/// Depth-first walk with an explicit stack (no recursion), sorted for a stable order.
fn walk(
    out: &mut LocalSelection,
    root: &Path,
    root_rel: &str,
    max_items: usize,
) -> Result<(), FilesError> {
    let mut stack = vec![(root.to_path_buf(), root_rel.to_string(), 1usize)];
    while let Some((dir, rel, depth)) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .map_err(|e| FilesError::io(&dir, e))?
            .collect::<Result<_, _>>()
            .map_err(|e| FilesError::io(&dir, e))?;
        entries.sort_by_key(|e| e.file_name());
        let mut subdirs = Vec::new();
        for entry in entries {
            let path = entry.path();
            let ft = entry.file_type().map_err(|e| FilesError::io(&path, e))?;
            let meta = if ft.is_symlink() {
                match std::fs::metadata(&path) {
                    Ok(m) if m.is_file() => m,
                    Ok(_) => {
                        tracing::warn!("not following a directory link inside a transfer");
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!(%e, "skipping a broken link inside a transfer");
                        continue;
                    }
                }
            } else {
                entry.metadata().map_err(|e| FilesError::io(&path, e))?
            };
            let name = name_of(&path)?;
            let child_rel = format!("{rel}/{name}");
            push(out, &child_rel, &path, &meta, max_items)?;
            if meta.is_dir() {
                if depth + 1 >= MAX_DEPTH {
                    return Err(FilesError::InvalidPath {
                        path: display_path(&path),
                        issue: PathIssue::TooDeep,
                    });
                }
                subdirs.push((path, child_rel, depth + 1));
            }
        }
        // Reverse so the stack pops them in sorted order.
        stack.extend(subdirs.into_iter().rev());
    }
    Ok(())
}

/// One directory entry for the wire.
pub fn entry_of(name: String, meta: &Metadata) -> FsEntry {
    #[cfg(windows)]
    let (hidden, readonly) = {
        use std::os::windows::fs::MetadataExt;
        let attrs = meta.file_attributes();
        (attrs & 0x2 != 0, attrs & 0x1 != 0)
    };
    #[cfg(not(windows))]
    let (hidden, readonly) = (name.starts_with('.'), meta.permissions().readonly());
    FsEntry {
        is_dir: meta.is_dir(),
        size: if meta.is_dir() { 0 } else { meta.len() },
        modified_unix_ms: meta.modified().map(to_unix_ms).unwrap_or(0),
        readonly,
        hidden,
        name,
    }
}

/// Host side: answers the `Stat` / `ListDir` requests the transfer engine uses to list a
/// download selection. Other operations belong to the file-browser feature.
pub fn fs_reply(op: &FsOp) -> Result<FsReply, FsError> {
    let io = |e: std::io::Error| super::error::io_to_fs_error(&e);
    match op {
        FsOp::Stat { path } => {
            let p = checked_abs(path)?;
            let meta = std::fs::metadata(&p).map_err(io)?;
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(path.as_str())
                .to_string();
            Ok(FsReply::Entry(entry_of(name, &meta)))
        }
        FsOp::ListDir { path } => {
            let p = checked_abs(path)?;
            let mut entries = Vec::new();
            for entry in std::fs::read_dir(&p).map_err(io)? {
                let entry = entry.map_err(io)?;
                let Ok(name) = entry.file_name().into_string() else {
                    tracing::debug!("skipping a non-Unicode file name");
                    continue;
                };
                let Ok(meta) = entry.metadata() else { continue };
                if entries.len() >= MAX_LIST_ENTRIES {
                    tracing::warn!("directory listing truncated at {MAX_LIST_ENTRIES} entries");
                    break;
                }
                entries.push(entry_of(name, &meta));
            }
            Ok(FsReply::Entries(entries))
        }
        _ => Err(FsError::InvalidPath),
    }
}

/// A peer-supplied absolute path, refused if relative or naming an ADS / device.
pub fn checked_abs(path: &str) -> Result<PathBuf, FsError> {
    let p = Path::new(path);
    if path.is_empty() || !p.is_absolute() || path.contains('\0') {
        return Err(FsError::InvalidPath);
    }
    if cfg!(windows) {
        // A drive colon is the only colon allowed (`C:\a:stream` is an alternate data stream).
        let rest = path
            .strip_prefix(r"\\?\")
            .unwrap_or(path)
            .get(2..)
            .unwrap_or_default();
        if rest.contains(':') || path.starts_with(r"\\.\") {
            return Err(FsError::InvalidPath);
        }
    }
    if p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(FsError::InvalidPath);
    }
    Ok(long_path(p))
}

/// Splits a host-native path into (parent, name), accepting `\` and `/` separators.
pub fn split_remote(path: &str) -> Option<(&str, &str)> {
    let trimmed = path.trim_end_matches(['\\', '/']);
    let i = trimmed.rfind(['\\', '/'])?;
    let name = trimmed.get(i + 1..)?;
    let parent = trimmed.get(..=i)?;
    // Keep `C:\` / `/` as the parent of top-level names; strip other trailing separators.
    let parent = if parent.len() > 1 && !parent.ends_with(":\\") && !parent.ends_with(":/") {
        parent.get(..parent.len() - 1)?
    } else {
        parent
    };
    if name.is_empty() {
        None
    } else {
        Some((parent, name))
    }
}

/// Joins a host-native directory and a name with the separator the directory uses.
pub fn join_remote(dir: &str, name: &str) -> String {
    let sep = if dir.contains('\\') || !dir.contains('/') {
        '\\'
    } else {
        '/'
    };
    if dir.ends_with(['\\', '/']) {
        format!("{dir}{name}")
    } else {
        format!("{dir}{sep}{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_nested_arabic_tree_with_empty_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Ù…Ø¬Ù„Ø¯");
        std::fs::create_dir_all(root.join("ÙØ±Ø¹ÙŠ").join("ÙØ§Ø±Øº")).unwrap();
        std::fs::write(root.join("Ù…Ù„Ù 1.txt"), b"hello").unwrap();
        std::fs::write(root.join("ÙØ±Ø¹ÙŠ").join("b.bin"), vec![7u8; 1000]).unwrap();
        let single = dir.path().join("single.txt");
        std::fs::write(&single, b"x").unwrap();
        let sel = collect_local(&[root.clone(), single], 100).unwrap();
        let rels: Vec<_> = sel.items.iter().map(|i| i.rel_path.as_str()).collect();
        assert_eq!(
            rels,
            [
                "Ù…Ø¬Ù„Ø¯",
                "Ù…Ø¬Ù„Ø¯/ÙØ±Ø¹ÙŠ",
                "Ù…Ø¬Ù„Ø¯/Ù…Ù„Ù 1.txt",
                "Ù…Ø¬Ù„Ø¯/ÙØ±Ø¹ÙŠ/b.bin",
                "Ù…Ø¬Ù„Ø¯/ÙØ±Ø¹ÙŠ/ÙØ§Ø±Øº",
                "single.txt"
            ]
        );
        assert_eq!(sel.top_level, 2);
        assert_eq!(sel.items[3].size, 1000);
        assert!(sel.items[4].is_dir);
        assert!(collect_local(&[root], 3).is_err());
    }

    #[test]
    fn remote_path_helpers() {
        assert_eq!(
            split_remote(r"C:\Users\Ù…\a.txt"),
            Some((r"C:\Users\Ù…", "a.txt"))
        );
        assert_eq!(split_remote(r"C:\a.txt"), Some((r"C:\", "a.txt")));
        assert_eq!(split_remote("/home/u/dir/"), Some(("/home/u", "dir")));
        assert_eq!(split_remote("C:\\"), None);
        assert_eq!(join_remote(r"C:\", "x"), r"C:\x");
        assert_eq!(join_remote(r"C:\a", "x"), r"C:\a\x");
        assert_eq!(join_remote("/home", "x"), "/home/x");
    }

    #[cfg(windows)]
    #[test]
    fn peer_paths_are_checked() {
        assert!(checked_abs(r"C:\Users").is_ok());
        assert!(checked_abs(r"relative\x").is_err());
        assert!(checked_abs(r"C:\a\file.txt:secret").is_err());
        assert!(checked_abs(r"C:\a\..\b").is_err());
        assert!(checked_abs(r"\\.\PhysicalDrive0").is_err());
    }

    #[test]
    fn host_lists_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Ù…Ù„Ù.txt"), b"abc").unwrap();
        let reply = fs_reply(&FsOp::ListDir {
            path: dir.path().to_str().unwrap().to_string(),
        })
        .unwrap();
        let FsReply::Entries(e) = reply else { panic!() };
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].name, "Ù…Ù„Ù.txt");
        assert_eq!(e[0].size, 3);
    }
}
