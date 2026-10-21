//! The remote devices file: `remote_devices.json` in the per-user data folder, with the
//! screen thumbnails as PNG files in `remote_thumbs/<id>.png` next to it.
//!
//! Same rules as the address book file: versioned, every struct `#[serde(default)]`, unknown
//! fields ignored, atomic writes (`.tmp` + rename), an unreadable file is moved aside (never
//! deleted) and one bad entry only drops that entry. A thumbnail that fails to decode is
//! simply not shown.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use super::{RemoteBook, RemoteEntry, Thumb, MAX_ENTRIES};
use crate::backend::DeviceId;

/// Current file schema version.
pub const SCHEMA_VERSION: u32 = 1;
/// File name inside the settings folder.
pub const FILE_NAME: &str = "remote_devices.json";
/// Folder name of the thumbnails, inside the settings folder.
pub const THUMBS_DIR: &str = "remote_thumbs";
/// Larger files are refused.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Larger thumbnails are refused (a 384x216 PNG is well under 100 KB).
const MAX_THUMB_BYTES: u64 = 2 * 1024 * 1024;

/// On-disk layout. Entries stay raw JSON until validated one by one.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct FileRepr {
    version: u32,
    entries: Vec<serde_json::Value>,
}

#[derive(Serialize)]
struct FileOut<'a> {
    version: u32,
    entries: &'a [RemoteEntry],
}

/// What [`Store::load`] found besides the list itself.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoadReport {
    /// The file was unreadable and was moved here.
    pub corrupt_backup: Option<PathBuf>,
    /// Entries skipped because they were invalid.
    pub skipped_entries: usize,
}

/// Reads and writes the remote devices file and the thumbnails.
#[derive(Debug, Clone)]
pub struct Store {
    path: PathBuf,
    thumbs: PathBuf,
}

impl Store {
    /// A store at an explicit file path; thumbnails go to `<dir>/remote_thumbs/`.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let thumbs = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
            .join(THUMBS_DIR);
        Self { path, thumbs }
    }

    /// `remote_devices.json` in the per-user data folder, or `None` without one.
    pub fn default_location() -> Option<Self> {
        Some(Self::at(crate::settings::app_data_dir()?.join(FILE_NAME)))
    }

    /// The file this store reads and writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The thumbnail file of `id`.
    pub fn thumb_path(&self, id: DeviceId) -> PathBuf {
        self.thumbs.join(format!("{:09}.png", id.0))
    }

    /// Load the list. Never fails: a missing file is an empty list, a corrupt one is moved
    /// aside and reported.
    pub fn load(&self) -> (RemoteBook, LoadReport) {
        let mut report = LoadReport::default();
        match read_file(&self.path) {
            Ok(Some((book, skipped))) => {
                report.skipped_entries = skipped;
                (book, report)
            }
            Ok(None) => (RemoteBook::default(), report),
            Err(e) => {
                tracing::warn!(path = %self.path.display(), "remote devices file unreadable: {e:#}");
                let dest = self.path.with_file_name(format!(
                    "remote_devices.corrupt-{}.json",
                    crate::address_book::now()
                ));
                report.corrupt_backup = fs::rename(&self.path, &dest).ok().map(|()| dest);
                (RemoteBook::default(), report)
            }
        }
    }

    /// Write the list atomically.
    ///
    /// # Errors
    ///
    /// Any I/O or serialization error; the existing file is left untouched then.
    pub fn save(&self, book: &RemoteBook) -> anyhow::Result<()> {
        let out = FileOut {
            version: SCHEMA_VERSION,
            entries: book.entries(),
        };
        write_atomic(&self.path, serde_json::to_string_pretty(&out)?.as_bytes())
    }

    /// Write `id`'s thumbnail as PNG (atomically).
    ///
    /// # Errors
    ///
    /// Any I/O or encoding error.
    pub fn save_thumb(&self, id: DeviceId, thumb: &Thumb) -> anyhow::Result<()> {
        write_atomic(&self.thumb_path(id), &encode_png(thumb)?)
    }

    /// Read `id`'s thumbnail, if there is a valid one.
    pub fn load_thumb(&self, id: DeviceId) -> Option<Thumb> {
        let path = self.thumb_path(id);
        let meta = fs::metadata(&path).ok()?;
        if !meta.is_file() || meta.len() > MAX_THUMB_BYTES {
            return None;
        }
        let bytes = fs::read(&path).ok()?;
        match decode_png(&bytes) {
            Ok(t) => Some(t),
            Err(e) => {
                tracing::warn!(path = %path.display(), "thumbnail unreadable: {e:#}");
                None
            }
        }
    }

    /// Delete `id`'s thumbnail (no error if there is none).
    pub fn remove_thumb(&self, id: DeviceId) {
        let path = self.thumb_path(id);
        if let Err(e) = fs::remove_file(&path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(path = %path.display(), "cannot delete the thumbnail: {e}");
            }
        }
    }
}

/// `Ok(None)` when the file does not exist.
fn read_file(path: &Path) -> anyhow::Result<Option<(RemoteBook, usize)>> {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    anyhow::ensure!(
        meta.len() <= MAX_FILE_BYTES,
        "file is too large ({} bytes)",
        meta.len()
    );
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let repr: FileRepr = serde_json::from_str(&text).context("not a remote devices file")?;
    let mut book = RemoteBook::default();
    let mut skipped = 0;
    for raw in repr.entries.into_iter().take(MAX_ENTRIES) {
        match serde_json::from_value::<RemoteEntry>(raw) {
            Ok(e) => book.upsert(e),
            Err(e) => {
                skipped += 1;
                tracing::warn!("skipping an invalid remote device entry: {e}");
            }
        }
    }
    Ok(Some((book, skipped)))
}

/// Write `bytes` to `path` through `<path>.tmp` + fsync + rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f =
            fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// RGBA thumbnail -> PNG bytes (RGB, the alpha is always opaque).
pub fn encode_png(thumb: &Thumb) -> anyhow::Result<Vec<u8>> {
    let (w, h) = (
        u32::try_from(thumb.width).context("width")?,
        u32::try_from(thumb.height).context("height")?,
    );
    anyhow::ensure!(
        thumb.rgba.len() == thumb.width * thumb.height * 4,
        "bad thumbnail size"
    );
    let rgb: Vec<u8> = thumb
        .rgba
        .chunks_exact(4)
        .flat_map(|px| [px[0], px[1], px[2]])
        .collect();
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().context("png header")?;
        writer.write_image_data(&rgb).context("png data")?;
    }
    Ok(out)
}

/// PNG bytes -> RGBA thumbnail (any PNG colour type; sizes above the cap are refused).
pub fn decode_png(bytes: &[u8]) -> anyhow::Result<Thumb> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().context("png header")?;
    let info = reader.info();
    let (w, h) = (info.width as usize, info.height as usize);
    anyhow::ensure!(
        w > 0 && h > 0 && w <= super::thumb::MAX_W && h <= super::thumb::MAX_H,
        "thumbnail is {w}x{h}"
    );
    let mut buf = vec![0u8; reader.output_buffer_size().context("png size")?];
    let frame = reader.next_frame(&mut buf).context("png data")?;
    let data = buf.get(..frame.buffer_size()).context("png data")?;
    let rgba: Vec<u8> = match frame.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => data
            .chunks_exact(3)
            .flat_map(|px| [px[0], px[1], px[2], 0xFF])
            .collect(),
        png::ColorType::Grayscale => data.iter().flat_map(|g| [*g, *g, *g, 0xFF]).collect(),
        png::ColorType::GrayscaleAlpha => data
            .chunks_exact(2)
            .flat_map(|px| [px[0], px[0], px[0], px[1]])
            .collect(),
        other => anyhow::bail!("unsupported png colour type {other:?}"),
    };
    Thumb::from_rgba(w, h, rgba).context("png size and data disagree")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thumb() -> Thumb {
        let (w, h) = (6, 4);
        let mut rgba = Vec::new();
        for i in 0..(w * h) {
            rgba.extend_from_slice(&[(i * 10) as u8, 7, 200 - (i as u8), 255]);
        }
        Thumb::from_rgba(w, h, rgba).unwrap()
    }

    #[test]
    fn png_round_trips() {
        let t = thumb();
        let png = encode_png(&t).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        let back = decode_png(&png).unwrap();
        assert_eq!(back, t);
        assert!(decode_png(b"not a png").is_err());
    }

    #[test]
    fn store_saves_loads_and_keeps_thumbnails_per_device() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().join(FILE_NAME));
        let (book, report) = store.load();
        assert!(book.is_empty() && report == LoadReport::default());

        let mut book = RemoteBook::default();
        book.record_connection(DeviceId(123_456_789), "بيت ليان", 100);
        let id = DeviceId(123_456_789);
        store.save(&book).unwrap();
        store.save_thumb(id, &thumb()).unwrap();
        assert!(store
            .thumb_path(id)
            .ends_with("remote_thumbs/123456789.png"));

        let (back, report) = store.load();
        assert_eq!(back, book);
        assert_eq!(report.skipped_entries, 0);
        assert_eq!(store.load_thumb(id), Some(thumb()));
        assert_eq!(store.load_thumb(DeviceId(1)), None);
        store.remove_thumb(id);
        store.remove_thumb(id);
        assert_eq!(store.load_thumb(id), None);
    }

    #[test]
    fn a_corrupt_file_is_moved_aside_and_bad_entries_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().join(FILE_NAME));
        fs::write(store.path(), "{not json").unwrap();
        let (book, report) = store.load();
        assert!(book.is_empty());
        assert!(report.corrupt_backup.is_some());
        assert!(!store.path().exists());

        fs::write(
            store.path(),
            r#"{"version":1,"entries":[{"device_id":"123456789"},{"device_id":"nope"},{"device_id":"987654321","future_field":1}]}"#,
        )
        .unwrap();
        let (book, report) = store.load();
        assert_eq!(book.len(), 2);
        assert_eq!(report.skipped_entries, 1);
    }
}
