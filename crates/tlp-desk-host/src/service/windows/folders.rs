//! The console user's folders (`SHGetKnownFolderPath` with the user's token from
//! `WTSQueryUserToken`), COM security for the agent's clipboard objects, file ACLs for the
//! machine-wide settings, and console output for `install-service`. Microsoft Learn only.

use std::io::Write;
use std::path::{Path, PathBuf};

use windows::core::{GUID, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SetNamedSecurityInfoW, SDDL_REVISION_1,
    SE_FILE_OBJECT,
};
use windows::Win32::Security::{
    GetSecurityDescriptorDacl, GetSecurityDescriptorGroup, GetSecurityDescriptorOwner,
    InitializeSecurityDescriptor, SetSecurityDescriptorDacl, SetSecurityDescriptorGroup,
    SetSecurityDescriptorOwner, ACL, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_DESCRIPTOR,
};
use windows::Win32::System::Com::{
    CoInitializeEx, CoInitializeSecurity, CoTaskMemFree, COINIT_MULTITHREADED, EOAC_NONE,
    RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IDENTIFY,
};
use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Downloads, FOLDERID_Profile, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
};

use super::launch::user_token;
use super::os_err;
use crate::service::ConsoleUser;
use crate::HostError;

/// DACL of the machine-wide host settings file: SYSTEM and Administrators write, users read.
pub const SETTINGS_FILE_SDDL: &str = "D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FR;;;BU)";

/// Who may call into the agent's COM objects (the virtual files on the clipboard): SYSTEM,
/// Administrators and the interactive user (Explorer pastes from the user's session).
const AGENT_COM_SDDL: &str = "O:SYG:SYD:(A;;0x3;;;SY)(A;;0x3;;;BA)(A;;0x3;;;IU)";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn known_folder(id: &GUID, token: HANDLE) -> Option<PathBuf> {
    // SAFETY: `id` points to a valid GUID and `token` is a valid user token; on success the
    // returned buffer is ours and freed with CoTaskMemFree below.
    let p = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, Some(token)) }.ok()?;
    // SAFETY: `p` is a NUL-terminated wide string returned by the shell.
    let s = unsafe { p.to_string() }.ok();
    // SAFETY: `p` was allocated by SHGetKnownFolderPath with the COM task allocator.
    unsafe { CoTaskMemFree(Some(p.0 as *const core::ffi::c_void)) };
    s.map(PathBuf::from)
}

/// The folders of the user logged on to `session` (`None`: nobody is logged on, or the
/// folders cannot be resolved). Only callable as LocalSystem.
pub fn console_user(session: u32) -> Option<ConsoleUser> {
    let token = user_token(session).ok()?;
    let h = HANDLE(std::os::windows::io::AsRawHandle::as_raw_handle(&token));
    let profile = known_folder(&FOLDERID_Profile, h)?;
    let desktop = known_folder(&FOLDERID_Desktop, h).unwrap_or_else(|| profile.join("Desktop"));
    let downloads =
        known_folder(&FOLDERID_Downloads, h).unwrap_or_else(|| profile.join("Downloads"));
    Some(ConsoleUser {
        session,
        profile,
        desktop,
        downloads,
    })
}

/// Lets Explorer in the user's session call the agent's clipboard objects: the agent runs as
/// SYSTEM, whose default COM access check refuses ordinary users. Call once, early, before
/// any COM object is handed out. Failure is logged by the caller; paste then may not work.
pub fn allow_interactive_com_calls() -> Result<(), HostError> {
    let sddl = wide(AGENT_COM_SDDL);
    let mut rel = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `sddl` is NUL-terminated; `rel` receives a LocalAlloc'd descriptor that is
    // deliberately never freed (COM may keep referring to it for the process lifetime).
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut rel,
            None,
        )
    }
    .map_err(|e| os_err("ConvertStringSecurityDescriptorToSecurityDescriptorW", &e))?;
    // CoInitializeSecurity wants an absolute descriptor with owner and group: build one that
    // points into the (leaked) self-relative one.
    let abs: &'static mut SECURITY_DESCRIPTOR = Box::leak(Box::default());
    let abs_ptr = PSECURITY_DESCRIPTOR(std::ptr::from_mut(abs).cast());
    let mut present = windows::core::BOOL::default();
    let mut defaulted = windows::core::BOOL::default();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut owner = PSID::default();
    let mut group = PSID::default();
    // SAFETY: `abs_ptr` points to a leaked SECURITY_DESCRIPTOR; `rel` is a valid descriptor
    // and every out pointer is valid. The DACL/SIDs point into `rel`, which lives forever.
    unsafe {
        InitializeSecurityDescriptor(abs_ptr, 1)
            .map_err(|e| os_err("InitializeSecurityDescriptor", &e))?;
        GetSecurityDescriptorDacl(rel, &mut present, &mut dacl, &mut defaulted)
            .map_err(|e| os_err("GetSecurityDescriptorDacl", &e))?;
        SetSecurityDescriptorDacl(abs_ptr, true, Some(dacl), false)
            .map_err(|e| os_err("SetSecurityDescriptorDacl", &e))?;
        GetSecurityDescriptorOwner(rel, &mut owner, &mut defaulted)
            .map_err(|e| os_err("GetSecurityDescriptorOwner", &e))?;
        SetSecurityDescriptorOwner(abs_ptr, Some(owner), false)
            .map_err(|e| os_err("SetSecurityDescriptorOwner", &e))?;
        GetSecurityDescriptorGroup(rel, &mut group, &mut defaulted)
            .map_err(|e| os_err("GetSecurityDescriptorGroup", &e))?;
        SetSecurityDescriptorGroup(abs_ptr, Some(group), false)
            .map_err(|e| os_err("SetSecurityDescriptorGroup", &e))?;
    }
    // SAFETY: plain COM initialization of the calling thread (kept for the process).
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .map_err(|e| os_err("CoInitializeEx", &e))?;
    // SAFETY: `abs_ptr` is a valid absolute descriptor that outlives the process's COM use.
    unsafe {
        CoInitializeSecurity(
            Some(abs_ptr),
            -1,
            None,
            None,
            RPC_C_AUTHN_LEVEL_DEFAULT,
            RPC_C_IMP_LEVEL_IDENTIFY,
            None,
            EOAC_NONE,
            None,
        )
    }
    .map_err(|e| os_err("CoInitializeSecurity", &e))
}

/// Replaces the DACL of `path` with the one in `sddl` (e.g. [`SETTINGS_FILE_SDDL`]).
pub fn set_file_dacl(path: &Path, sddl: &str) -> Result<(), HostError> {
    let s = wide(sddl);
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `s` is NUL-terminated; `sd` receives a LocalAlloc'd descriptor freed below.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(s.as_ptr()),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
    }
    .map_err(|e| os_err("ConvertStringSecurityDescriptorToSecurityDescriptorW", &e))?;
    let mut present = windows::core::BOOL::default();
    let mut defaulted = windows::core::BOOL::default();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    // SAFETY: `sd` is valid; out pointers are valid.
    let got = unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) };
    let name = wide(&path.to_string_lossy());
    let result = got
        .map_err(|e| os_err("GetSecurityDescriptorDacl", &e))
        .and_then(|()| {
            // SAFETY: `name` is NUL-terminated; `dacl` points into `sd`, alive until freed.
            let err = unsafe {
                SetNamedSecurityInfoW(
                    PCWSTR(name.as_ptr()),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    None,
                    None,
                    Some(dacl),
                    None,
                )
            };
            if err.is_ok() {
                Ok(())
            } else {
                Err(HostError::Os {
                    call: "SetNamedSecurityInfoW",
                    code: i32::try_from(err.0).unwrap_or(-1),
                })
            }
        });
    // SAFETY: `sd` came from ConvertStringSecurityDescriptorToSecurityDescriptorW.
    unsafe {
        let _ = windows::Win32::Foundation::LocalFree(Some(HLOCAL(sd.0)));
    }
    result
}

/// Writes `text` to the console of the process that started us (`TLP-Desk.exe
/// install-service` typed in a terminal); does nothing when there is none.
pub fn print_to_parent_console(text: &str) {
    // SAFETY: plain call; fails harmlessly when there is no parent console or we already
    // have one.
    let attached = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok();
    if !attached {
        return;
    }
    if let Ok(mut out) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
        let _ = writeln!(out);
        let _ = writeln!(out, "{text}");
    }
}
