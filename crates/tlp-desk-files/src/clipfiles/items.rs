//! Turns the file list of a clipboard copy into protocol [`TransferItem`]s.

use std::path::{Path, PathBuf};

use tlp_desk_proto::TransferItem;

use crate::fs::unix_ms;

/// Most items (files and folders, recursively) one announcement carries. Keeps the
/// `ClipboardFiles` message far below the 16 MiB frame limit and bounds the walk.
pub const MAX_ITEMS: usize = 50_000;
/// Deepest folder nesting walked below a copied folder.
pub const MAX_DEPTH: usize = 64;

/// A copied set: the folder it was copied from and every item below it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopiedSet {
    /// Parent folder of the copied entries; `items[].rel_path` are relative to it.
    pub source_dir: PathBuf,
    /// Copied files and folders, folders before their contents, `/`-separated paths.
    pub items: Vec<TransferItem>,
}

/// Builds the items for `paths` (what Explorer put on the clipboard). Entries are taken from
/// the folder of the first path; entries from other folders are skipped (Explorer always
/// copies from one folder). Links are not followed, names that are not valid Unicode are
/// skipped, and the walk stops at [`MAX_ITEMS`] / [`MAX_DEPTH`]. `None` if nothing usable
/// remains.
pub fn build(paths: &[PathBuf]) -> Option<CopiedSet> {
    let source_dir = paths.first()?.parent()?.to_path_buf();
    let mut items = Vec::new();
    for p in paths {
        if p.parent() != Some(source_dir.as_path()) {
            tracing::debug!(path = %p.display(), "copied entry from another folder skipped");
            continue;
        }
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        add(p, name.to_owned(), 0, &mut items);
        if items.len() >= MAX_ITEMS {
            tracing::warn!(limit = MAX_ITEMS, "clipboard file list truncated");
            items.truncate(MAX_ITEMS);
            break;
        }
    }
    (!items.is_empty()).then_some(CopiedSet { source_dir, items })
}

fn add(path: &Path, rel: String, depth: usize, out: &mut Vec<TransferItem>) {
    if out.len() >= MAX_ITEMS {
        return;
    }
    let Ok(md) = std::fs::symlink_metadata(path) else {
        return;
    };
    if md.file_type().is_symlink() {
        return;
    }
    let modified_unix_ms = md.modified().map_or(0, unix_ms);
    if md.is_dir() {
        out.push(TransferItem {
            rel_path: rel.clone(),
            size: 0,
            modified_unix_ms,
            is_dir: true,
        });
        if depth >= MAX_DEPTH {
            return;
        }
        let Ok(rd) = std::fs::read_dir(path) else {
            return;
        };
        let mut children: Vec<(String, PathBuf)> = rd
            .filter_map(Result::ok)
            .filter_map(|e| Some((e.file_name().into_string().ok()?, e.path())))
            .collect();
        children.sort();
        for (name, child) in children {
            if out.len() >= MAX_ITEMS {
                return;
            }
            add(&child, format!("{rel}/{name}"), depth + 1, out);
        }
    } else {
        out.push(TransferItem {
            rel_path: rel,
            size: md.len(),
            modified_unix_ms,
            is_dir: false,
        });
    }
}

/// Whether an announced item path is safe to create below a local folder: relative,
/// `/`-separated, no empty, `.` or `..` components, no drive or root prefix, no `\` or `:`.
pub fn is_safe_rel_path(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.starts_with('/')
        && !rel.contains(['\\', ':', '\0'])
        && rel.split('/').all(|c| {
            !c.is_empty() && c != "." && c != ".." && crate::fs::path::check_name(c).is_ok()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_and_folders_become_relative_items() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("صور الرحلة");
        std::fs::create_dir_all(dir.join("اليوم 1")).unwrap();
        std::fs::write(dir.join("اليوم 1").join("شاطئ.jpg"), [1, 2, 3]).unwrap();
        std::fs::write(tmp.path().join("ملاحظات.txt"), "abc").unwrap();
        let set = build(&[dir.clone(), tmp.path().join("ملاحظات.txt")]).unwrap();
        assert_eq!(set.source_dir, tmp.path());
        let rel: Vec<_> = set
            .items
            .iter()
            .map(|i| (i.rel_path.as_str(), i.is_dir, i.size))
            .collect();
        assert_eq!(
            rel,
            [
                ("صور الرحلة", true, 0),
                ("صور الرحلة/اليوم 1", true, 0),
                ("صور الرحلة/اليوم 1/شاطئ.jpg", false, 3),
                ("ملاحظات.txt", false, 3),
            ]
        );
        assert!(set.items.iter().all(|i| is_safe_rel_path(&i.rel_path)));
    }

    #[test]
    fn other_folders_and_missing_files_are_skipped() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::write(a.path().join("x.txt"), "x").unwrap();
        std::fs::write(b.path().join("y.txt"), "y").unwrap();
        let set = build(&[
            a.path().join("x.txt"),
            b.path().join("y.txt"),
            a.path().join("gone.txt"),
        ])
        .unwrap();
        assert_eq!(set.items.len(), 1);
        assert_eq!(set.items[0].rel_path, "x.txt");
        assert_eq!(build(&[]), None);
        assert_eq!(build(&[a.path().join("gone.txt")]), None);
    }

    #[test]
    fn rel_path_safety() {
        for ok in ["a.txt", "مجلد/ملف.txt", "a/b/c"] {
            assert!(is_safe_rel_path(ok), "{ok}");
        }
        for bad in [
            "", "/a", "a//b", "../a", "a/../b", "./a", "C:/a", "a\\b", "a/",
        ] {
            assert!(!is_safe_rel_path(bad), "{bad}");
        }
    }
}
