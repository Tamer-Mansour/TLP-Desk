//! Receiver planning: where each incoming item goes, what already exists (resume, identical
//! files, conflicts), and whether everything stays inside the destination folder.
//!
//! Files are written to `<final name>.tlpdesk-part` and renamed into place only after their
//! BLAKE3 hash verified, so a half-written file never carries the real name. When a transfer
//! stops early, `<final name>.tlpdesk-part.meta` records how much of the partial file is valid;
//! the next offer for the same file (same size and modification time) resumes from there.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use tlp_desk_proto::TransferItem;

use super::disk::to_unix_ms;
use super::error::{FilesError, PathIssue};
use super::hash::PIECE;
use super::path::{conflict_name, dedup_key, display_path, join_under, validate_rel_path};
use super::types::ConflictPolicy;

/// Suffix of a file being received.
pub const PART_SUFFIX: &str = ".tlpdesk-part";
/// Suffix of the resume record next to a partial file.
pub const META_SUFFIX: &str = ".tlpdesk-part.meta";
/// First line of a resume record.
const META_MAGIC: &str = "tlpdesk-part 1";
/// Conflict names tried before giving up (`name (2)` .. `name (999)`).
const MAX_RENAME_TRIES: u32 = 999;

/// What happens to one incoming item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A directory (created before data arrives).
    Dir {
        /// Where.
        path: PathBuf,
    },
    /// A file written to `part` and renamed to `final_path` after verification.
    File {
        /// The temporary partial file.
        part: PathBuf,
        /// The final name.
        final_path: PathBuf,
        /// Bytes already present in `part` (piece-aligned).
        resume: u64,
    },
    /// The destination already holds a file with the same size and time: verify its hash,
    /// write nothing.
    Identical {
        /// The existing file.
        path: PathBuf,
    },
    /// An existing file is kept ([`ConflictPolicy::Skip`]).
    Skipped {
        /// The existing file.
        path: PathBuf,
    },
}

/// Result of [`plan_receive`].
#[derive(Debug, Clone)]
pub struct ReceivePlan {
    /// Canonical destination root.
    pub root: PathBuf,
    /// One target per offered item.
    pub targets: Vec<Target>,
    /// `(item, offset)` the sender should start from (resumed, identical and skipped files).
    pub resume: Vec<(u32, u64)>,
    /// Bytes still to be written.
    pub need_bytes: u64,
}

/// Plans where the `items` of an offer go under `dest` (which must exist).
///
/// # Errors
/// Invalid or duplicate item paths, paths escaping `dest` (through links), a missing or
/// non-directory `dest`, or a directory name already taken by a file.
pub fn plan_receive(
    dest: &Path,
    items: &[TransferItem],
    policy: ConflictPolicy,
) -> Result<ReceivePlan, FilesError> {
    let root = std::fs::canonicalize(dest).map_err(|e| FilesError::io(dest, e))?;
    if !root.is_dir() {
        return Err(FilesError::io(
            dest,
            std::io::Error::new(std::io::ErrorKind::NotADirectory, "not a folder"),
        ));
    }
    let mut p = Planner {
        root: root.clone(),
        policy,
        dirs: HashMap::new(),
        claimed: HashSet::new(),
        inside: HashMap::new(),
    };
    let mut seen = HashSet::new();
    let mut targets = Vec::with_capacity(items.len());
    let mut resume = Vec::new();
    let mut need_bytes = 0u64;
    for (idx, item) in items.iter().enumerate() {
        let bad = |issue| FilesError::InvalidPath {
            path: item.rel_path.clone(),
            issue,
        };
        let comps = validate_rel_path(&item.rel_path).map_err(bad)?;
        if !seen.insert(dedup_key(&item.rel_path)) {
            return Err(bad(PathIssue::Duplicate));
        }
        let (name, parents) = comps.split_last().ok_or_else(|| bad(PathIssue::Empty))?;
        let parent = p.resolve_dirs(parents)?;
        p.check_inside(&parent, &item.rel_path)?;
        let idx = u32::try_from(idx).map_err(|_| bad(PathIssue::TooLong))?;
        if item.is_dir {
            let path = p.resolve_dirs(&comps)?;
            targets.push(Target::Dir { path });
            continue;
        }
        let target = p.plan_file(&parent, name, item)?;
        match &target {
            Target::File { resume: r, .. } => {
                need_bytes = need_bytes.saturating_add(item.size.saturating_sub(*r));
                if *r > 0 {
                    resume.push((idx, *r));
                }
            }
            Target::Identical { .. } | Target::Skipped { .. } => {
                if item.size > 0 {
                    resume.push((idx, item.size));
                }
            }
            Target::Dir { .. } => {}
        }
        targets.push(target);
    }
    Ok(ReceivePlan {
        root,
        targets,
        resume,
        need_bytes,
    })
}

struct Planner {
    root: PathBuf,
    policy: ConflictPolicy,
    /// Resolved directory for each relative directory prefix (dedup key).
    dirs: HashMap<String, PathBuf>,
    /// Final paths already given to an item of this offer (dedup key of the full path).
    claimed: HashSet<String>,
    /// Parent directories already checked against the root.
    inside: HashMap<PathBuf, bool>,
}

impl Planner {
    /// The local directory for relative directory components (a directory name taken by a
    /// file is refused: we never rename folders, so resuming keeps working).
    fn resolve_dirs(&mut self, comps: &[&str]) -> Result<PathBuf, FilesError> {
        let key = dedup_key(&comps.join("/"));
        if let Some(p) = self.dirs.get(&key) {
            return Ok(p.clone());
        }
        let path = join_under(&self.root, comps);
        if path.is_file() {
            return Err(FilesError::io(
                &path,
                std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "a file has the folder's name",
                ),
            ));
        }
        self.dirs.insert(key, path.clone());
        Ok(path)
    }

    /// Refuses a destination whose existing part resolves outside the root (a junction or
    /// symlink inside the destination pointing elsewhere).
    fn check_inside(&mut self, dir: &Path, rel: &str) -> Result<(), FilesError> {
        if let Some(ok) = self.inside.get(dir) {
            return if *ok { Ok(()) } else { Err(escapes(rel)) };
        }
        let mut probe = dir.to_path_buf();
        while !probe.exists() {
            if !probe.pop() {
                break;
            }
        }
        let ok = std::fs::canonicalize(&probe).is_ok_and(|c| c.starts_with(&self.root));
        self.inside.insert(dir.to_path_buf(), ok);
        if ok {
            Ok(())
        } else {
            Err(escapes(rel))
        }
    }

    fn plan_file(
        &mut self,
        parent: &Path,
        name: &str,
        item: &TransferItem,
    ) -> Result<Target, FilesError> {
        let final_path = parent.join(name);
        if let Some(t) = self.try_resume(&final_path, item) {
            return Ok(t);
        }
        let Ok(meta) = std::fs::metadata(&final_path) else {
            if self.claim(&final_path) {
                return Ok(fresh(final_path));
            }
            return self.renamed(parent, name, item);
        };
        if meta.is_file()
            && meta.len() == item.size
            && meta.modified().map(to_unix_ms).ok() == Some(item.modified_unix_ms)
            && self.claim(&final_path)
        {
            return Ok(Target::Identical { path: final_path });
        }
        match self.policy {
            ConflictPolicy::Skip => Ok(Target::Skipped { path: final_path }),
            ConflictPolicy::Overwrite if meta.is_file() => {
                if self.claim(&final_path) {
                    Ok(fresh(final_path))
                } else {
                    self.renamed(parent, name, item)
                }
            }
            _ => self.renamed(parent, name, item),
        }
    }

    /// `name (n).ext`: resumes an earlier renamed attempt, or takes the first free name.
    fn renamed(
        &mut self,
        parent: &Path,
        name: &str,
        item: &TransferItem,
    ) -> Result<Target, FilesError> {
        for n in 2..=MAX_RENAME_TRIES {
            let cand = parent.join(conflict_name(name, n));
            if let Some(t) = self.try_resume(&cand, item) {
                return Ok(t);
            }
            if !cand.exists() && !part_of(&cand).exists() && self.claim(&cand) {
                return Ok(fresh(cand));
            }
        }
        Err(FilesError::io(
            parent.join(name),
            std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free name"),
        ))
    }

    /// A matching resume record next to `final_path` (and the final name still free).
    fn try_resume(&mut self, final_path: &Path, item: &TransferItem) -> Option<Target> {
        let valid = read_meta(final_path, item)?;
        let taken = final_path.exists() && self.policy != ConflictPolicy::Overwrite;
        if taken || !self.claim(final_path) {
            return None;
        }
        Some(Target::File {
            part: part_of(final_path),
            final_path: final_path.to_path_buf(),
            resume: valid,
        })
    }

    fn claim(&mut self, p: &Path) -> bool {
        self.claimed.insert(dedup_key(&p.to_string_lossy()))
    }
}

fn fresh(final_path: PathBuf) -> Target {
    Target::File {
        part: part_of(&final_path),
        final_path,
        resume: 0,
    }
}

fn escapes(rel: &str) -> FilesError {
    FilesError::InvalidPath {
        path: rel.to_string(),
        issue: PathIssue::Escapes,
    }
}

/// `<final>.tlpdesk-part`.
pub fn part_of(final_path: &Path) -> PathBuf {
    with_suffix(final_path, PART_SUFFIX)
}

/// `<final>.tlpdesk-part.meta`.
pub fn meta_of(final_path: &Path) -> PathBuf {
    with_suffix(final_path, META_SUFFIX)
}

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// Records that the first `valid` bytes of the partial file for `final_path` are written.
pub fn write_meta(final_path: &Path, item: &TransferItem, valid: u64) {
    let valid = valid - valid % PIECE;
    let text = format!(
        "{META_MAGIC}\n{}\n{}\n{valid}\n",
        item.size, item.modified_unix_ms
    );
    if let Err(e) = std::fs::write(meta_of(final_path), text) {
        tracing::debug!(%e, path = %display_path(final_path), "could not write a resume record");
    }
}

/// Valid prefix of a matching partial file, or `None` (no record, other file, or damaged).
/// A record that does not match is removed: the partial file will be overwritten.
pub fn read_meta(final_path: &Path, item: &TransferItem) -> Option<u64> {
    let text = std::fs::read_to_string(meta_of(final_path)).ok()?;
    let valid = parse_meta(final_path, item, &text);
    if valid.is_none() {
        let _ = std::fs::remove_file(meta_of(final_path));
    }
    valid
}

fn parse_meta(final_path: &Path, item: &TransferItem, text: &str) -> Option<u64> {
    let mut lines = text.lines();
    if lines.next()? != META_MAGIC {
        return None;
    }
    let size: u64 = lines.next()?.parse().ok()?;
    let mtime: i64 = lines.next()?.parse().ok()?;
    let valid: u64 = lines.next()?.parse().ok()?;
    let part_len = std::fs::metadata(part_of(final_path)).ok()?.len();
    let ok = size == item.size
        && mtime == item.modified_unix_ms
        && valid <= size
        && valid % PIECE == 0
        && part_len >= valid;
    ok.then_some(valid)
}

/// Removes the partial file and its resume record (best effort).
pub fn remove_partial(final_path: &Path) {
    let _ = std::fs::remove_file(part_of(final_path));
    let _ = std::fs::remove_file(meta_of(final_path));
}

/// Creates every directory the plan needs.
///
/// # Errors
/// The first directory that could not be created.
pub fn create_dirs(plan: &ReceivePlan) -> Result<(), FilesError> {
    let mut done = HashSet::new();
    for t in &plan.targets {
        let dir = match t {
            Target::Dir { path } => Some(path.as_path()),
            Target::File { final_path, .. } => final_path.parent(),
            _ => None,
        };
        if let Some(d) = dir {
            if done.insert(d.to_path_buf()) {
                std::fs::create_dir_all(d).map_err(|e| FilesError::io(d, e))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(rel: &str, size: u64) -> TransferItem {
        TransferItem {
            rel_path: rel.into(),
            size,
            modified_unix_ms: 1_700_000_000_000,
            is_dir: false,
        }
    }

    fn dir(rel: &str) -> TransferItem {
        TransferItem {
            rel_path: rel.into(),
            size: 0,
            modified_unix_ms: 0,
            is_dir: true,
        }
    }

    #[test]
    fn plans_rename_overwrite_skip() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("ملف.txt"), b"old").unwrap();
        let items = [file("ملف.txt", 10), dir("مجلد"), file("مجلد/a.txt", 5)];
        let plan = plan_receive(d.path(), &items, ConflictPolicy::Rename).unwrap();
        let Target::File {
            final_path, resume, ..
        } = &plan.targets[0]
        else {
            panic!()
        };
        assert!(final_path.ends_with("ملف (2).txt"));
        assert_eq!(*resume, 0);
        assert_eq!(plan.need_bytes, 15);
        create_dirs(&plan).unwrap();
        assert!(d.path().join("مجلد").is_dir());

        let plan = plan_receive(d.path(), &items, ConflictPolicy::Overwrite).unwrap();
        let Target::File { final_path, .. } = &plan.targets[0] else {
            panic!()
        };
        assert!(final_path.ends_with("ملف.txt"));
        let plan = plan_receive(d.path(), &items, ConflictPolicy::Skip).unwrap();
        assert!(matches!(plan.targets[0], Target::Skipped { .. }));
        assert_eq!(plan.resume, vec![(0, 10)]);
    }

    #[test]
    fn identical_file_is_not_resent() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("same.bin");
        std::fs::write(&p, b"12345").unwrap();
        let t = super::super::disk::from_unix_ms(1_700_000_000_000).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(t)
            .unwrap();
        let plan = plan_receive(d.path(), &[file("same.bin", 5)], ConflictPolicy::Rename).unwrap();
        assert!(matches!(plan.targets[0], Target::Identical { .. }));
        assert_eq!(plan.resume, vec![(0, 5)]);
        assert_eq!(plan.need_bytes, 0);
    }

    #[test]
    fn resume_record_is_used_only_when_it_matches() {
        let d = tempfile::tempdir().unwrap();
        let item = file("big.bin", 3 * PIECE + 7);
        let final_path = d.path().join("big.bin");
        std::fs::write(part_of(&final_path), vec![0u8; 100]).unwrap();
        write_meta(&final_path, &item, 2 * PIECE + 5);
        // The partial file is shorter than the record claims: not trusted.
        assert_eq!(read_meta(&final_path, &item), None);
        assert!(!meta_of(&final_path).exists(), "a stale record is removed");
        write_meta(&final_path, &item, 2 * PIECE + 5);
        std::fs::File::options()
            .write(true)
            .open(part_of(&final_path))
            .unwrap()
            .set_len(2 * PIECE)
            .unwrap();
        assert_eq!(read_meta(&final_path, &item), Some(2 * PIECE));
        let plan = plan_receive(
            d.path(),
            std::slice::from_ref(&item),
            ConflictPolicy::Rename,
        )
        .unwrap();
        assert_eq!(plan.resume, vec![(0, 2 * PIECE)]);
        // Another file with the same name does not resume.
        let other = file("big.bin", 3 * PIECE + 8);
        assert_eq!(read_meta(&final_path, &other), None);
        remove_partial(&final_path);
        assert!(!part_of(&final_path).exists());
    }

    #[test]
    fn duplicate_and_invalid_items_are_refused() {
        let d = tempfile::tempdir().unwrap();
        let dup = [file("a.txt", 1), file("A.TXT", 1)];
        if cfg!(windows) {
            assert!(plan_receive(d.path(), &dup, ConflictPolicy::Rename).is_err());
        }
        for bad in ["../x", "a/../../x", "C:/x", "x:ads", "NUL", "/abs"] {
            assert!(
                plan_receive(d.path(), &[file(bad, 1)], ConflictPolicy::Rename).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn two_items_never_get_the_same_final_name() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), b"x").unwrap();
        let items = [file("a.txt", 3), file("a (2).txt", 3)];
        let plan = plan_receive(d.path(), &items, ConflictPolicy::Rename).unwrap();
        let names: Vec<_> = plan
            .targets
            .iter()
            .map(|t| match t {
                Target::File { final_path, .. } => final_path.file_name().unwrap().to_owned(),
                _ => panic!(),
            })
            .collect();
        assert_ne!(names[0], names[1]);
    }
}
