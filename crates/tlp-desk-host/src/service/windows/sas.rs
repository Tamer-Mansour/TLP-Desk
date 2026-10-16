//! Ctrl+Alt+Del (`SendSAS` in sas.dll), `LockWorkStation`, and the `SoftwareSASGeneration`
//! policy that allows services to send the secure attention sequence.
//!
//! Microsoft Learn ("SendSAS function"): a service calls `SendSAS(FALSE)`; it only works if the
//! policy `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System\SoftwareSASGeneration`
//! allows services (1 = services, 2 = Ease of Access applications, 3 = both). The installer
//! sets it through `install-service`; a domain Group Policy can override it.

use windows::core::{s, w};
use windows::Win32::Foundation::{FreeLibrary, ERROR_SUCCESS};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegGetValueW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE,
    KEY_QUERY_VALUE, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, RRF_RT_REG_DWORD,
};
use windows::Win32::System::Shutdown::LockWorkStation;

use super::os_err;
use crate::HostError;

/// Registry key of the policy (under `HKEY_LOCAL_MACHINE`).
pub const SAS_POLICY_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System";
/// Value name of the policy.
pub const SAS_POLICY_VALUE: &str = "SoftwareSASGeneration";

/// The value to write so that services may send SAS, keeping whatever else was allowed;
/// `None` when nothing needs to change.
pub fn sas_policy_value(existing: Option<u32>) -> Option<u32> {
    match existing {
        Some(1) | Some(3) => None,
        // Ease of Access was allowed: allow services too.
        Some(2) => Some(3),
        // Missing, 0 (nobody) or an unknown value: services only.
        _ => Some(1),
    }
}

type SendSasFn = unsafe extern "system" fn(as_user: windows::core::BOOL);

/// Sends the secure attention sequence (as if Ctrl+Alt+Del was pressed). Must run in the
/// service (LocalSystem); fails silently on the OS side if the policy forbids it.
pub fn send_sas() -> Result<(), HostError> {
    // SAFETY: loading a system DLL by name from the system directory search path.
    let lib = unsafe { LoadLibraryW(w!("sas.dll")) }.map_err(|e| os_err("LoadLibraryW", &e))?;
    // SAFETY: `lib` is a valid module handle; the name is a static NUL-terminated string.
    let proc = unsafe { GetProcAddress(lib, s!("SendSAS")) };
    let result = match proc {
        Some(f) => {
            // SAFETY: `SendSAS` has the documented signature `VOID SendSAS(BOOL AsUser)`, so
            // reinterpreting the export as `SendSasFn` is sound.
            let send: SendSasFn = unsafe { std::mem::transmute::<_, SendSasFn>(f) };
            // SAFETY: calling the export with the documented argument; FALSE = from a
            // service.
            unsafe { send(false.into()) };
            tracing::info!("secure attention sequence sent");
            Ok(())
        }
        None => Err(os_err(
            "GetProcAddress(SendSAS)",
            &windows::core::Error::from_thread(),
        )),
    };
    // SAFETY: `lib` was loaded above and is released once.
    let _ = unsafe { FreeLibrary(lib) };
    result
}

/// Locks the session of the calling process (must run on the interactive desktop: the agent).
pub fn lock_workstation() -> Result<(), HostError> {
    // SAFETY: no arguments.
    unsafe { LockWorkStation() }.map_err(|e| os_err("LockWorkStation", &e))
}

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the key was opened by RegCreateKeyExW and is closed once.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

/// Reads the current policy value (`None` if absent).
fn read_policy(key: &Key) -> Option<u32> {
    let mut value = 0u32;
    let mut size = 4u32;
    // SAFETY: `key` is open with KEY_QUERY_VALUE; `value`/`size` are valid out pointers for a
    // REG_DWORD (4 bytes).
    let rc = unsafe {
        RegGetValueW(
            key.0,
            None,
            w!("SoftwareSASGeneration"),
            RRF_RT_REG_DWORD,
            None,
            Some(std::ptr::from_mut(&mut value).cast()),
            Some(&mut size),
        )
    };
    (rc == ERROR_SUCCESS).then_some(value)
}

/// Makes sure services may send SAS (elevated; called by `install-service`). Returns the value
/// now in effect.
pub fn enable_software_sas_for_services() -> Result<u32, HostError> {
    let mut hkey = HKEY::default();
    // SAFETY: valid predefined root key, static NUL-terminated subkey, valid out pointer.
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            w!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System"),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            None,
            &mut hkey,
            None,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(HostError::Os {
            call: "RegCreateKeyExW",
            code: rc.to_hresult().0,
        });
    }
    let key = Key(hkey);
    let existing = read_policy(&key);
    let Some(new) = sas_policy_value(existing) else {
        return Ok(existing.unwrap_or(1));
    };
    let bytes = new.to_le_bytes();
    // SAFETY: `key` is open with KEY_SET_VALUE; the data is a 4-byte REG_DWORD.
    let rc = unsafe {
        RegSetValueExW(
            key.0,
            w!("SoftwareSASGeneration"),
            None,
            REG_DWORD,
            Some(&bytes),
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(HostError::Os {
            call: "RegSetValueExW",
            code: rc.to_hresult().0,
        });
    }
    tracing::info!(old = ?existing, new, "SoftwareSASGeneration policy set");
    Ok(new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_value_keeps_other_permissions() {
        assert_eq!(sas_policy_value(None), Some(1));
        assert_eq!(sas_policy_value(Some(0)), Some(1));
        assert_eq!(sas_policy_value(Some(1)), None);
        assert_eq!(sas_policy_value(Some(2)), Some(3));
        assert_eq!(sas_policy_value(Some(3)), None);
        assert_eq!(sas_policy_value(Some(99)), Some(1));
    }

    #[test]
    fn policy_location_matches_the_documentation() {
        assert_eq!(
            SAS_POLICY_KEY,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System"
        );
        assert_eq!(SAS_POLICY_VALUE, "SoftwareSASGeneration");
    }
}
