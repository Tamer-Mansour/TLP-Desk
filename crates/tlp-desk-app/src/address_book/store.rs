//! The address book file: `%APPDATA%\TLP-Desk\address_book.json`.
//!
//! * **Versioned** (`"version"`), every struct `#[serde(default)]`, unknown fields ignored, so
//!   older and newer files load. A file from a *newer* version is copied to
//!   `address_book.v<N>.bak` before we first overwrite it, so a downgrade loses nothing.
//! * **Atomic writes**: write `address_book.json.tmp`, flush to disk, keep the previous good
//!   file as `address_book.json.bak`, then rename over the original.
//! * **Corruption recovery**: an unreadable file is moved aside to
//!   `address_book.corrupt-<unix time>.json` (never deleted), and the `.bak` copy is loaded
//!   instead if it is good. One bad entry only drops that entry, not the whole book.
//! * **Hostile input** (hand-edited files, imports): size, entry count and every string are
//!   capped before use.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use super::{entry, AddressBook, Entry};

/// Current file schema version.
pub const SCHEMA_VERSION: u32 = 1;
/// File name inside the settings folder.
pub const FILE_NAME: &str = "address_book.json";
/// Larger files are refused (an address book of thousands of devices is far below this).
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Most entries loaded from one file.
pub const MAX_ENTRIES: usize = 5_000;

/// Settings folder name under `%APPDATA%` (same as `settings.rs`).
const APP_DIR: &str = "TLP-Desk";

/// On-disk layout. Entries stay raw JSON until validated one by one.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct FileRepr {
    /// Missing in files written before versioning (treated as version 0).
    version: u32,
    entries: Vec<serde_json::Value>,
}

/// Borrowing twin of [`FileRepr`] for writing.
#[derive(Serialize)]
struct FileOut<'a> {
    version: u32,
    entries: &'a [Entry],
}

/// What [`Store::load`] found besides the book itself.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoadReport {
    /// The main file was unreadable and was moved here.
    pub corrupt_backup: Option<PathBuf>,
    /// The book was restored from `address_book.json.bak`.
    pub restored_from_backup: bool,
    /// Entries skipped because they were invalid.
    pub skipped_entries: usize,
}

/// Reads and writes the address book file.
#[derive(Debug, Clone)]
pub struct Store {
    path: PathBuf,
}

impl Store {
    /// A store at an explicit path (tests, portable mode).
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// `%APPDATA%\TLP-Desk\address_book.json`, or `None` without `APPDATA`.
    pub fn default_location() -> Option<Self> {
        let base = std::env::var_os("APPDATA").map(PathBuf::from)?;
        Some(Self::at(base.join(APP_DIR).join(FILE_NAME)))
    }

    /// The file this store reads and writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn sibling(&self, name: &str) -> PathBuf {
        let mut s = self.path.as_os_str().to_owned();
        s.push(name);
        PathBuf::from(s)
    }

    fn backup_path(&self) -> PathBuf {
        self.sibling(".bak")
    }

    /// Load the book. Never fails: a missing file is an empty book, a corrupt one is moved
    /// aside (see the module docs) and reported in the [`LoadReport`].
    pub fn load(&self) -> (AddressBook, LoadReport) {
        let mut report = LoadReport::default();
        match read_book(&self.path) {
            Ok(Some((book, skipped, version))) => {
                report.skipped_entries = skipped;
                if version > SCHEMA_VERSION {
                    self.keep_newer_copy(version);
                }
                return (book, report);
            }
            Ok(None) => return (AddressBook::default(), report),
            Err(e) => {
                tracing::warn!(path = %self.path.display(), "address book unreadable: {e:#}");
                report.corrupt_backup = self.move_corrupt_aside();
            }
        }
        match read_book(&self.backup_path()) {
            Ok(Some((book, skipped, _))) => {
                tracing::warn!("address book restored from the backup copy");
                report.restored_from_backup = true;
                report.skipped_entries = skipped;
                (book, report)
            }
            Ok(None) => (AddressBook::default(), report),
            Err(e) => {
                tracing::warn!("address book backup unreadable too: {e:#}");
                (AddressBook::default(), report)
            }
        }
    }

    fn move_corrupt_aside(&self) -> Option<PathBuf> {
        let name = format!("address_book.corrupt-{}.json", entry::now());
        let dest = self.path.with_file_name(name);
        match fs::rename(&self.path, &dest) {
            Ok(()) => Some(dest),
            Err(e) => {
                tracing::warn!("cannot move the corrupt address book aside: {e}");
                None
            }
        }
    }

    fn keep_newer_copy(&self, version: u32) {
        let dest = self
            .path
            .with_file_name(format!("address_book.v{version}.bak"));
        if !dest.exists() {
            if let Err(e) = fs::copy(&self.path, &dest) {
                tracing::warn!("cannot keep a copy of the newer address book: {e}");
            }
        }
    }

    /// Write the book atomically (temp file + rename), keeping the previous file as `.bak`.
    ///
    /// # Errors
    ///
    /// Any I/O or serialization error; the existing file is left untouched in that case.
    pub fn save(&self, book: &AddressBook) -> anyhow::Result<()> {
        write_atomic(&self.path, &to_json(book)?, Some(&self.backup_path()))
    }
}

/// Write `text` to `path` through `<path>.tmp` + fsync + rename, so a crash or full disk
/// never leaves a half-written file. With `backup`, the previous file is copied there first
/// if it still parses as an address book.
fn write_atomic(path: &Path, text: &str, backup: Option<&Path>) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f =
            fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    if let Some(backup) = backup {
        if matches!(read_book(path), Ok(Some(_))) {
            if let Err(e) = fs::copy(path, backup) {
                tracing::warn!("cannot update the address book backup: {e}");
            }
        }
    }
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// The book as pretty JSON in the current schema (also the export format).
///
/// # Errors
///
/// Only if serialization fails, which plain data cannot do.
pub fn to_json(book: &AddressBook) -> anyhow::Result<String> {
    let out = FileOut {
        version: SCHEMA_VERSION,
        entries: book.entries(),
    };
    Ok(serde_json::to_string_pretty(&out)?)
}

/// Parse an address book JSON document (a saved file or an export). Returns the book, how
/// many entries were skipped as invalid, and the file's schema version.
///
/// # Errors
///
/// When the text is not an address book document at all.
pub fn from_json(text: &str) -> anyhow::Result<(AddressBook, usize, u32)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let repr: FileRepr = serde_json::from_str(text).context("not an address book file")?;
    let repr = migrate(repr);
    let mut book = AddressBook::default();
    let mut skipped = 0usize;
    for raw in repr.entries.into_iter().take(MAX_ENTRIES) {
        // An entry without an ID is not a device; serde's defaults would make it ID 0.
        let parsed = raw
            .get("device_id")
            .is_some()
            .then(|| serde_json::from_value::<Entry>(raw))
            .and_then(Result::ok);
        match parsed {
            Some(mut e) => {
                e.sanitize();
                book.upsert(e);
            }
            None => skipped = skipped.saturating_add(1),
        }
    }
    if skipped > 0 {
        tracing::warn!(skipped, "skipped invalid address book entries");
    }
    Ok((book, skipped, repr.version))
}

/// Upgrade an older document in place. Version 0 (no `version` field) has the same entry
/// layout as version 1; future migrations are added here as `if repr.version < N { .. }`.
fn migrate(repr: FileRepr) -> FileRepr {
    if repr.version == 0 {
        tracing::debug!("reading an unversioned (version 0) address book");
    }
    repr
}

/// `Ok(None)` for a missing file.
fn read_book(path: &Path) -> anyhow::Result<Option<(AddressBook, usize, u32)>> {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context("reading file metadata"),
    };
    anyhow::ensure!(
        meta.len() <= MAX_FILE_BYTES,
        "file of {} bytes exceeds the {MAX_FILE_BYTES}-byte limit",
        meta.len()
    );
    let bytes = fs::read(path).context("reading file")?;
    let text = String::from_utf8(bytes).context("file is not UTF-8")?;
    from_json(&text).map(Some)
}

/// Read an exported address book from `path` (same checks as loading).
///
/// # Errors
///
/// When the file is missing, too large, not UTF-8 or not an address book.
pub fn import_file(path: &Path) -> anyhow::Result<AddressBook> {
    read_book(path)?
        .map(|(book, _, _)| book)
        .ok_or_else(|| anyhow::anyhow!("{} does not exist", path.display()))
}

/// Write `book` to `path` as an export (atomic like saving, but no `.bak` next to it).
///
/// # Errors
///
/// Any I/O error.
pub fn export_file(book: &AddressBook, path: &Path) -> anyhow::Result<()> {
    write_atomic(path, &to_json(book)?, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address_book::{MacAddr, MacSource};
    use crate::backend::DeviceId;

    fn sample() -> AddressBook {
        let mut book = AddressBook::default();
        let mut a = Entry::new(DeviceId(482_913_057));
        a.alias = "حاسوب المكتب الرئيسي".into();
        a.name = "OFFICE-PC".into();
        a.group = "المكتب".into();
        a.tags = vec!["عمل".into(), "طابعة".into()];
        a.notes = "في الطابق الثاني\nبجانب النافذة".into();
        a.push_ip("192.168.1.20".parse().unwrap());
        a.mac = Some(MacAddr([0, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e]));
        a.mac_source = MacSource::Manual;
        a.last_seen = Some(1_760_000_000);
        a.last_connected = Some(1_759_000_000);
        a.pinned_fingerprint = Some("3f9a 1c07 e2b4 58d1".into());
        a.favorite = true;
        book.upsert(a);
        let mut b = Entry::new(DeviceId(7));
        b.alias = "Lab-2 مختبر".into();
        book.upsert(b);
        book
    }

    #[test]
    fn round_trip_keeps_arabic_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().join("دفتر").join(FILE_NAME));
        let book = sample();
        store.save(&book).unwrap();
        let (loaded, report) = store.load();
        assert_eq!(report, LoadReport::default());
        assert_eq!(loaded, book);
        let text = fs::read_to_string(store.path()).unwrap();
        assert!(text.contains("حاسوب المكتب الرئيسي"), "{text}");
        assert!(text.contains("\"version\": 1"));
        assert!(text.contains("\"000000007\""));
    }

    #[test]
    fn missing_file_is_an_empty_book() {
        let dir = tempfile::tempdir().unwrap();
        let (book, report) = Store::at(dir.path().join(FILE_NAME)).load();
        assert!(book.entries().is_empty());
        assert_eq!(report, LoadReport::default());
    }

    #[test]
    fn version_zero_and_missing_fields_get_defaults() {
        let (book, skipped, version) =
            from_json(r#"{"entries":[{"device_id":"123456789","alias":"قديم"}]}"#).unwrap();
        assert_eq!((skipped, version), (0, 0));
        let e = book.get(DeviceId(123_456_789)).unwrap();
        assert_eq!(e.alias, "قديم");
        assert!(!e.favorite);
        assert!(e.mac.is_none() && e.last_ips.is_empty() && e.last_seen.is_none());
        // Saving writes the current version.
        assert!(to_json(&book).unwrap().contains("\"version\": 1"));
    }

    #[test]
    fn newer_version_loads_and_is_kept_before_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().join(FILE_NAME));
        fs::write(
            store.path(),
            r#"{"version":9,"future":true,"entries":[{"device_id":"000000042","alias":"جديد","colour":"red"}]}"#,
        )
        .unwrap();
        let (book, _) = store.load();
        assert_eq!(book.get(DeviceId(42)).unwrap().alias, "جديد");
        assert!(dir.path().join("address_book.v9.bak").exists());
    }

    #[test]
    fn invalid_entries_are_skipped_not_fatal() {
        let (book, skipped, _) = from_json(
            r#"{"version":1,"entries":[
                {"device_id":"111111111","alias":"ok"},
                {"alias":"no id"},
                {"device_id":"12"},
                {"device_id":"222222222","mac":"not a mac"},
                42
            ]}"#,
        )
        .unwrap();
        assert_eq!(skipped, 4);
        assert_eq!(book.entries().len(), 1);
    }

    #[test]
    fn corrupt_file_is_moved_aside_and_backup_restored() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().join(FILE_NAME));
        let book = sample();
        store.save(&book).unwrap();
        // Second save creates the .bak of the first good file.
        store.save(&book).unwrap();
        fs::write(store.path(), "{\"version\":1,\"entries\":[{\"device_id\":").unwrap();
        let (loaded, report) = store.load();
        assert_eq!(loaded, book);
        assert!(report.restored_from_backup);
        let moved = report.corrupt_backup.unwrap();
        assert!(moved.exists());
        assert!(!store.path().exists());
        // The corrupt bytes are kept for the user, never deleted.
        assert!(fs::read_to_string(moved).unwrap().contains("device_id"));
    }

    #[test]
    fn corrupt_file_without_backup_gives_empty_book() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().join(FILE_NAME));
        fs::write(store.path(), [0xff, 0xfe, 0x00, 0x41]).unwrap();
        let (book, report) = store.load();
        assert!(book.entries().is_empty());
        assert!(report.corrupt_backup.is_some());
        assert!(!report.restored_from_backup);
    }

    #[test]
    fn oversized_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.json");
        let f = fs::File::create(&path).unwrap();
        f.set_len(MAX_FILE_BYTES + 1).unwrap();
        assert!(import_file(&path).is_err());
    }

    #[test]
    fn export_then_import_round_trips_with_arabic_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("نسخة دفتر العناوين.json");
        let book = sample();
        export_file(&book, &path).unwrap();
        export_file(&book, &path).unwrap();
        assert_eq!(import_file(&path).unwrap(), book);
        // Exports leave nothing else behind in the user's folder.
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        assert!(import_file(&dir.path().join("missing.json")).is_err());
    }

    #[test]
    fn utf8_bom_is_accepted() {
        let (book, _, _) =
            from_json("\u{feff}{\"version\":1,\"entries\":[{\"device_id\":\"000000001\"}]}")
                .unwrap();
        assert_eq!(book.entries().len(), 1);
    }
}
