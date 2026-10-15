//! Small, safe disk helpers: positional reads/writes (several streams write one file at
//! different offsets), sequential-scan opens, free space and modification times.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Read/write buffer size. Large buffers keep syscalls and per-call overhead negligible at
/// multi-GB/s; 2 MiB measured best on NTFS (1 MiB is within a few percent).
pub const IO_BUF: usize = 2 << 20;

/// `FILE_FLAG_SEQUENTIAL_SCAN`: tells the Windows cache manager to read ahead aggressively
/// and drop pages behind us.
#[cfg(windows)]
const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
/// `FILE_FLAG_BACKUP_SEMANTICS`: needed to open a directory handle (to set its times).
#[cfg(windows)]
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

/// Opens a file for reading with a sequential-access hint.
pub fn open_read(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.custom_flags(FILE_FLAG_SEQUENTIAL_SCAN);
    }
    o.open(path)
}

/// Opens (creating if needed) a partial file for positional writes without truncating it.
pub fn open_write(path: &Path, truncate: bool) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(truncate)
        .open(path)
}

/// Reads exactly `buf.len()` bytes at `offset`.
pub fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        let n = read_at(file, buf, offset)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "file is shorter than announced",
            ));
        }
        let rest = std::mem::take(&mut buf);
        buf = rest.get_mut(n..).unwrap_or_default();
        offset += n as u64;
    }
    Ok(())
}

/// Writes all of `buf` at `offset`.
pub fn write_all_at(file: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        let n = write_at(file, buf, offset)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "disk write stalled",
            ));
        }
        buf = buf.get(n..).unwrap_or_default();
        offset += n as u64;
    }
    Ok(())
}

#[cfg(windows)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buf, offset)
}

#[cfg(windows)]
fn write_at(file: &File, buf: &[u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_write(file, buf, offset)
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, offset)
}

#[cfg(unix)]
fn write_at(file: &File, buf: &[u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::write_at(file, buf, offset)
}

/// Bytes available to the current user on the volume holding `path`.
pub fn available_space(path: &Path) -> io::Result<u64> {
    fs4::available_space(path)
}

/// Milliseconds since the Unix epoch (negative before 1970).
pub fn to_unix_ms(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
        Err(e) => i64::try_from(e.duration().as_millis()).map_or(i64::MIN, |v| -v),
    }
}

/// The inverse of [`to_unix_ms`]; `None` if it does not fit `SystemTime`.
pub fn from_unix_ms(ms: i64) -> Option<SystemTime> {
    let d = Duration::from_millis(ms.unsigned_abs());
    if ms >= 0 {
        UNIX_EPOCH.checked_add(d)
    } else {
        UNIX_EPOCH.checked_sub(d)
    }
}

/// Best effort: sets a directory's modification time (after its files were written).
pub fn set_dir_modified(path: &Path, ms: i64) {
    let Some(t) = from_unix_ms(ms) else { return };
    let mut o = OpenOptions::new();
    o.write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    }
    #[cfg(unix)]
    {
        o.write(false).read(true);
    }
    match o.open(path).and_then(|f| f.set_modified(t)) {
        Ok(()) => {}
        Err(e) => tracing::debug!(%e, "could not restore a directory time"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positional_io_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ملف.part");
        let f = open_write(&p, true).unwrap();
        write_all_at(&f, b"world", 6).unwrap();
        write_all_at(&f, b"hello ", 0).unwrap();
        let mut buf = [0u8; 11];
        read_exact_at(&f, &mut buf, 0).unwrap();
        assert_eq!(&buf, b"hello world");
        let mut over = [0u8; 4];
        assert!(read_exact_at(&f, &mut over, 9).is_err());
        assert!(available_space(dir.path()).unwrap() > 0);
    }

    #[test]
    fn unix_ms_roundtrip() {
        for ms in [0i64, 1, -1, 1_700_000_000_123, -86_400_000] {
            assert_eq!(to_unix_ms(from_unix_ms(ms).unwrap()), ms);
        }
    }
}
