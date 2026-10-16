//! Linux capture backends.
//!
//! * **X11** ([`x11`]): reads the root window per RandR monitor with `GetImage` and gets the
//!   cursor image/position from XFixes. Works in any X session, including Xvfb and the X
//!   servers used for headless hosts.
//! * **Wayland**: a Wayland compositor does not let clients read the screen. The supported
//!   route is the xdg-desktop-portal ScreenCast interface, which hands out a PipeWire stream
//!   after the user approves a dialog. That path is **not implemented yet**: capture fails with
//!   a clear [`CaptureError::Platform`] message. Set `TLP_DESK_FORCE_X11=1` to capture the
//!   XWayland root anyway (it shows only X11 windows and is usually black). The design notes
//!   live in `docs/src/dev/linux.md`.

// The pure helpers are unit tested on every platform; only Linux builds use them.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) mod layout;
#[cfg(target_os = "linux")]
mod x11;

#[cfg(target_os = "linux")]
pub(crate) use imp::{list_monitors, open_capturer};

#[cfg(target_os = "linux")]
mod imp {
    use super::layout::{classify_session, SessionKind};
    use super::x11;
    use crate::{CaptureError, CaptureOptions, MonitorInfo, ScreenCapturer};

    fn session() -> SessionKind {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        if env("TLP_DESK_FORCE_X11").is_some() && env("DISPLAY").is_some() {
            return SessionKind::X11;
        }
        classify_session(
            env("XDG_SESSION_TYPE").as_deref(),
            env("DISPLAY").is_some(),
            env("WAYLAND_DISPLAY").is_some(),
        )
    }

    fn check_session() -> Result<(), CaptureError> {
        match session() {
            SessionKind::X11 => Ok(()),
            SessionKind::Wayland => Err(CaptureError::Platform(
                "this is a Wayland session; screen capture there needs the ScreenCast portal \
                 (not implemented yet). Log in to an X11 session, or set TLP_DESK_FORCE_X11=1 \
                 to capture XWayland windows only"
                    .into(),
            )),
            SessionKind::NoDisplay => Err(CaptureError::Platform(
                "DISPLAY is not set; there is no X server to capture".into(),
            )),
        }
    }

    pub(crate) fn list_monitors() -> Result<Vec<MonitorInfo>, CaptureError> {
        check_session()?;
        x11::list_monitors()
    }

    pub(crate) fn open_capturer(
        monitor_index: u32,
        options: CaptureOptions,
    ) -> Result<Box<dyn ScreenCapturer>, CaptureError> {
        check_session()?;
        x11::open_capturer(monitor_index, options)
    }
}
