//! The OS clipboard through `arboard`, with change notifications on Windows.

use std::borrow::Cow;
use std::sync::{Mutex, PoisonError};

use tokio::sync::watch;

use super::error::ClipboardError;
use super::{ClipContent, ClipImage, ClipboardBackend};

/// The system clipboard. Created lazily (opening it can fail, e.g. in a service session);
/// errors are reported per call, never as a panic.
pub struct SystemClipboard {
    board: Mutex<Option<arboard::Clipboard>>,
    changes: Option<watch::Receiver<u64>>,
    #[cfg(windows)]
    _listener: Option<super::windows::Listener>,
    /// Fallback sequence for platforms without one (bumped on each write).
    #[cfg(not(windows))]
    writes: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for SystemClipboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemClipboard")
            .field("notifies", &self.changes.is_some())
            .finish_non_exhaustive()
    }
}

impl Default for SystemClipboard {
    fn default() -> Self {
        Self::new()
    }
}

fn err(e: arboard::Error) -> ClipboardError {
    ClipboardError::Clipboard(e.to_string())
}

fn present<T>(r: Result<T, arboard::Error>) -> Result<Option<T>, ClipboardError> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(arboard::Error::ContentNotAvailable | arboard::Error::ConversionFailure) => Ok(None),
        Err(e) => Err(err(e)),
    }
}

impl SystemClipboard {
    /// Opens the system clipboard and (on Windows) starts the change listener. Never fails:
    /// without a listener, local changes are simply not detected (logged once).
    pub fn new() -> Self {
        #[cfg(windows)]
        {
            let (tx, rx) = watch::channel(super::windows::sequence());
            match super::windows::Listener::start(tx) {
                Ok(l) => Self {
                    board: Mutex::new(None),
                    changes: Some(rx),
                    _listener: Some(l),
                },
                Err(e) => {
                    tracing::warn!(%e, "clipboard change listener unavailable");
                    Self {
                        board: Mutex::new(None),
                        changes: None,
                        _listener: None,
                    }
                }
            }
        }
        #[cfg(not(windows))]
        {
            Self {
                board: Mutex::new(None),
                changes: None,
                writes: std::sync::atomic::AtomicU64::new(0),
            }
        }
    }

    fn with<T>(
        &self,
        f: impl FnOnce(&mut arboard::Clipboard) -> Result<T, ClipboardError>,
    ) -> Result<T, ClipboardError> {
        let mut guard = self.board.lock().unwrap_or_else(PoisonError::into_inner);
        let board = match guard.as_mut() {
            Some(b) => b,
            None => guard.insert(arboard::Clipboard::new().map_err(err)?),
        };
        f(board)
    }

    fn sequence(&self) -> u64 {
        #[cfg(windows)]
        {
            super::windows::sequence()
        }
        #[cfg(not(windows))]
        {
            self.writes
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                .wrapping_add(1)
        }
    }
}

impl ClipboardBackend for SystemClipboard {
    fn read(&self) -> Result<Option<ClipContent>, ClipboardError> {
        self.with(|cb| {
            if let Some(files) = present(cb.get().file_list())? {
                if !files.is_empty() {
                    return Ok(Some(ClipContent::Files(files)));
                }
            }
            let text = present(cb.get_text())?;
            if let Some(html) = present(cb.get().html())? {
                // A copied browser image comes as `<img>` HTML without text: the bitmap is the
                // useful part then (the image URL may not even be reachable offline).
                if text.as_deref().is_some_and(|t| !t.is_empty()) {
                    return Ok(Some(ClipContent::Html {
                        html,
                        alt_text: text.unwrap_or_default(),
                    }));
                }
                if let Some(img) = read_image(cb)? {
                    return Ok(Some(img));
                }
                return Ok(Some(ClipContent::Html {
                    html,
                    alt_text: String::new(),
                }));
            }
            if let Some(t) = text {
                return Ok(Some(ClipContent::Text(t)));
            }
            read_image(cb)
        })
    }

    fn write(&self, content: &ClipContent) -> Result<u64, ClipboardError> {
        self.with(|cb| match content {
            ClipContent::Text(t) => cb.set_text(t.as_str()).map_err(err),
            ClipContent::Html { html, alt_text } => cb
                .set_html(html.as_str(), Some(alt_text.as_str()))
                .map_err(err),
            ClipContent::Image(i) => cb
                .set_image(arboard::ImageData {
                    width: i.width as usize,
                    height: i.height as usize,
                    bytes: Cow::Borrowed(&i.rgba),
                })
                .map_err(err),
            ClipContent::Files(f) => cb.set().file_list(f).map_err(err),
        })?;
        Ok(self.sequence())
    }

    fn changes(&self) -> Option<watch::Receiver<u64>> {
        self.changes.clone()
    }
}

fn read_image(cb: &mut arboard::Clipboard) -> Result<Option<ClipContent>, ClipboardError> {
    let Some(img) = present(cb.get_image())? else {
        return Ok(None);
    };
    let (Ok(width), Ok(height)) = (u32::try_from(img.width), u32::try_from(img.height)) else {
        return Ok(None);
    };
    Ok(Some(ClipContent::Image(ClipImage {
        width,
        height,
        rgba: img.bytes.into_owned(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Touches the user's REAL clipboard (overwrites it). Run manually only:
    /// `cargo test -p tlp-desk-files real_clipboard -- --ignored`.
    #[test]
    #[ignore = "overwrites the user's real clipboard"]
    fn real_clipboard_round_trips_arabic_and_notifies() {
        let cb = SystemClipboard::new();
        let mut rx = cb.changes().expect("listener");
        let text = "مَرْحَبًا\u{200F} لا\u{200C}مي\u{200D} a\r\nb\nc 👨\u{200D}👩\u{200D}👧";
        let seq = cb.write(&ClipContent::Text(text.into())).unwrap();
        assert_eq!(cb.read().unwrap(), Some(ClipContent::Text(text.into())));
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(*rx.borrow_and_update(), seq);
        let html = ClipContent::Html {
            html: "<p dir=\"rtl\">مرحبا</p>".into(),
            alt_text: "مرحبا".into(),
        };
        cb.write(&html).unwrap();
        assert_eq!(cb.read().unwrap(), Some(html));
    }
}
