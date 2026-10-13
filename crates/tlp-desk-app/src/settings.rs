//! Small per-user UI settings file: `%APPDATA%\tlp-desk\settings.json`.
//!
//! Only UI preferences live here (language, digit style). Keys and security settings live under
//! `%ProgramData%\tlp-desk\` and are owned by the crypto/host crates.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::i18n::{DigitStyle, Lang};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub lang: Lang,
    pub digits: DigitStyle,
    /// Name announced on the LAN; `None` = the computer name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").map(PathBuf::from)?;
    Some(base.join("tlp-desk").join("settings.json"))
}

impl Settings {
    /// Load settings. On first run (no file) the language follows the Windows UI language and
    /// the result is saved. A corrupt file yields defaults.
    pub fn load_or_first_run() -> Self {
        let Some(p) = path() else {
            return Self::first_run();
        };
        match std::fs::read_to_string(&p) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => {
                let s = Self::first_run();
                s.save();
                s
            }
        }
    }

    fn first_run() -> Self {
        Self {
            lang: Lang::detect(),
            digits: DigitStyle::Western,
            device_name: None,
        }
    }

    /// Best-effort save; failures are logged, never fatal.
    pub fn save(&self) {
        let Some(p) = path() else { return };
        let result = (|| -> anyhow::Result<()> {
            if let Some(dir) = p.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&p, serde_json::to_string_pretty(self)?)?;
            Ok(())
        })();
        if let Err(e) = result {
            tracing::warn!("could not save settings to {}: {e:#}", p.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip_and_defaults() {
        let s = Settings {
            lang: Lang::Ar,
            digits: DigitStyle::ArabicIndic,
            device_name: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#"{"lang":"ar","digits":"arabic_indic"}"#);
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), s);
        assert_eq!(
            serde_json::from_str::<Settings>("{}").unwrap(),
            Settings::default()
        );
        let named = Settings {
            device_name: Some("حاسوب المكتب".into()),
            ..s
        };
        let json = serde_json::to_string(&named).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), named);
    }
}
