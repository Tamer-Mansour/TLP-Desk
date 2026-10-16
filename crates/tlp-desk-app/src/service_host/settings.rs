//! Machine-wide host settings of the service: `%ProgramData%\TLP-Desk\host-settings.json`.
//!
//! The service runs as SYSTEM and never reads a user's `settings.json`. The user's GUI sends
//! its host settings over the GUI pipe when they change (the user cannot write this folder
//! without elevation); the service validates them like a hostile file, applies them live and
//! saves them here (ACL: SYSTEM and Administrators write, users read).

use std::io;
use std::path::{Path, PathBuf};

use crate::settings::HostSettings;

/// File name of the machine-wide host settings, in the machine data folder.
pub const HOST_SETTINGS_FILE: &str = "host-settings.json";

/// Largest settings text accepted (a real file is under 2 KB).
pub const MAX_SETTINGS_BYTES: usize = 64 * 1024;

/// Path of the settings file in `data_dir`.
pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(HOST_SETTINGS_FILE)
}

/// Parses and repairs settings text (hostile input: size capped, unknown fields ignored,
/// numbers clamped, invalid allow-list entries dropped, `VIEW` kept).
pub fn parse(json: &str) -> Result<HostSettings, String> {
    if json.len() > MAX_SETTINGS_BYTES {
        return Err(format!("settings larger than {MAX_SETTINGS_BYTES} bytes"));
    }
    let mut s: HostSettings = serde_json::from_str(json).map_err(|e| e.to_string())?;
    s.sanitize();
    Ok(s)
}

/// The saved settings, or the defaults when there is no (valid) file.
pub fn load(data_dir: &Path) -> HostSettings {
    let p = path(data_dir);
    match std::fs::read_to_string(&p) {
        Ok(text) => parse(&text).unwrap_or_else(|e| {
            tracing::warn!(%e, path = %p.display(), "invalid machine host settings; using defaults");
            HostSettings::default()
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => HostSettings::default(),
        Err(e) => {
            tracing::warn!(%e, path = %p.display(), "cannot read machine host settings");
            HostSettings::default()
        }
    }
}

/// Saves `settings` atomically (temp file, then rename).
pub fn save(data_dir: &Path, settings: &HostSettings) -> io::Result<PathBuf> {
    std::fs::create_dir_all(data_dir)?;
    let p = path(data_dir);
    let tmp = p.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(settings).map_err(io::Error::other)?;
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &p)?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use tlp_desk_proto::Permissions;

    use super::*;

    #[test]
    fn missing_file_gives_defaults_and_save_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), HostSettings::default());
        let mut s = HostSettings {
            accept_timeout_secs: 45,
            allow_one_time_code: false,
            allow_list: vec!["192.168.50.0/24".into()],
            ask_on_upload_conflict: true,
            ..HostSettings::default()
        };
        s.default_permissions.set(Permissions::AUDIO, true);
        save(dir.path(), &s).unwrap();
        assert_eq!(load(dir.path()), s);
    }

    #[test]
    fn hostile_values_are_repaired_or_refused() {
        let s = parse(
            r#"{"accept_timeout_secs": 4294967295, "local_input_grace_ms": 99999999,
                "allow_list": ["0.0.0.0/0x", "10.1.0.0/16", "نص عربي"],
                "default_permissions": ["control", "teleport"], "unknown_field": 1}"#,
        )
        .unwrap();
        assert_eq!(s.accept_timeout_secs, 600);
        assert_eq!(s.local_input_grace_ms, 5000);
        assert_eq!(s.allow_list, vec!["10.1.0.0/16".to_string()]);
        assert!(s.default_permissions.0.contains(Permissions::VIEW));
        assert!(s.default_permissions.0.contains(Permissions::CONTROL));
        assert!(parse("not json").is_err());
        assert!(parse(r#"{"accept_timeout_secs": -5}"#).is_err());
        assert!(parse(&" ".repeat(MAX_SETTINGS_BYTES + 1)).is_err());
        // A corrupt file falls back to the defaults.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path(dir.path()), "{{{").unwrap();
        assert_eq!(load(dir.path()), HostSettings::default());
    }
}
