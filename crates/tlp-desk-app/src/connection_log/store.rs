//! The log files: `connections.log` (newest) plus up to [`ROTATED_FILES`] older
//! `connections.N.log`, JSON lines, append-only, rotated by size.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::record::LogEntry;

/// Base file name of the current log.
pub const LOG_FILE: &str = "connections.log";
/// Rotate when the current file would grow past this many bytes.
pub const DEFAULT_MAX_BYTES: u64 = 1024 * 1024;
/// How many rotated files are kept (`connections.1.log` is the newest of them).
pub const ROTATED_FILES: usize = 3;
/// Lines longer than this are skipped when reading (a corrupt or tampered file).
const MAX_LINE_BYTES: usize = 16 * 1024;
/// At most this many entries are read back (bounds memory for a huge hand-made file).
pub const MAX_READ_ENTRIES: usize = 100_000;

/// Append-only, size-rotated connection log in one folder.
#[derive(Debug)]
pub struct LogStore {
    dir: PathBuf,
    max_bytes: u64,
    /// Serializes appends and rotation between threads of this process.
    write_lock: Mutex<()>,
}

impl LogStore {
    /// A store in `dir` rotating at [`DEFAULT_MAX_BYTES`].
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self::with_max_bytes(dir, DEFAULT_MAX_BYTES)
    }

    /// A store with a custom rotation size (tests).
    pub fn with_max_bytes(dir: impl Into<PathBuf>, max_bytes: u64) -> Self {
        Self {
            dir: dir.into(),
            max_bytes: max_bytes.max(1),
            write_lock: Mutex::new(()),
        }
    }

    /// `%APPDATA%\TLP-Desk` (`None` if `APPDATA` is not set).
    pub fn default_location() -> Option<Self> {
        crate::settings::app_data_dir().map(Self::new)
    }

    /// Path of the current (newest) file.
    pub fn path(&self) -> PathBuf {
        self.dir.join(LOG_FILE)
    }

    fn rotated(&self, n: usize) -> PathBuf {
        self.dir.join(format!("connections.{n}.log"))
    }

    /// Append one entry as a JSON line, rotating first if the file would exceed the limit.
    ///
    /// # Errors
    /// The folder or file cannot be written.
    pub fn append(&self, entry: &LogEntry) -> std::io::Result<()> {
        let mut line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
        line.push('\n');
        let _guard = self.write_lock.lock().unwrap_or_else(|p| p.into_inner());
        std::fs::create_dir_all(&self.dir)?;
        let current = std::fs::metadata(self.path()).map_or(0, |m| m.len());
        let line_len = u64::try_from(line.len()).unwrap_or(u64::MAX);
        if current > 0 && current.saturating_add(line_len) > self.max_bytes {
            self.rotate()?;
        }
        if !ends_with_newline(&self.path()) {
            // A crash left half a line: terminate it so this record stays readable.
            line.insert(0, '\n');
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path())?;
        f.write_all(line.as_bytes())?;
        f.flush()
    }

    /// `connections.log` -> `.1`, `.1` -> `.2`, ...; the oldest is deleted.
    fn rotate(&self) -> std::io::Result<()> {
        let oldest = self.rotated(ROTATED_FILES);
        if oldest.exists() {
            std::fs::remove_file(&oldest)?;
        }
        for n in (1..ROTATED_FILES).rev() {
            let from = self.rotated(n);
            if from.exists() {
                std::fs::rename(&from, self.rotated(n + 1))?;
            }
        }
        std::fs::rename(self.path(), self.rotated(1))
    }

    /// Every readable entry, oldest first. Unreadable lines (corruption, a partial last line
    /// after a crash, a tampered file) are skipped and counted in the log.
    pub fn read_all(&self) -> Vec<LogEntry> {
        let mut out = Vec::new();
        let mut skipped = 0usize;
        let files = (1..=ROTATED_FILES)
            .rev()
            .map(|n| self.rotated(n))
            .chain(std::iter::once(self.path()));
        for path in files {
            if out.len() >= MAX_READ_ENTRIES {
                break;
            }
            skipped = skipped.saturating_add(read_file(&path, &mut out));
        }
        if skipped > 0 {
            tracing::warn!(skipped, "connection log has unreadable lines");
        }
        out
    }
}

/// `true` for a missing/empty file or one whose last byte is `\n`.
fn ends_with_newline(path: &Path) -> bool {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let Ok(mut f) = File::open(path) else {
        return true;
    };
    if f.seek(SeekFrom::End(-1)).is_err() {
        return true; // empty file
    }
    let mut last = [0u8; 1];
    f.read_exact(&mut last).map_or(true, |()| last[0] == b'\n')
}

/// Append the entries of one file to `out`; returns how many lines were skipped.
fn read_file(path: &Path, out: &mut Vec<LogEntry>) -> usize {
    let Ok(f) = File::open(path) else {
        return 0;
    };
    let mut skipped = 0usize;
    let mut reader = BufReader::new(f);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        // Bounded read: never buffer more than MAX_LINE_BYTES + 1 of a single line.
        let n = match read_line_capped(&mut reader, &mut buf) {
            Ok(n) => n,
            Err(_) => {
                skipped = skipped.saturating_add(1);
                break;
            }
        };
        if n == 0 {
            break;
        }
        if out.len() >= MAX_READ_ENTRIES {
            break;
        }
        let parsed = (buf.len() <= MAX_LINE_BYTES)
            .then(|| std::str::from_utf8(&buf).ok())
            .flatten()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(serde_json::from_str::<LogEntry>);
        match parsed {
            Some(Ok(e)) => out.push(e.capped()),
            Some(Err(_)) => skipped = skipped.saturating_add(1),
            None if buf.iter().all(u8::is_ascii_whitespace) => {}
            None => skipped = skipped.saturating_add(1),
        }
    }
    skipped
}

/// Read one line into `buf`, keeping at most `MAX_LINE_BYTES + 1` bytes of it (the rest of an
/// overlong line is consumed and dropped). Returns the number of bytes consumed.
fn read_line_capped(reader: &mut impl BufRead, buf: &mut Vec<u8>) -> std::io::Result<usize> {
    let mut consumed = 0usize;
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return Ok(consumed);
        }
        let (part, done) = match chunk.iter().position(|b| *b == b'\n') {
            Some(i) => (&chunk[..=i], true),
            None => (chunk, false),
        };
        let room = (MAX_LINE_BYTES + 1).saturating_sub(buf.len());
        buf.extend_from_slice(&part[..part.len().min(room)]);
        let len = part.len();
        reader.consume(len);
        consumed = consumed.saturating_add(len);
        if done {
            return Ok(consumed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::record::{Direction, LogEnd};
    use super::*;

    fn entry(i: u32, name: &str) -> LogEntry {
        LogEntry {
            started_ms: 1_760_000_000_000 + u64::from(i),
            device_id: i,
            name: name.into(),
            direction: Direction::Incoming,
            end: LogEnd::ClosedHere,
            ..LogEntry::default()
        }
    }

    #[test]
    fn log_writes_and_reads_back_arabic_names() {
        let dir = tempfile::tempdir().unwrap();
        let store = LogStore::new(dir.path().join("TLP-Desk"));
        assert!(store.read_all().is_empty(), "no file yet");
        store.append(&entry(1, "حاسوب المكتب")).unwrap();
        store.append(&entry(2, "Laptop-محمد")).unwrap();
        let back = store.read_all();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].name, "حاسوب المكتب");
        assert_eq!(back[1].name, "Laptop-محمد");
        let raw = std::fs::read_to_string(store.path()).unwrap();
        assert_eq!(raw.lines().count(), 2);
        assert!(raw.contains("حاسوب المكتب"), "UTF-8 on disk, not escaped");
    }

    #[test]
    fn log_rotates_by_size_and_keeps_order() {
        let dir = tempfile::tempdir().unwrap();
        let line_len = serde_json::to_string(&entry(19, "جهاز")).unwrap().len() as u64 + 1;
        // Room for 3 of the longest lines per file.
        let store = LogStore::with_max_bytes(dir.path(), line_len * 3);
        for i in 0..20 {
            store.append(&entry(i, "جهاز")).unwrap();
        }
        assert!(store.path().exists());
        for n in 1..=ROTATED_FILES {
            assert!(
                dir.path().join(format!("connections.{n}.log")).exists(),
                "{n}"
            );
        }
        assert!(!dir
            .path()
            .join(format!("connections.{}.log", ROTATED_FILES + 1))
            .exists());
        let back = store.read_all();
        // Current file + 3 rotated files of 3 lines each; the oldest entries were dropped.
        assert!((10..=12).contains(&back.len()), "{}", back.len());
        let ids: Vec<u32> = back.iter().map(|e| e.device_id).collect();
        assert!(ids.windows(2).all(|w| w[0] < w[1]), "oldest first: {ids:?}");
        assert_eq!(ids.last(), Some(&19));
    }

    #[test]
    fn corrupt_lines_are_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let store = LogStore::new(dir.path());
        store.append(&entry(1, "أ")).unwrap();
        let mut f = OpenOptions::new().append(true).open(store.path()).unwrap();
        f.write_all(b"{ broken json\n\xff\xfe not utf8\n\n")
            .unwrap();
        f.write_all(&vec![b'x'; MAX_LINE_BYTES * 3]).unwrap();
        f.write_all(b"\n").unwrap();
        drop(f);
        store.append(&entry(2, "ب")).unwrap();
        let back = store.read_all();
        assert_eq!(back.iter().map(|e| e.device_id).collect::<Vec<_>>(), [1, 2]);
    }

    #[test]
    fn partial_last_line_after_a_crash_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let store = LogStore::new(dir.path());
        store.append(&entry(1, "ج")).unwrap();
        let mut f = OpenOptions::new().append(true).open(store.path()).unwrap();
        f.write_all(br#"{"started_ms":17600"#).unwrap();
        drop(f);
        assert_eq!(store.read_all().len(), 1);
        // The next record starts on its own line and is not lost.
        store.append(&entry(2, "د")).unwrap();
        let ids: Vec<u32> = store.read_all().iter().map(|e| e.device_id).collect();
        assert_eq!(ids, [1, 2]);
    }
}
