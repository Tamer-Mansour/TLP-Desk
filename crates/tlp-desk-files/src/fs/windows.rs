//! Win32 calls for the browser's starting points: logical drives (type, volume label, size,
//! free space) and the user's known folders. The only `unsafe` code of this crate; every call
//! is wrapped in a safe function.

use std::path::PathBuf;

use windows::core::{GUID, HSTRING};
use windows::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Pictures,
    FOLDERID_Profile, FOLDERID_Videos, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
};

use tlp_desk_proto::FsRootKind;

/// `GetDriveTypeW` results we offer for browsing: removable, fixed, CD/DVD and RAM disks.
/// Network drives (`DRIVE_REMOTE` = 4) are left out: serving them would hand the viewer the
/// host user's network credentials, and UNC paths are refused anyway.
const BROWSABLE_DRIVE_TYPES: [u32; 4] = [2, 3, 5, 6];

/// One local drive.
pub(crate) struct Drive {
    pub root: String,
    pub label: String,
}

/// The browsable local drives, `A:\` to `Z:\`.
pub(crate) fn drives() -> Vec<Drive> {
    // SAFETY: no arguments; returns a bitmask (0 on failure).
    let mask = unsafe { GetLogicalDrives() };
    (0u8..26)
        .filter(|i| mask & (1u32 << i) != 0)
        .filter_map(|i| {
            let root = format!("{}:\\", char::from(b'A' + i));
            let wide = HSTRING::from(root.as_str());
            // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call.
            let kind = unsafe { GetDriveTypeW(&wide) };
            if !BROWSABLE_DRIVE_TYPES.contains(&kind) {
                return None;
            }
            Some(Drive {
                label: volume_label(&wide),
                root,
            })
        })
        .collect()
}

fn volume_label(root: &HSTRING) -> String {
    // MAX_PATH + 1 is the documented maximum for the volume name buffer.
    let mut buf = [0u16; 261];
    // SAFETY: `root` is NUL-terminated and outlives the call; `buf` is a valid writable slice
    // whose length the wrapper passes; the optional out-pointers are all `None`.
    let ok = unsafe { GetVolumeInformationW(root, Some(&mut buf), None, None, None, None) };
    if ok.is_err() {
        // An empty card reader or DVD drive: no label, still listed.
        return String::new();
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(buf.get(..len).unwrap_or_default())
}

/// `(total, free for the caller)` bytes of the volume holding `dir`, if it is mounted.
pub(crate) fn space(dir: &str) -> Option<(u64, u64)> {
    let wide = HSTRING::from(dir);
    let mut free = 0u64;
    let mut total = 0u64;
    // SAFETY: `wide` is NUL-terminated and outlives the call; `free` and `total` are valid,
    // writable u64s on our stack for the duration of the call.
    let ok = unsafe { GetDiskFreeSpaceExW(&wide, Some(&mut free), Some(&mut total), None) };
    ok.ok().map(|()| (total, free))
}

/// The user's known folders that resolve to a path, in display order.
pub(crate) fn known_folders() -> Vec<(FsRootKind, PathBuf)> {
    [
        (FsRootKind::Home, FOLDERID_Profile),
        (FsRootKind::Desktop, FOLDERID_Desktop),
        (FsRootKind::Documents, FOLDERID_Documents),
        (FsRootKind::Downloads, FOLDERID_Downloads),
        (FsRootKind::Pictures, FOLDERID_Pictures),
        (FsRootKind::Videos, FOLDERID_Videos),
        (FsRootKind::Music, FOLDERID_Music),
    ]
    .into_iter()
    .filter_map(|(kind, id)| known_folder(&id).map(|p| (kind, p)))
    .collect()
}

fn known_folder(id: &GUID) -> Option<PathBuf> {
    // SAFETY: `id` points to a valid GUID for the duration of the call; no token (current
    // user). On success the shell allocates the string, which we free below exactly once.
    let pwstr = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }.ok()?;
    // SAFETY: on success `pwstr` is a valid NUL-terminated UTF-16 string.
    let path = unsafe { pwstr.to_string() };
    // SAFETY: `pwstr` was allocated by the shell with CoTaskMemAlloc and is not used after this.
    unsafe { CoTaskMemFree(Some(pwstr.0 as *const _)) };
    path.ok().filter(|p| !p.is_empty()).map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_drive_and_profile_are_found() {
        let drives = drives();
        let sys = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        assert!(
            drives.iter().any(|d| d.root.starts_with(&sys)),
            "system drive missing"
        );
        let (total, free) = space(&format!("{sys}\\")).expect("space of the system drive");
        assert!(total > 0 && free <= total);
        let known = known_folders();
        assert!(known.iter().any(|(k, _)| *k == FsRootKind::Home));
        assert!(known.iter().all(|(_, p)| p.is_absolute()));
    }
}
