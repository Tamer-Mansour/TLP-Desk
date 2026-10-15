//! Where recordings live and what they are called, plus listing and deleting them.
//!
//! Names are `TLP-Desk <peer> <YYYY-MM-DD HH-MM-SS>.mp4` (local time, Western digits, no `:`
//! because Windows forbids it), with ` (2)`, ` (3)`, ... appended when the name is taken or
//! when a recording continues in a new file after the remote screen size changed. The peer
//! name is kept exactly as announced (Arabic included) apart from characters Windows does not
//! allow in file names and invisible bidi controls, which could make a name display as
//! something else in Explorer.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::demux::probe;
use crate::ViewerError;

/// Prefix of every recording file name.
pub const FILE_PREFIX: &str = "TLP-Desk";
/// Extension of recording files.
pub const FILE_EXTENSION: &str = "mp4";
/// Longest peer name (in characters) kept in a file name.
const MAX_PEER_CHARS: usize = 64;
/// Most entries [`list_recordings`] returns (newest first).
const MAX_LISTED: usize = 1_000;

/// `%USERPROFILE%\Videos\TLP-Desk` (or `$HOME/Videos/TLP-Desk` elsewhere). `None` when the
/// profile folder is unknown.
pub fn default_folder() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join("Videos").join(FILE_PREFIX))
}

fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// Makes a peer name safe for a Windows file name while keeping its letters (any script).
pub(crate) fn sanitize_peer(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                ' '
            } else {
                c
            }
        })
        .filter(|c| !is_bidi_control(*c))
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let capped: String = collapsed.chars().take(MAX_PEER_CHARS).collect();
    let trimmed = capped.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if trimmed.is_empty() {
        "remote".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Local wall-clock time split into fields (falls back to UTC when the offset is unknown).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stamp {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl Stamp {
    pub fn now() -> Self {
        let t =
            time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
        Self {
            year: t.year(),
            month: u8::from(t.month()),
            day: t.day(),
            hour: t.hour(),
            minute: t.minute(),
            second: t.second(),
        }
    }
}

/// `TLP-Desk <peer> <date time>` without the extension.
pub(crate) fn base_name(peer: &str, at: Stamp) -> String {
    format!(
        "{FILE_PREFIX} {} {:04}-{:02}-{:02} {:02}-{:02}-{:02}",
        sanitize_peer(peer),
        at.year,
        at.month,
        at.day,
        at.hour,
        at.minute,
        at.second
    )
}

/// The first of `base.mp4`, `base (2).mp4`, ... that does not exist yet.
pub(crate) fn next_free_path(folder: &Path, base: &str) -> PathBuf {
    let mut path = folder.join(format!("{base}.{FILE_EXTENSION}"));
    let mut n = 2u32;
    while path.exists() && n < 10_000 {
        path = folder.join(format!("{base} ({n}).{FILE_EXTENSION}"));
        n += 1;
    }
    path
}

fn has_recording_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(FILE_EXTENSION))
}

/// One file in the recordings folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingEntry {
    pub path: PathBuf,
    /// File name as stored (exact Unicode, Arabic included).
    pub file_name: String,
    pub bytes: u64,
    pub modified: Option<SystemTime>,
    /// Length of the video, when the file was closed properly (or repaired).
    pub duration: Option<Duration>,
    /// Closed properly. `false` while it is being recorded, or after a crash (it still plays up
    /// to the last complete second; [`recover_file`](super::recover_file) finishes it).
    pub complete: bool,
}

/// Lists the `.mp4` files in `folder`, newest first. A missing folder is an empty list.
pub fn list_recordings(folder: &Path) -> Result<Vec<RecordingEntry>, ViewerError> {
    let dir = match std::fs::read_dir(folder) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(ViewerError::RecordingIo(e)),
    };
    let mut out = Vec::new();
    for entry in dir.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() || !has_recording_extension(&path) {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            continue; // not valid Unicode: not one of ours
        };
        let info = File::open(&path).ok().and_then(|mut f| probe(&mut f).ok());
        out.push(RecordingEntry {
            file_name,
            bytes: meta.len(),
            modified: meta.modified().ok(),
            duration: info.and_then(|i| i.duration),
            complete: info.is_some_and(|i| i.complete),
            path,
        });
    }
    out.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then(b.file_name.cmp(&a.file_name))
    });
    out.truncate(MAX_LISTED);
    Ok(out)
}

/// Deletes one recording. Refuses anything that is not an `.mp4` file directly inside
/// `folder`, so a stale or crafted path can never delete something else.
pub fn delete_recording(folder: &Path, path: &Path) -> Result<(), ViewerError> {
    let folder = folder.canonicalize().map_err(ViewerError::RecordingIo)?;
    let file = path.canonicalize().map_err(ViewerError::RecordingIo)?;
    if file.parent() != Some(folder.as_path()) || !has_recording_extension(&file) || !file.is_file()
    {
        return Err(ViewerError::BadRecording(
            "not a recording in the recordings folder",
        ));
    }
    std::fs::remove_file(&file).map_err(ViewerError::RecordingIo)
}

/// Whether the last bytes of the file are an `mfra` index (the file was closed properly).
pub(crate) fn ends_with_index<R: Read + Seek>(r: &mut R, len: u64) -> bool {
    let mut check = || -> std::io::Result<bool> {
        if len < 16 {
            return Ok(false);
        }
        r.seek(SeekFrom::Start(len - 4))?;
        let mut size = [0u8; 4];
        r.read_exact(&mut size)?;
        let size = u64::from(u32::from_be_bytes(size));
        if size < 16 || size > len {
            return Ok(false);
        }
        r.seek(SeekFrom::Start(len - size + 4))?;
        let mut kind = [0u8; 4];
        r.read_exact(&mut kind)?;
        Ok(&kind == b"mfra")
    };
    check().unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAMP: Stamp = Stamp {
        year: 2026,
        month: 3,
        day: 7,
        hour: 9,
        minute: 5,
        second: 1,
    };

    #[test]
    fn arabic_names_are_kept_exactly() {
        assert_eq!(
            base_name("مكتب أحمد", STAMP),
            "TLP-Desk مكتب أحمد 2026-03-07 09-05-01"
        );
        assert_eq!(
            base_name("PC-٣ مكتب 2", STAMP),
            "TLP-Desk PC-٣ مكتب 2 2026-03-07 09-05-01"
        );
    }

    #[test]
    fn unsafe_characters_and_bidi_controls_are_removed() {
        assert_eq!(
            sanitize_peer("a/b\\c:d*e?f\"g<h>i|j"),
            "a b c d e f g h i j"
        );
        assert_eq!(sanitize_peer("\u{202E}gnp.exe"), "gnp.exe");
        assert_eq!(
            sanitize_peer("حاسوب\u{2067}\u{2069}\n\tالمكتب"),
            "حاسوب المكتب"
        );
        assert_eq!(sanitize_peer(" ..  "), "remote");
        assert_eq!(sanitize_peer("name."), "name");
        assert_eq!(
            sanitize_peer(&"ع".repeat(200)).chars().count(),
            MAX_PEER_CHARS
        );
    }

    #[test]
    fn next_free_path_counts_up() {
        let dir = tempfile::tempdir().unwrap();
        let base = base_name("جهاز", STAMP);
        let first = next_free_path(dir.path(), &base);
        assert_eq!(
            first.file_name().unwrap().to_str().unwrap(),
            format!("{base}.mp4")
        );
        std::fs::write(&first, b"x").unwrap();
        let second = next_free_path(dir.path(), &base);
        assert_eq!(
            second.file_name().unwrap().to_str().unwrap(),
            format!("{base} (2).mp4")
        );
    }

    #[test]
    fn delete_only_inside_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let inside = dir.path().join("TLP-Desk مكتب.mp4");
        let outside = other.path().join("x.mp4");
        let wrong_ext = dir.path().join("notes.txt");
        for p in [&inside, &outside, &wrong_ext] {
            std::fs::write(p, b"data").unwrap();
        }
        assert!(delete_recording(dir.path(), &outside).is_err());
        assert!(delete_recording(dir.path(), &wrong_ext).is_err());
        let sneaky = dir
            .path()
            .join("..")
            .join(other.path().file_name().unwrap())
            .join("x.mp4");
        assert!(delete_recording(dir.path(), &sneaky).is_err());
        assert!(outside.exists() && wrong_ext.exists());
        delete_recording(dir.path(), &inside).unwrap();
        assert!(!inside.exists());
    }

    #[test]
    fn listing_a_missing_folder_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list_recordings(&dir.path().join("none"))
            .unwrap()
            .is_empty());
    }
}
