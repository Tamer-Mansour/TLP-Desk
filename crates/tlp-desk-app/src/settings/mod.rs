//! Per-user settings file: `%APPDATA%\TLP-Desk\settings.json`.
//!
//! UI preferences (language, digit style, device name), what this device grants to incoming
//! sessions ([`HostSettings`]), the viewer's preferred quality ([`QualityChoice`]) and startup
//! behaviour ([`StartupSettings`]). Keys and the unattended password live under
//! `%ProgramData%\TLP-Desk\` and are owned by the crypto/host crates.
//!
//! Every struct is `#[serde(default)]` and unknown keys are ignored, so files written by older
//! and newer builds both load. [`Settings::sanitize`] clamps values a hand-edited file may have
//! pushed out of range; [`Settings::migrate`] upgrades older schema versions.

pub mod allow_list;
pub mod autostart;
pub mod host;

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use tlp_desk_proto::{QualityPreset, QualitySettings};

use crate::i18n::{DigitStyle, Lang};

pub use host::{HostSettings, PermissionSet};

/// Current schema version written to `settings.json`.
pub const SETTINGS_VERSION: u32 = 1;

/// Version assumed for files that have no `version` key (written before versioning).
fn legacy_version() -> u32 {
    0
}

/// Everything the app persists for the current Windows user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Schema version of the file (see [`SETTINGS_VERSION`]).
    #[serde(default = "legacy_version")]
    pub version: u32,
    pub lang: Lang,
    pub digits: DigitStyle,
    /// Name announced on the LAN; `None` = the computer name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    /// What incoming sessions get and who may connect.
    pub host: HostSettings,
    /// Picture quality this device asks for when it views another device.
    pub quality: QualityChoice,
    /// Autostart, start hidden, close-to-tray.
    pub startup: StartupSettings,
    /// New sessions start with clipboard sync off (default: on, so stored as an opt-out).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub clipboard_sync_off: bool,
    /// New viewer sessions start with the remote computer's sound off. Default `false`: sound
    /// is requested, and plays only if the host grants the `AUDIO` permission.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub mute_remote_audio: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            lang: Lang::default(),
            digits: DigitStyle::default(),
            device_name: None,
            host: HostSettings::default(),
            quality: QualityChoice::default(),
            startup: StartupSettings::default(),
            clipboard_sync_off: false,
            mute_remote_audio: false,
        }
    }
}

/// Viewer quality preset (Settings and the session toolbar). The host's encoder interprets it
/// (`Message::SetQuality`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityChoice {
    /// Lowest latency and bandwidth.
    Speed,
    /// Good picture at moderate bandwidth.
    #[default]
    Balanced,
    /// Best picture.
    Quality,
}

impl QualityChoice {
    /// All presets in display order.
    pub const ALL: [QualityChoice; 3] = [Self::Speed, Self::Balanced, Self::Quality];

    /// The wire request: the preset only, size/fps/bitrate left to the host ("0 = host
    /// default", adaptive bitrate).
    pub fn to_proto(self) -> QualitySettings {
        QualitySettings {
            preset: match self {
                Self::Speed => QualityPreset::Speed,
                Self::Balanced => QualityPreset::Balanced,
                Self::Quality => QualityPreset::Quality,
            },
            max_width: 0,
            max_height: 0,
            fps: 0,
            bitrate_kbps: None,
        }
    }
}

/// How the app starts and closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StartupSettings {
    /// Start TLP-Desk when the user signs in to Windows (HKCU `Run` key).
    pub start_with_windows: bool,
    /// Start hidden in the notification area (tray).
    pub start_minimized: bool,
    /// Closing the window hides it to the tray instead of quitting (hosting keeps running).
    pub close_to_tray: bool,
}

impl Default for StartupSettings {
    fn default() -> Self {
        Self {
            start_with_windows: false,
            start_minimized: false,
            close_to_tray: true,
        }
    }
}

/// Settings folder name under `%APPDATA%`. Windows paths are case-insensitive, so this is the
/// same folder older builds created as `tlp-desk` (existing settings are kept).
pub const APP_DIR: &str = "TLP-Desk";

/// `%APPDATA%\TLP-Desk`, the per-user data folder (`None` if `APPDATA` is not set).
///
/// On macOS this is `~/Library/Application Support/TLP-Desk`.
pub fn app_data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").map(PathBuf::from)?;
        Some(macos_app_data_dir(&home))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let base = std::env::var_os("APPDATA").map(PathBuf::from)?;
        Some(base.join(APP_DIR))
    }
}

/// The macOS per-user data folder for a home directory.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn macos_app_data_dir(home: &std::path::Path) -> PathBuf {
    home.join("Library")
        .join("Application Support")
        .join(APP_DIR)
}

fn default_path() -> Option<PathBuf> {
    Some(app_data_dir()?.join("settings.json"))
}

impl Settings {
    /// Load settings. On first run (no file) the language follows the Windows UI language and
    /// the result is saved. A corrupt file yields defaults (and is left on disk untouched until
    /// the next save).
    pub fn load_or_first_run() -> Self {
        let Some(p) = default_path() else {
            return Self::first_run();
        };
        match Self::load_from(&p) {
            Ok(Some(s)) => s,
            Ok(None) => {
                let s = Self::first_run();
                s.save();
                s
            }
            Err(e) => {
                tracing::warn!("ignoring unreadable settings file {}: {e:#}", p.display());
                Self::first_run()
            }
        }
    }

    /// Read, migrate and sanitize the file at `path`. `Ok(None)` if it does not exist.
    ///
    /// # Errors
    /// The file exists but cannot be read or is not valid JSON.
    pub fn load_from(path: &Path) -> anyhow::Result<Option<Self>> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).context("reading settings"),
        };
        let mut s: Self = serde_json::from_str(&text).context("parsing settings")?;
        s.migrate();
        s.sanitize();
        Ok(Some(s))
    }

    /// Write the settings as pretty JSON to `path` (creating the folder). The file is written
    /// next to the target and renamed over it, so a crash never leaves half a file.
    ///
    /// # Errors
    /// The folder or file cannot be written.
    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).context("creating the settings folder")?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?).context("writing settings")?;
        std::fs::rename(&tmp, path).context("replacing settings")?;
        Ok(())
    }

    fn first_run() -> Self {
        Self {
            lang: Lang::detect(),
            ..Self::default()
        }
    }

    /// Upgrade a file written by an older build. Unknown newer versions are kept as they are
    /// (their extra keys were already ignored).
    pub fn migrate(&mut self) {
        if self.version < 1 {
            // v0 (before versioning) only had lang/digits/device_name; every new section was
            // filled with defaults by serde. Nothing else to convert.
            tracing::info!(from = self.version, to = 1, "migrating settings");
            self.version = 1;
        }
    }

    /// Clamp values a hand-edited file may have pushed out of range.
    pub fn sanitize(&mut self) {
        if self
            .device_name
            .as_ref()
            .is_some_and(|n| n.trim().is_empty())
        {
            self.device_name = None;
        }
        self.host.sanitize();
    }

    /// Best-effort save to `%APPDATA%\TLP-Desk\settings.json`; failures are logged, never
    /// fatal.
    pub fn save(&self) {
        let Some(p) = default_path() else { return };
        if let Err(e) = self.save_to(&p) {
            tracing::warn!("could not save settings to {}: {e:#}", p.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_data_dir_is_application_support() {
        let d = macos_app_data_dir(std::path::Path::new("/Users/tamer"));
        assert!(d.ends_with("Library/Application Support/TLP-Desk"));
    }

    #[test]
    fn settings_roundtrip_and_defaults() {
        let s = Settings {
            lang: Lang::Ar,
            digits: DigitStyle::ArabicIndic,
            ..Settings::default()
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.starts_with(r#"{"version":1,"lang":"ar","digits":"arabic_indic","host":{"#));
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), s);
        let named = Settings {
            device_name: Some("حاسوب المكتب".into()),
            quality: QualityChoice::Quality,
            ..s
        };
        let json = serde_json::to_string(&named).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), named);
    }

    #[test]
    fn empty_file_gives_defaults_and_v0_file_migrates() {
        let mut empty: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.version, 0, "a file without a version is v0");
        empty.migrate();
        assert_eq!(empty, Settings::default());

        // A file written by the M1 build: only lang/digits/device_name.
        let mut old: Settings =
            serde_json::from_str(r#"{"lang":"ar","digits":"western","device_name":"مكتب 2"}"#)
                .unwrap();
        old.migrate();
        old.sanitize();
        assert_eq!(old.version, SETTINGS_VERSION);
        assert_eq!(old.lang, Lang::Ar);
        assert_eq!(old.device_name.as_deref(), Some("مكتب 2"));
        assert_eq!(old.host, HostSettings::default());
        assert_eq!(old.startup, StartupSettings::default());
    }

    #[test]
    fn unknown_keys_and_newer_versions_still_load() {
        let s: Settings = serde_json::from_str(
            r#"{"version":7,"lang":"en","future_key":{"x":1},"host":{"also_new":true}}"#,
        )
        .unwrap();
        assert_eq!(s.version, 7);
        assert_eq!(s.host, HostSettings::default());
    }

    #[test]
    fn load_and_save_through_a_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("settings.json");
        assert!(Settings::load_from(&path).unwrap().is_none());

        let mut s = Settings {
            device_name: Some("جهاز الاستقبال".into()),
            ..Settings::default()
        };
        s.host.accept_timeout_secs = 45;
        s.startup.start_minimized = true;
        s.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path).unwrap(), Some(s));

        std::fs::write(&path, "{ not json").unwrap();
        assert!(Settings::load_from(&path).is_err());
    }

    #[test]
    fn sanitize_drops_blank_names() {
        let mut s = Settings {
            device_name: Some("   ".into()),
            ..Settings::default()
        };
        s.sanitize();
        assert_eq!(s.device_name, None);
    }

    #[test]
    fn quality_choice_maps_to_wire_presets() {
        assert_eq!(QualityChoice::default(), QualityChoice::Balanced);
        assert_eq!(QualityChoice::Speed.to_proto().preset, QualityPreset::Speed);
        let q = QualityChoice::Quality.to_proto();
        assert_eq!(q.preset, QualityPreset::Quality);
        assert_eq!((q.max_width, q.max_height, q.fps), (0, 0, 0));
        assert_eq!(q.bitrate_kbps, None);
        assert_eq!(
            serde_json::to_string(&QualityChoice::Speed).unwrap(),
            r#""speed""#
        );
    }

    #[test]
    fn remote_sound_is_on_by_default_and_the_mute_setting_persists() {
        assert!(!Settings::default().mute_remote_audio);
        let muted = Settings {
            mute_remote_audio: true,
            ..Settings::default()
        };
        let json = serde_json::to_string(&muted).unwrap();
        assert!(json.contains(r#""mute_remote_audio":true"#), "{json}");
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), muted);
    }
}
