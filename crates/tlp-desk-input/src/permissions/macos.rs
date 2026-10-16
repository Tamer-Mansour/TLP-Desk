//! Apple API calls behind [`super`]: TCC permission queries.

use objc2_core_foundation::{CFBoolean, CFDictionary, CFString};

use super::PermissionState;

// CoreGraphics and ApplicationServices are linked by `objc2-core-graphics`; the permission
// functions below are not wrapped by the safe crates, so they are declared here from Apple's
// documentation (`CGPreflightScreenCaptureAccess`, `CGRequestScreenCaptureAccess`,
// `AXIsProcessTrusted`, `AXIsProcessTrustedWithOptions`).
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool;
}

fn state(granted: bool) -> PermissionState {
    if granted {
        PermissionState::Granted
    } else {
        PermissionState::Denied
    }
}

pub(super) fn screen_recording() -> PermissionState {
    // SAFETY: no arguments, returns a bool; safe to call from any thread.
    state(unsafe { CGPreflightScreenCaptureAccess() })
}

pub(super) fn request_screen_recording() -> PermissionState {
    // SAFETY: no arguments, returns a bool; shows the system prompt at most once per app.
    state(unsafe { CGRequestScreenCaptureAccess() })
}

pub(super) fn accessibility() -> PermissionState {
    // SAFETY: no arguments, returns a bool.
    state(unsafe { AXIsProcessTrusted() })
}

pub(super) fn request_accessibility() -> PermissionState {
    // The documented option key `kAXTrustedCheckOptionPrompt` has the string value
    // "AXTrustedCheckOptionPrompt"; true asks the system to show its prompt.
    let key = CFString::from_static_str("AXTrustedCheckOptionPrompt");
    let options: objc2_core_foundation::CFRetained<CFDictionary<CFString, CFBoolean>> =
        CFDictionary::from_slices(&[&*key], &[CFBoolean::new(true)]);
    let ptr: *const CFDictionary<CFString, CFBoolean> = &*options;
    // SAFETY: `ptr` points to a live CFDictionary kept alive by `options` for the whole call;
    // AXIsProcessTrustedWithOptions only reads it.
    state(unsafe { AXIsProcessTrustedWithOptions(ptr.cast()) })
}
