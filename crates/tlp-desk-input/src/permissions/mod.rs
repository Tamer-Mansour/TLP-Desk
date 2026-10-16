//! Operating-system privacy permissions the host needs (macOS).
//!
//! macOS only lets an app capture the screen after the user ticks it under *System Settings >
//! Privacy & Security > Screen Recording*, and only lets it inject keyboard/mouse events after
//! *Accessibility*. Without them capture returns blank frames and injected events vanish
//! silently, so the app checks both up front and shows guidance (a button that opens the right
//! Settings pane) instead of a black screen. Windows has no such permissions: every query
//! reports [`PermissionState::NotApplicable`].
//!
//! The state queries and the Settings deep links are the only places that touch Apple APIs; the
//! rest ([`HostPermissions`], [`settings_url`]) is plain logic with unit tests.

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos;

use crate::InputError;

/// One privacy permission the host may need.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    /// Capture the screen contents (`CGPreflightScreenCaptureAccess`).
    ScreenRecording,
    /// Post keyboard and mouse events (`AXIsProcessTrusted`).
    Accessibility,
}

/// Whether a [`Permission`] is currently held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionState {
    /// The user granted it.
    Granted,
    /// Not granted yet (or revoked); the feature does not work.
    Denied,
    /// This OS has no such permission, nothing to do.
    NotApplicable,
}

impl PermissionState {
    /// True when the feature can work: granted or not needed.
    pub fn is_ok(self) -> bool {
        !matches!(self, PermissionState::Denied)
    }
}

/// Snapshot of every host permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostPermissions {
    /// Screen Recording.
    pub screen_recording: PermissionState,
    /// Accessibility.
    pub accessibility: PermissionState,
}

impl HostPermissions {
    /// The permissions that are still missing, in the order the user should grant them.
    pub fn missing(&self) -> Vec<Permission> {
        let mut out = Vec::new();
        if !self.screen_recording.is_ok() {
            out.push(Permission::ScreenRecording);
        }
        if !self.accessibility.is_ok() {
            out.push(Permission::Accessibility);
        }
        out
    }

    /// True when nothing is missing.
    pub fn all_granted(&self) -> bool {
        self.missing().is_empty()
    }
}

/// Reads both permissions without prompting.
pub fn host_permissions() -> HostPermissions {
    HostPermissions {
        screen_recording: screen_recording(),
        accessibility: accessibility(),
    }
}

/// Screen Recording state, without prompting.
pub fn screen_recording() -> PermissionState {
    #[cfg(target_os = "macos")]
    {
        macos::screen_recording()
    }
    #[cfg(not(target_os = "macos"))]
    {
        PermissionState::NotApplicable
    }
}

/// Accessibility state, without prompting.
pub fn accessibility() -> PermissionState {
    #[cfg(target_os = "macos")]
    {
        macos::accessibility()
    }
    #[cfg(not(target_os = "macos"))]
    {
        PermissionState::NotApplicable
    }
}

/// Asks macOS to show its own permission prompt (and add the app to the Settings list).
/// Returns the state right after the request; the user usually answers later, so poll
/// [`host_permissions`]. A no-op elsewhere.
pub fn request(permission: Permission) -> PermissionState {
    #[cfg(target_os = "macos")]
    {
        match permission {
            Permission::ScreenRecording => macos::request_screen_recording(),
            Permission::Accessibility => macos::request_accessibility(),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = permission;
        PermissionState::NotApplicable
    }
}

/// The `x-apple.systempreferences:` deep link that opens the Settings pane for `permission`.
///
/// These URLs are the documented Privacy anchors and also work on macOS 13+ System Settings.
pub fn settings_url(permission: Permission) -> &'static str {
    match permission {
        Permission::ScreenRecording => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
        }
        Permission::Accessibility => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
        }
    }
}

/// Opens System Settings at the pane for `permission`. [`InputError::Unsupported`] off macOS.
pub fn open_settings(permission: Permission) -> Result<(), InputError> {
    #[cfg(target_os = "macos")]
    {
        // `open` is the supported way to follow a URL scheme from a process.
        std::process::Command::new("open")
            .arg(settings_url(permission))
            .spawn()
            .map(|_| ())
            .map_err(|e| InputError::Platform(format!("cannot open System Settings: {e}")))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = permission;
        Err(InputError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perms(sr: PermissionState, ax: PermissionState) -> HostPermissions {
        HostPermissions {
            screen_recording: sr,
            accessibility: ax,
        }
    }

    #[test]
    fn missing_lists_denied_in_grant_order() {
        use PermissionState::*;
        assert!(perms(Granted, Granted).all_granted());
        assert!(perms(NotApplicable, NotApplicable).all_granted());
        assert_eq!(
            perms(Denied, Denied).missing(),
            vec![Permission::ScreenRecording, Permission::Accessibility]
        );
        assert_eq!(
            perms(Granted, Denied).missing(),
            vec![Permission::Accessibility]
        );
    }

    #[test]
    fn settings_urls_target_the_privacy_panes() {
        for p in [Permission::ScreenRecording, Permission::Accessibility] {
            assert!(settings_url(p).starts_with("x-apple.systempreferences:"));
        }
        assert!(settings_url(Permission::ScreenRecording).ends_with("Privacy_ScreenCapture"));
        assert!(settings_url(Permission::Accessibility).ends_with("Privacy_Accessibility"));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn nothing_is_required_off_macos() {
        assert!(host_permissions().all_granted());
        assert!(matches!(
            open_settings(Permission::Accessibility),
            Err(InputError::Unsupported)
        ));
    }
}
