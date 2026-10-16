//! The list of recent transfers, kept in `%APPDATA%\TLP-Desk\transfers.json`.
//!
//! One [`TransferRecord`] per finished transfer (this device's own and those the other side
//! started), newest last, at most [`MAX_RECORDS`]. A record keeps what was copied so the queue
//! window can offer "send again". File names and device names are stored, never file contents.
//! A missing, unreadable or damaged file is an empty history (it is never an error for the
//! transfer that is finishing), and writes go through a temporary file so a crash cannot leave
//! half a file behind.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tlp_desk_files::{TransferOrigin, TransferRole, TransferSnapshot, TransferState};

/// File name inside the app data folder.
pub const FILE_NAME: &str = "transfers.json";
/// Most records kept; the oldest are dropped first.
pub const MAX_RECORDS: usize = 100;
/// Most source paths kept per record (a paste of thousands of files stays small).
pub const MAX_PATHS: usize = 200;

/// How a recorded transfer ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Every file arrived.
    Completed,
    /// Stopped by either side.
    Cancelled,
    /// Failed as a whole.
    Failed,
    /// The session ended first.
    Lost,
}

/// One finished transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransferRecord {
    /// When it ended (Unix milliseconds).
    pub finished_unix_ms: u64,
    /// `true` if this device sent the files, `false` if it received them.
    pub sent: bool,
    /// `true` if this device started it (as the viewer).
    pub local: bool,
    /// Name of the file or folder, or the first of several.
    pub label: String,
    /// Number of top-level items.
    pub items: u32,
    /// Name the other device announced.
    pub peer_name: String,
    /// The other device's 9-digit ID (0 if unknown).
    pub peer_id: u32,
    /// Sum of the file sizes.
    pub total_bytes: u64,
    /// Number of files.
    pub total_files: u32,
    /// Files that failed.
    pub failed_files: u32,
    /// How it ended.
    pub outcome: Outcome,
    /// Sent from this device: the local files and folders (for "send again").
    pub local_paths: Vec<String>,
    /// Sent from this device: the folder on the other device (empty = its default).
    pub remote_dest: String,
    /// Fetched from the other device: the remote files and folders (for "get again").
    pub remote_paths: Vec<String>,
    /// Received on this device: the folder the files went to.
    pub local_dest: Option<String>,
}

impl Default for TransferRecord {
    fn default() -> Self {
        Self {
            finished_unix_ms: 0,
            sent: true,
            local: true,
            label: String::new(),
            items: 0,
            peer_name: String::new(),
            peer_id: 0,
            total_bytes: 0,
            total_files: 0,
            failed_files: 0,
            outcome: Outcome::Completed,
            local_paths: Vec::new(),
            remote_dest: String::new(),
            remote_paths: Vec::new(),
            local_dest: None,
        }
    }
}

impl TransferRecord {
    /// Whether "send again" is possible: this device started it, so the sources are known.
    pub fn can_repeat(&self) -> bool {
        self.local
            && if self.sent {
                !self.local_paths.is_empty()
            } else {
                !self.remote_paths.is_empty() && self.local_dest.is_some()
            }
    }
}

/// The record for a transfer that just stopped, or `None` for a pause (it will finish again
/// when resumed, and is recorded then).
pub fn history_record(
    snap: &TransferSnapshot,
    origin: Option<TransferOrigin>,
    peer_name: String,
    peer_id: u32,
    now_unix_ms: u64,
) -> Option<TransferRecord> {
    let outcome = match &snap.state {
        TransferState::Completed => Outcome::Completed,
        TransferState::Cancelled => Outcome::Cancelled,
        TransferState::Failed(_) => Outcome::Failed,
        TransferState::Disconnected => Outcome::Lost,
        TransferState::Preparing | TransferState::Running | TransferState::Paused => return None,
    };
    let origin = origin.unwrap_or(TransferOrigin {
        upload: false,
        local_paths: Vec::new(),
        remote_dest: String::new(),
        remote_paths: Vec::new(),
        local_dest: None,
    });
    let text = |p: &Path| p.to_string_lossy().into_owned();
    Some(TransferRecord {
        finished_unix_ms: now_unix_ms,
        sent: snap.info.role == TransferRole::Sending,
        local: snap.info.initiated_locally,
        label: snap.info.label.clone(),
        items: snap.info.top_level_items,
        peer_name,
        peer_id,
        total_bytes: snap.info.total_bytes,
        total_files: snap.info.total_files,
        failed_files: snap.failed_items,
        outcome,
        local_paths: origin.local_paths.iter().map(|p| text(p)).collect(),
        remote_dest: origin.remote_dest,
        remote_paths: origin.remote_paths,
        local_dest: snap
            .local_dest
            .as_deref()
            .or(origin.local_dest.as_deref())
            .map(text),
    })
}

/// Now, as Unix milliseconds.
pub fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct FileData {
    version: u32,
    records: Vec<TransferRecord>,
}

/// The persisted list.
#[derive(Debug, Default)]
pub struct HistoryStore {
    path: Option<PathBuf>,
    records: Vec<TransferRecord>,
}

impl HistoryStore {
    /// A history that is only kept in memory (tests, `--fake`).
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Loads `dir/transfers.json`; a missing or damaged file gives an empty history that is
    /// still saved there from now on.
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(FILE_NAME);
        let records = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<FileData>(&text) {
                Ok(mut data) => {
                    let extra = data.records.len().saturating_sub(MAX_RECORDS);
                    data.records.drain(..extra);
                    data.records
                }
                Err(e) => {
                    tracing::warn!(%e, "transfer history is damaged; starting empty");
                    Vec::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                tracing::warn!(%e, "transfer history cannot be read; starting empty");
                Vec::new()
            }
        };
        Self {
            path: Some(path),
            records,
        }
    }

    /// The records, oldest first.
    pub fn records(&self) -> &[TransferRecord] {
        &self.records
    }

    /// Adds a finished transfer and saves.
    pub fn push(&mut self, mut record: TransferRecord) {
        record.local_paths.truncate(MAX_PATHS);
        record.remote_paths.truncate(MAX_PATHS);
        self.records.push(record);
        let extra = self.records.len().saturating_sub(MAX_RECORDS);
        self.records.drain(..extra);
        self.save();
    }

    /// Forgets everything and saves.
    pub fn clear(&mut self) {
        self.records.clear();
        self.save();
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let data = FileData {
            version: 1,
            records: self.records.clone(),
        };
        let result = serde_json::to_vec_pretty(&data)
            .map_err(std::io::Error::other)
            .and_then(|bytes| write_atomic(path, &bytes));
        if let Err(e) = result {
            tracing::warn!(%e, "transfer history not saved");
        }
    }
}

/// Writes `bytes` to a sibling temporary file, then renames it over `path`.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(n: u64, label: &str) -> TransferRecord {
        TransferRecord {
            finished_unix_ms: n,
            label: label.into(),
            peer_name: "حاسوب المكتب".into(),
            local_paths: vec![format!("C:\\مستندات\\{label}")],
            ..TransferRecord::default()
        }
    }

    #[test]
    fn records_survive_a_restart_with_arabic_names() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = HistoryStore::load(dir.path());
        assert!(h.records().is_empty());
        h.push(rec(1, "تقرير نهائي.pdf"));
        h.push(TransferRecord {
            sent: false,
            outcome: Outcome::Failed,
            remote_paths: vec!["D:\\صور".into()],
            local_dest: Some("C:\\Users\\سارة\\Downloads".into()),
            ..rec(2, "صور")
        });
        let again = HistoryStore::load(dir.path());
        assert_eq!(again.records(), h.records());
        assert_eq!(again.records()[0].label, "تقرير نهائي.pdf");
        assert!(again.records()[0].can_repeat());
        assert!(again.records()[1].can_repeat());
        assert!(!dir.path().join("transfers.json.tmp").exists());
    }

    #[test]
    fn only_the_newest_hundred_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = HistoryStore::load(dir.path());
        for n in 0..(MAX_RECORDS as u64 + 15) {
            h.push(rec(n, "f"));
        }
        assert_eq!(h.records().len(), MAX_RECORDS);
        assert_eq!(h.records()[0].finished_unix_ms, 15);
        let again = HistoryStore::load(dir.path());
        assert_eq!(again.records().len(), MAX_RECORDS);
    }

    #[test]
    fn damaged_or_foreign_files_load_as_empty_and_get_replaced() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE_NAME), b"{ not json").unwrap();
        let mut h = HistoryStore::load(dir.path());
        assert!(h.records().is_empty());
        h.push(rec(1, "a"));
        assert_eq!(HistoryStore::load(dir.path()).records().len(), 1);
        // Unknown and missing keys are fine (older and newer builds share the file).
        std::fs::write(
            dir.path().join(FILE_NAME),
            "{\"version\":9,\"future\":1,\"records\":[{\"label\":\"x-label\",\"sent\":false}]}"
                .as_bytes(),
        )
        .unwrap();
        let h = HistoryStore::load(dir.path());
        assert_eq!(h.records()[0].label, "x-label");
        assert!(!h.records()[0].sent);
    }

    #[test]
    fn clear_empties_the_file_and_in_memory_never_touches_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = HistoryStore::load(dir.path());
        h.push(rec(1, "a"));
        h.clear();
        assert!(HistoryStore::load(dir.path()).records().is_empty());
        let mut mem = HistoryStore::in_memory();
        mem.push(rec(1, "a"));
        assert_eq!(mem.records().len(), 1);
    }

    fn snapshot(state: TransferState, role: TransferRole, local: bool) -> TransferSnapshot {
        use tlp_desk_files::{TransferId, TransferInfo};
        TransferSnapshot {
            info: TransferInfo {
                id: TransferId(3),
                session_key: 1,
                role,
                direction: tlp_desk_proto::TransferDirection::Upload,
                initiated_locally: local,
                label: "مشروع".into(),
                top_level_items: 2,
                source_dir: String::new(),
                dest_dir: String::new(),
                total_bytes: 4096,
                total_files: 7,
                total_dirs: 1,
                started_unix_ms: 5,
            },
            state,
            bytes_done: 4096,
            resumed_bytes: 0,
            files_done: 7,
            failed_items: 1,
            bytes_per_sec: 0,
            local_dest: None,
        }
    }

    #[test]
    fn finished_transfers_become_records_but_pauses_do_not() {
        let origin = TransferOrigin {
            upload: true,
            local_paths: vec![PathBuf::from("C:\\عمل\\مشروع")],
            remote_dest: "D:\\وارد".into(),
            remote_paths: Vec::new(),
            local_dest: None,
        };
        let done = snapshot(TransferState::Completed, TransferRole::Sending, true);
        let r = history_record(&done, Some(origin), "حاسوب سارة".into(), 482_913_057, 99).unwrap();
        assert_eq!(
            (r.sent, r.local, r.outcome, r.failed_files),
            (true, true, Outcome::Completed, 1)
        );
        assert_eq!(r.local_paths, ["C:\\عمل\\مشروع"]);
        assert_eq!(r.remote_dest, "D:\\وارد");
        assert!(r.can_repeat());

        let mut incoming = snapshot(TransferState::Cancelled, TransferRole::Receiving, false);
        incoming.local_dest = Some(PathBuf::from("C:\\Users\\Public\\Desktop"));
        let r = history_record(&incoming, None, "x".into(), 0, 100).unwrap();
        assert_eq!(
            (r.sent, r.local, r.outcome),
            (false, false, Outcome::Cancelled)
        );
        assert_eq!(r.local_dest.as_deref(), Some("C:\\Users\\Public\\Desktop"));
        assert!(!r.can_repeat());

        for s in [
            TransferState::Paused,
            TransferState::Running,
            TransferState::Preparing,
        ] {
            assert!(history_record(
                &snapshot(s, TransferRole::Sending, true),
                None,
                "".into(),
                0,
                1
            )
            .is_none());
        }
        let lost = snapshot(TransferState::Disconnected, TransferRole::Sending, true);
        assert_eq!(
            history_record(&lost, None, "".into(), 0, 1)
                .unwrap()
                .outcome,
            Outcome::Lost
        );
    }

    #[test]
    fn repeat_needs_the_sources_and_a_local_start() {
        let mut r = rec(1, "a");
        assert!(r.can_repeat());
        r.local = false;
        assert!(!r.can_repeat(), "the other side started it");
        let mut g = rec(1, "a");
        g.sent = false;
        assert!(!g.can_repeat(), "no remote paths");
    }
}
