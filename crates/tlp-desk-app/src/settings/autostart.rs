//! "Start with Windows" (on Linux: a freedesktop autostart `.desktop` file in
//! `~/.config/autostart`): a value under the current user's `Run` key
//! (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`), so no admin rights are needed and
//! nothing is installed system-wide.
//!
//! The UI talks to the [`AutoStart`] trait; tests use [`FakeAutoStart`] and never touch the
//! real registry.

use std::path::Path;
use std::sync::Mutex;

/// Name of the value under the `Run` key.
pub const RUN_VALUE_NAME: &str = "TLP-Desk";

/// Command-line flag the `Run` entry passes, so an autostart launch starts hidden in the tray.
pub const AUTOSTART_FLAG: &str = "--autostart";

/// The command line stored in the `Run` key for `exe` (quoted: Program Files has spaces).
pub fn run_command(exe: &Path) -> String {
    format!("\"{}\" {AUTOSTART_FLAG}", exe.display())
}

/// Turns "start with Windows" on and off.
pub trait AutoStart: Send + Sync {
    /// Whether the entry currently exists.
    ///
    /// # Errors
    /// The setting cannot be read (e.g. registry access denied).
    fn is_enabled(&self) -> anyhow::Result<bool>;

    /// Create (`true`) or remove (`false`) the entry.
    ///
    /// # Errors
    /// The setting cannot be written.
    fn set_enabled(&self, enabled: bool) -> anyhow::Result<()>;
}

/// In-memory [`AutoStart`] for tests and the fake backend.
#[derive(Debug, Default)]
pub struct FakeAutoStart {
    enabled: Mutex<bool>,
    /// Make every call fail (to test error display).
    pub fail: bool,
}

impl AutoStart for FakeAutoStart {
    fn is_enabled(&self) -> anyhow::Result<bool> {
        anyhow::ensure!(!self.fail, "simulated failure");
        Ok(*self.enabled.lock().unwrap_or_else(|p| p.into_inner()))
    }

    fn set_enabled(&self, enabled: bool) -> anyhow::Result<()> {
        anyhow::ensure!(!self.fail, "simulated failure");
        *self.enabled.lock().unwrap_or_else(|p| p.into_inner()) = enabled;
        Ok(())
    }
}

/// Apply the desired state, returning what is in effect afterwards. On error the previous
/// state is reported so the checkbox does not lie.
pub fn apply(auto: &dyn AutoStart, enabled: bool) -> Result<bool, String> {
    match auto.set_enabled(enabled) {
        Ok(()) => Ok(enabled),
        Err(e) => {
            tracing::warn!("could not change start with Windows: {e:#}");
            Err(format!("{e:#}"))
        }
    }
}

/// The platform implementation (the `Run` key on Windows; unsupported elsewhere).
pub fn system() -> Box<dyn AutoStart> {
    #[cfg(windows)]
    {
        match std::env::current_exe() {
            Ok(exe) => Box::new(windows::RunKey::new(RUN_VALUE_NAME, run_command(&exe))),
            Err(e) => {
                tracing::warn!(%e, "cannot find our own executable; autostart unavailable");
                Box::new(Unsupported)
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        match (std::env::current_exe(), autostart_dir()) {
            (Ok(exe), Some(dir)) => Box::new(xdg::DesktopAutostart::new(dir, &exe)),
            _ => {
                tracing::warn!("no executable or config folder; autostart unavailable");
                Box::new(Unsupported)
            }
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Box::new(Unsupported)
    }
}

/// `$XDG_CONFIG_HOME/autostart` or `~/.config/autostart` (freedesktop Desktop Application
/// Autostart specification), `None` without a usable home.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn autostart_dir() -> Option<std::path::PathBuf> {
    autostart_dir_in(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn autostart_dir_in(
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
    let base = xdg_config_home
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            home.filter(|h| !h.is_empty())
                .map(|h| std::path::PathBuf::from(h).join(".config"))
        })?;
    Some(base.join("autostart"))
}

/// The text of the autostart `.desktop` file for `exe` (freedesktop Desktop Entry spec).
///
/// The `Exec` value is double-quoted with `"`, `` ` ``, `$` and `\` escaped, so paths with
/// spaces or Arabic names start correctly.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn desktop_entry(exe: &Path) -> String {
    let mut quoted = String::from("\"");
    for c in exe.to_string_lossy().chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    format!(
        "[Desktop Entry]\nType=Application\nName=TLP-Desk\nExec={quoted} {AUTOSTART_FLAG}\n\
         X-GNOME-Autostart-enabled=true\nTerminal=false\n"
    )
}

/// Autostart is not available on this platform / install.
#[derive(Debug, Default)]
pub struct Unsupported;

impl AutoStart for Unsupported {
    fn is_enabled(&self) -> anyhow::Result<bool> {
        Ok(false)
    }

    fn set_enabled(&self, _enabled: bool) -> anyhow::Result<()> {
        anyhow::bail!("starting with the system is not supported here")
    }
}

#[cfg(target_os = "linux")]
mod xdg {
    use std::path::{Path, PathBuf};

    use anyhow::Context as _;

    use super::{desktop_entry, AutoStart};

    /// `~/.config/autostart/TLP-Desk.desktop`: per user, no elevation.
    pub struct DesktopAutostart {
        file: PathBuf,
        contents: String,
    }

    impl DesktopAutostart {
        pub fn new(dir: PathBuf, exe: &Path) -> Self {
            Self {
                file: dir.join("TLP-Desk.desktop"),
                contents: desktop_entry(exe),
            }
        }
    }

    impl AutoStart for DesktopAutostart {
        fn is_enabled(&self) -> anyhow::Result<bool> {
            Ok(self.file.is_file())
        }

        fn set_enabled(&self, enabled: bool) -> anyhow::Result<()> {
            if enabled {
                if let Some(dir) = self.file.parent() {
                    std::fs::create_dir_all(dir).context("creating the autostart folder")?;
                }
                std::fs::write(&self.file, &self.contents)
                    .context("writing the autostart entry")?;
            } else if self.file.is_file() {
                std::fs::remove_file(&self.file).context("removing the autostart entry")?;
            }
            tracing::info!(enabled, "start with the desktop session changed");
            Ok(())
        }
    }
}

#[cfg(windows)]
mod windows {
    use anyhow::Context as _;

    use super::AutoStart;

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    /// A value under `HKCU\...\Run` (per user, no elevation).
    pub struct RunKey {
        name: String,
        command: String,
    }

    impl RunKey {
        pub fn new(name: &str, command: String) -> Self {
            Self {
                name: name.to_owned(),
                command,
            }
        }
    }

    impl AutoStart for RunKey {
        fn is_enabled(&self) -> anyhow::Result<bool> {
            let Ok(key) = windows_registry::CURRENT_USER.open(RUN_KEY) else {
                return Ok(false);
            };
            Ok(key.get_string(&self.name).is_ok())
        }

        fn set_enabled(&self, enabled: bool) -> anyhow::Result<()> {
            let key = windows_registry::CURRENT_USER
                .create(RUN_KEY)
                .context("opening the Run key")?;
            if enabled {
                key.set_string(&self.name, &self.command)
                    .context("writing the Run value")?;
            } else if key.get_string(&self.name).is_ok() {
                key.remove_value(&self.name)
                    .context("removing the Run value")?;
            }
            tracing::info!(enabled, "start with Windows changed");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_command_is_quoted_and_flagged() {
        let exe = Path::new(r"C:\Program Files\TLP-Desk\برنامج\TLP-Desk.exe");
        assert_eq!(
            run_command(exe),
            "\"C:\\Program Files\\TLP-Desk\\برنامج\\TLP-Desk.exe\" --autostart"
        );
    }

    #[test]
    fn desktop_entry_quotes_the_executable() {
        let text = desktop_entry(Path::new("/opt/TLP Desk/برنامج/TLP-Desk"));
        assert!(text.starts_with("[Desktop Entry]\n"));
        assert!(
            text.contains("Exec=\"/opt/TLP Desk/برنامج/TLP-Desk\" --autostart\n"),
            "{text}"
        );
        let tricky = desktop_entry(Path::new("/a\"b$c`d"));
        assert!(
            tricky.contains("Exec=\"/a\\\"b\\$c\\`d\" --autostart"),
            "{tricky}"
        );
    }

    #[test]
    fn autostart_folder_follows_xdg() {
        use std::ffi::OsString;
        let os = |s: &str| Some(OsString::from(s));
        let cfg = std::env::temp_dir().join("cfg");
        let home = std::env::temp_dir().join("home");
        let os_path = |p: &std::path::PathBuf| Some(p.clone().into_os_string());
        assert_eq!(
            autostart_dir_in(os_path(&cfg), os_path(&home)),
            Some(cfg.join("autostart"))
        );
        assert_eq!(
            autostart_dir_in(os("relative"), os_path(&home)),
            Some(home.join(".config").join("autostart"))
        );
        assert_eq!(autostart_dir_in(None, None), None);
    }

    #[test]
    fn fake_autostart_toggles_and_reports_errors() {
        let fake = FakeAutoStart::default();
        assert!(!fake.is_enabled().unwrap());
        assert_eq!(apply(&fake, true), Ok(true));
        assert!(fake.is_enabled().unwrap());
        assert_eq!(apply(&fake, false), Ok(false));
        assert!(!fake.is_enabled().unwrap());

        let broken = FakeAutoStart {
            fail: true,
            ..FakeAutoStart::default()
        };
        assert!(apply(&broken, true).is_err());
        assert!(Unsupported.set_enabled(true).is_err());
        assert!(!Unsupported.is_enabled().unwrap());
    }
}
