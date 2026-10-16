//! Asking what to do when a file already exists at the destination.
//!
//! With [`ConflictPolicy::Ask`] the receiving side first lists the clashes
//! ([`detect_conflicts`]), emits [`TransferEvent::ConflictsFound`] and waits for the user's
//! [`ConflictResolution`]. Nothing is written before the answer, and identical files (same
//! size and time: already transferred) never count as clashes.
//!
//! [`TransferEvent::ConflictsFound`]: super::TransferEvent::ConflictsFound

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use tlp_desk_proto::TransferItem;
use tokio::sync::oneshot;

use super::disk::to_unix_ms;
use super::error::FilesError;
use super::path::{display_path, join_under, validate_rel_path};
use super::plan::{read_meta, Decisions};
use super::session::SessionState;
use super::state::lock;
use super::types::{ConflictPolicy, TransferEvent, TransferId};

/// Most clashes listed in one [`ConflictRequest`] (the total is always reported).
pub const MAX_LISTED_CONFLICTS: usize = 500;

/// One incoming file that would collide with an existing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileConflict {
    /// Index of the item in the offer.
    pub index: u32,
    /// `/`-separated path relative to the destination.
    pub rel_path: String,
    /// Size of the file that exists.
    pub existing_size: u64,
    /// Modification time of the file that exists (Unix ms).
    pub existing_modified_unix_ms: i64,
    /// Size of the incoming file.
    pub incoming_size: u64,
    /// Modification time of the incoming file (Unix ms).
    pub incoming_modified_unix_ms: i64,
    /// Valid bytes of an interrupted earlier transfer of this very file, if any: choosing
    /// [`ConflictChoice::Resume`] continues from there.
    pub partial_bytes: Option<u64>,
}

/// What to do with one clashing file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    /// Keep both: the new file becomes `name (2).ext`.
    KeepBoth,
    /// Replace the existing file.
    Overwrite,
    /// Keep the existing file, drop the incoming one.
    Skip,
    /// Continue the interrupted earlier transfer (`partial_bytes`), replacing the old file
    /// when it completes.
    Resume,
}

impl ConflictChoice {
    /// The planner policy that implements this choice.
    pub fn policy(self) -> ConflictPolicy {
        match self {
            Self::KeepBoth => ConflictPolicy::Rename,
            // A resume record is honoured under `Overwrite`.
            Self::Overwrite | Self::Resume => ConflictPolicy::Overwrite,
            Self::Skip => ConflictPolicy::Skip,
        }
    }
}

/// The user's answer to a [`ConflictRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictResolution {
    /// Applied to every clash not named in `per_file` ("apply to all").
    pub default: ConflictChoice,
    /// Choices for single files, by [`FileConflict::index`].
    pub per_file: Vec<(u32, ConflictChoice)>,
}

impl ConflictResolution {
    /// The same choice for all clashes.
    pub fn all(choice: ConflictChoice) -> Self {
        Self {
            default: choice,
            per_file: Vec::new(),
        }
    }

    pub(crate) fn decisions(&self) -> Decisions {
        self.per_file
            .iter()
            .fold(Decisions::uniform(self.default.policy()), |d, (i, c)| {
                d.with_item(*i, c.policy())
            })
    }
}

/// The question the UI must answer with [`crate::FileTransfer::resolve_conflicts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictRequest {
    /// The transfer waiting for the answer.
    pub id: TransferId,
    /// Session the transfer belongs to.
    pub session_key: u64,
    /// `true` if this side started the transfer (viewer), `false` on the host.
    pub initiated_locally: bool,
    /// Name of the first top-level item, for the title.
    pub label: String,
    /// The destination folder.
    pub dest_dir: String,
    /// Number of clashes (may exceed `conflicts.len()`).
    pub total_conflicts: u32,
    /// The first [`MAX_LISTED_CONFLICTS`] clashes.
    pub conflicts: Vec<FileConflict>,
}

/// Lists the files of `items` that already exist under `dest` (and are not identical).
///
/// # Errors
/// A missing or non-directory `dest`, or an invalid item path.
pub fn detect_conflicts(
    dest: &Path,
    items: &[TransferItem],
) -> Result<Vec<FileConflict>, FilesError> {
    let root = std::fs::canonicalize(dest).map_err(|e| FilesError::io(dest, e))?;
    let mut out = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        if item.is_dir {
            continue;
        }
        let comps = validate_rel_path(&item.rel_path).map_err(|issue| FilesError::InvalidPath {
            path: item.rel_path.clone(),
            issue,
        })?;
        let path = join_under(&root, &comps);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        let modified = meta.modified().map(to_unix_ms).unwrap_or_default();
        if meta.is_file() && meta.len() == item.size && modified == item.modified_unix_ms {
            continue;
        }
        out.push(FileConflict {
            index: u32::try_from(idx).unwrap_or(u32::MAX),
            rel_path: item.rel_path.clone(),
            existing_size: meta.len(),
            existing_modified_unix_ms: modified,
            incoming_size: item.size,
            incoming_modified_unix_ms: item.modified_unix_ms,
            partial_bytes: read_meta(&path, item).filter(|v| *v > 0),
        });
    }
    Ok(out)
}

/// An answer to a prompt: `None` cancels the transfer.
pub(crate) type Answer = Option<ConflictResolution>;

/// Prompts waiting for an answer: transfer id -> (session key, where to send the answer).
type Waiters = HashMap<TransferId, (u64, oneshot::Sender<Answer>)>;

/// Open [`ConflictRequest`]s of all sessions.
#[derive(Default)]
pub(crate) struct Pending(Mutex<Waiters>);

impl Pending {
    /// Starts waiting for the answer to `id`.
    pub fn register(&self, id: TransferId, session_key: u64) -> oneshot::Receiver<Answer> {
        let (tx, rx) = oneshot::channel();
        lock(&self.0).insert(id, (session_key, tx));
        rx
    }

    /// Delivers the user's answer (`None` = cancel); `false` if nothing waits for `id`.
    pub fn answer(&self, id: TransferId, answer: Answer) -> bool {
        match lock(&self.0).remove(&id) {
            Some((_, tx)) => tx.send(answer).is_ok(),
            None => false,
        }
    }

    /// Stops waiting for `id` (timeout).
    pub fn forget(&self, id: TransferId) {
        lock(&self.0).remove(&id);
    }

    /// The session ended: every prompt of it is cancelled.
    pub fn drop_session(&self, session_key: u64) {
        lock(&self.0).retain(|_, (k, _)| *k != session_key);
    }
}

/// Works out the conflict policy of an incoming offer. Policies other than
/// [`ConflictPolicy::Ask`] apply as they are; `Ask` lists the clashes, emits
/// [`TransferEvent::ConflictsFound`] and waits for [`crate::FileTransfer::resolve_conflicts`]
/// (no clashes: nothing is asked; no answer in time: keep both).
///
/// # Errors
/// [`FilesError::Cancelled`] if the user cancelled or the session ended while asking, and the
/// errors of [`detect_conflicts`].
pub(crate) async fn decide(
    s: &Arc<SessionState>,
    id: TransferId,
    dest: &Path,
    items: &[TransferItem],
    policy: ConflictPolicy,
    initiated_locally: bool,
) -> Result<Decisions, FilesError> {
    if policy != ConflictPolicy::Ask {
        return Ok(Decisions::uniform(policy));
    }
    let (d, it) = (dest.to_path_buf(), items.to_vec());
    let mut found = tokio::task::spawn_blocking(move || detect_conflicts(&d, &it))
        .await
        .map_err(|_| FilesError::Protocol("conflict check failed".into()))??;
    if found.is_empty() {
        return Ok(Decisions::uniform(ConflictPolicy::Rename));
    }
    let total_conflicts = u32::try_from(found.len()).unwrap_or(u32::MAX);
    found.truncate(MAX_LISTED_CONFLICTS);
    let label = items
        .iter()
        .find(|i| !i.rel_path.contains('/'))
        .map(|i| i.rel_path.clone())
        .unwrap_or_default();
    let rx = s.inner.pending.register(id, s.ctx.session_key);
    s.emit(TransferEvent::ConflictsFound(ConflictRequest {
        id,
        session_key: s.ctx.session_key,
        initiated_locally,
        label,
        dest_dir: display_path(dest),
        total_conflicts,
        conflicts: found,
    }));
    let answer = tokio::time::timeout(s.inner.cfg.ask_timeout, rx).await;
    let out = match answer {
        Ok(Ok(Some(resolution))) => Ok(resolution.decisions()),
        Ok(_) => Err(FilesError::Cancelled),
        Err(_) => {
            s.inner.pending.forget(id);
            tracing::info!(
                transfer = id.0,
                "no answer to the conflict prompt; keeping both"
            );
            Ok(Decisions::uniform(ConflictPolicy::Rename))
        }
    };
    s.emit(TransferEvent::ConflictsResolved(id));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(rel: &str, size: u64, ms: i64) -> TransferItem {
        TransferItem {
            rel_path: rel.into(),
            size,
            modified_unix_ms: ms,
            is_dir: false,
        }
    }

    #[test]
    fn detects_only_real_clashes() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("تقرير.txt"), b"old!").unwrap();
        std::fs::create_dir(d.path().join("مجلد")).unwrap();
        std::fs::write(d.path().join("مجلد").join("b.bin"), b"x").unwrap();
        let items = [
            file("تقرير.txt", 10, 1),
            file("جديد.txt", 3, 1),
            file("مجلد/b.bin", 99, 1),
            TransferItem {
                rel_path: "مجلد".into(),
                size: 0,
                modified_unix_ms: 0,
                is_dir: true,
            },
        ];
        let c = detect_conflicts(d.path(), &items).unwrap();
        let names: Vec<_> = c.iter().map(|c| c.rel_path.as_str()).collect();
        assert_eq!(names, ["تقرير.txt", "مجلد/b.bin"]);
        assert_eq!(
            (c[0].index, c[0].existing_size, c[0].incoming_size),
            (0, 4, 10)
        );
        assert_eq!(c[1].index, 2);
        assert_eq!(c[0].partial_bytes, None);
    }

    #[test]
    fn identical_files_are_not_asked_about() {
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
        let same = file("same.bin", 5, 1_700_000_000_000);
        assert!(detect_conflicts(d.path(), &[same]).unwrap().is_empty());
    }

    #[test]
    fn resolution_maps_to_planner_decisions() {
        let r = ConflictResolution {
            default: ConflictChoice::Skip,
            per_file: vec![
                (2, ConflictChoice::Overwrite),
                (3, ConflictChoice::KeepBoth),
            ],
        };
        let d = r.decisions();
        assert_eq!(d.for_item(0), ConflictPolicy::Skip);
        assert_eq!(d.for_item(2), ConflictPolicy::Overwrite);
        assert_eq!(d.for_item(3), ConflictPolicy::Rename);
        assert_eq!(ConflictChoice::Resume.policy(), ConflictPolicy::Overwrite);
    }

    #[test]
    fn answers_reach_the_waiter_once() {
        let p = Pending::default();
        let mut rx = p.register(TransferId(1), 7);
        assert!(p.answer(
            TransferId(1),
            Some(ConflictResolution::all(ConflictChoice::Skip))
        ));
        assert!(!p.answer(TransferId(1), None), "answered already");
        assert!(rx.try_recv().unwrap().is_some());
        let mut rx = p.register(TransferId(2), 7);
        p.drop_session(7);
        assert!(rx.try_recv().is_err(), "a dropped prompt ends the wait");
    }
}
