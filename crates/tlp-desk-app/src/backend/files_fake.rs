//! Files lane for the fake backend: the "remote PC" is a sandbox folder in the temp directory
//! (seeded with Arabic and English names) served through the real `fs::execute` with an
//! allow-list, so the browser behaves exactly as against a real host. Transfers are only
//! acknowledged, never performed.

use std::path::PathBuf;
use std::sync::OnceLock;

use tlp_desk_files::fs::{execute, FsConfig};
use tlp_desk_proto::TransferItem;

use super::files::{FilesNotice, FsFailure, FsOp, FsReply, RemoteClipboardFiles, TransferRequest};

/// The simulated remote disk: `%TEMP%\TLP-Desk-fake-remote`, created and seeded on first use.
fn sandbox() -> Option<&'static PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join("TLP-Desk-fake-remote");
        let seed = [
            ("المستندات", None),
            ("المستندات/تقرير الربع الأول.txt", Some("نص تجريبي")),
            ("صور الرحلة", None),
            ("Projects", None),
            ("Projects/notes.md", Some("# notes")),
            ("اقرأني.txt", Some("مرحباً")),
        ];
        for (rel, content) in seed {
            let p = dir.join(rel);
            let ok = match content {
                None => std::fs::create_dir_all(&p),
                Some(text) if !p.exists() => std::fs::write(&p, text),
                Some(_) => Ok(()),
            };
            if let Err(e) = ok {
                tracing::warn!(%e, path = %p.display(), "cannot seed the fake remote folder");
                return None;
            }
        }
        Some(dir)
    })
    .as_ref()
}

/// Runs `op` on the simulated remote PC.
pub fn remote_request(op: &FsOp) -> Result<FsReply, FsFailure> {
    let dir = sandbox().ok_or_else(|| FsFailure::Other("no sandbox".into()))?;
    let cfg = FsConfig {
        allowed_roots: vec![dir.clone()],
    };
    execute(op, &cfg).map_err(FsFailure::Fs)
}

/// Runs `op` on this PC.
pub fn local_request(op: &FsOp) -> Result<FsReply, FsFailure> {
    execute(op, &FsConfig::default()).map_err(FsFailure::Fs)
}

/// What the fake "remote PC" has copied in Explorer.
pub fn remote_clipboard() -> RemoteClipboardFiles {
    let item = |rel: &str, is_dir: bool, size: u64| TransferItem {
        rel_path: rel.into(),
        size,
        modified_unix_ms: 1_700_000_000_000,
        is_dir,
    };
    RemoteClipboardFiles {
        transfer_id: (1 << 63) | 1,
        items: vec![
            item("عرض تقديمي.pptx", false, 2_400_000),
            item("صور", true, 0),
            item("صور/1.jpg", false, 350_000),
        ],
    }
}

/// The acknowledgement a transfer request gets (nothing is copied).
pub fn transfer_notice(req: &TransferRequest) -> FilesNotice {
    match req {
        TransferRequest::Upload { local, .. } => FilesNotice::TransferStarted {
            count: local.len(),
            upload: true,
        },
        TransferRequest::Download { remote, .. } => FilesNotice::TransferStarted {
            count: remote.len(),
            upload: false,
        },
        TransferRequest::PasteRemoteClipboard { .. } => FilesNotice::TransferStarted {
            count: remote_clipboard().top_level_count(),
            upload: false,
        },
        TransferRequest::PasteLocalClipboard { .. } => FilesNotice::NothingToPaste,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_is_the_only_remote_root_and_lists_arabic_names() {
        let Ok(FsReply::Roots(roots)) = remote_request(&FsOp::Roots) else {
            panic!("no roots");
        };
        assert_eq!(roots.len(), 1);
        let Ok(FsReply::Entries(e)) = remote_request(&FsOp::ListDir {
            path: roots[0].path.clone(),
        }) else {
            panic!("no listing");
        };
        assert!(e.iter().any(|x| x.name == "المستندات" && x.is_dir));
        // Outside the sandbox is refused.
        assert!(remote_request(&FsOp::ListDir {
            path: std::env::temp_dir().to_str().unwrap().into(),
        })
        .is_err());
        assert_eq!(remote_clipboard().top_level_count(), 2);
    }
}
