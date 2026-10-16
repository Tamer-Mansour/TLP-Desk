//! Starting points for browsing ([`FsOp::Roots`](tlp_desk_proto::FsOp::Roots)): the local
//! drives and the user's known folders, or only the host's allowed folders when an allow-list
//! is configured.

use std::path::Path;

use tlp_desk_proto::{FsRoot, FsRootKind};

use super::FsConfig;

/// The roots a viewer may start from under `cfg`.
pub fn list(cfg: &FsConfig) -> Vec<FsRoot> {
    if !cfg.allowed_roots.is_empty() {
        return cfg
            .allowed_roots
            .iter()
            .filter_map(|p| {
                let path = p.to_str()?.to_owned();
                let label = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_owned();
                Some(root(path, label, FsRootKind::Other))
            })
            .collect();
    }
    let mut out = drives();
    for (kind, p) in known_folders() {
        if p.is_dir() {
            if let Some(path) = p.to_str() {
                out.push(root(path.to_owned(), String::new(), kind));
            }
        }
    }
    out
}

fn root(path: String, label: String, kind: FsRootKind) -> FsRoot {
    let (total_bytes, free_bytes) = match space(&path) {
        Some((t, f)) => (Some(t), Some(f)),
        None => (None, None),
    };
    FsRoot {
        path,
        label,
        kind,
        total_bytes,
        free_bytes,
    }
}

#[cfg(windows)]
fn drives() -> Vec<FsRoot> {
    super::windows::drives()
        .into_iter()
        .map(|d| root(d.root, d.label, FsRootKind::Drive))
        .collect()
}

#[cfg(windows)]
fn known_folders() -> Vec<(FsRootKind, std::path::PathBuf)> {
    super::windows::known_folders()
}

#[cfg(windows)]
fn space(path: &str) -> Option<(u64, u64)> {
    super::windows::space(path)
}

#[cfg(not(windows))]
fn drives() -> Vec<FsRoot> {
    vec![root("/".into(), String::new(), FsRootKind::Drive)]
}

#[cfg(not(windows))]
fn known_folders() -> Vec<(FsRootKind, std::path::PathBuf)> {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return Vec::new();
    };
    let mut v = vec![(FsRootKind::Home, home.clone())];
    for (kind, name) in [
        (FsRootKind::Desktop, "Desktop"),
        (FsRootKind::Documents, "Documents"),
        (FsRootKind::Downloads, "Downloads"),
        (FsRootKind::Pictures, "Pictures"),
        (FsRootKind::Videos, "Videos"),
        (FsRootKind::Music, "Music"),
    ] {
        v.push((kind, home.join(name)));
    }
    v
}

#[cfg(not(windows))]
fn space(_path: &str) -> Option<(u64, u64)> {
    None
}

/// The local folder for a known-folder kind (e.g. where "paste remote files" lands by default:
/// [`FsRootKind::Downloads`]). `None` if this device has no such folder.
pub fn known_folder(kind: FsRootKind) -> Option<std::path::PathBuf> {
    known_folders()
        .into_iter()
        .find(|(k, p)| *k == kind && Path::new(p).is_dir())
        .map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roots_have_a_drive_and_existing_folders() {
        let roots = list(&FsConfig::default());
        assert!(roots.iter().any(|r| r.kind == FsRootKind::Drive));
        for r in roots.iter().filter(|r| r.kind != FsRootKind::Drive) {
            assert!(Path::new(&r.path).is_dir(), "{}", r.path);
        }
        // Every root path is accepted by the peer-path validator.
        for r in &roots {
            assert!(super::super::path::parse(&r.path).is_ok(), "{}", r.path);
        }
    }

    #[test]
    fn allow_list_replaces_drives_and_keeps_arabic_labels() {
        let tmp = tempfile::tempdir().unwrap();
        let shared = tmp.path().join("مشاركة");
        std::fs::create_dir(&shared).unwrap();
        let cfg = FsConfig {
            allowed_roots: vec![shared.clone()],
        };
        let roots = list(&cfg);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].label, "مشاركة");
        assert_eq!(roots[0].kind, FsRootKind::Other);
        assert_eq!(roots[0].path, shared.to_str().unwrap());
    }
}
