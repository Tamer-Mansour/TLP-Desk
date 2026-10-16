//! Main-window state for tray mode: where the window is and what to do on close, open,
//! quit and incoming requests. Pure logic; the app turns [`WindowCmd`]s into viewport
//! commands.
//!
//! "In the tray" means minimized **and** removed from the taskbar, not hidden: a hidden eframe
//! root window stops running its UI pass, and the always-visible host banner is an egui
//! viewport that needs that pass (CLAUDE.md: the "who is connected" banner is always visible).

/// Where the main window is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowMode {
    /// Normal window with a taskbar button.
    Shown,
    /// Minimized with no taskbar button; reachable from the tray icon.
    InTray,
}

/// What the app must do with the main window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowCmd {
    /// Show the taskbar button again and un-minimize.
    Restore,
    /// Bring to the foreground.
    Focus,
    /// Flash the taskbar button.
    Attention,
    /// Minimize and remove the taskbar button.
    ToTray,
    /// Veto the pending close request.
    CancelClose,
    /// Close the window (quits the app).
    Close,
}

/// Main-window state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shell {
    mode: WindowMode,
    /// A tray icon exists (without one, "to tray" would strand the window).
    has_tray: bool,
    /// Quit was chosen: let the next close through.
    quitting: bool,
}

impl Shell {
    /// Initial state. Without a tray icon the window always starts shown.
    pub fn new(has_tray: bool) -> Self {
        Self {
            mode: WindowMode::Shown,
            has_tray,
            quitting: false,
        }
    }

    /// Current mode.
    pub fn mode(&self) -> WindowMode {
        self.mode
    }

    /// Whether the user chose Quit.
    pub fn quitting(&self) -> bool {
        self.quitting
    }

    /// First frame: go to the tray if asked (start minimized / autostart).
    pub fn startup(&mut self, start_in_tray: bool) -> Vec<WindowCmd> {
        if start_in_tray {
            self.to_tray()
        } else {
            Vec::new()
        }
    }

    /// Send the window to the tray (no-op without a tray icon).
    pub fn to_tray(&mut self) -> Vec<WindowCmd> {
        if !self.has_tray || self.mode == WindowMode::InTray {
            return Vec::new();
        }
        self.mode = WindowMode::InTray;
        vec![WindowCmd::ToTray]
    }

    /// The user closed the window (title-bar X, Alt+F4).
    pub fn close_requested(&mut self, close_to_tray: bool) -> Vec<WindowCmd> {
        if self.quitting || !close_to_tray || !self.has_tray {
            return Vec::new(); // let it close
        }
        let mut cmds = vec![WindowCmd::CancelClose];
        cmds.extend(self.to_tray());
        cmds
    }

    /// Tray "Open", a click on the tray icon, or a screen picked from the tray menu.
    pub fn open(&mut self) -> Vec<WindowCmd> {
        self.mode = WindowMode::Shown;
        vec![WindowCmd::Restore, WindowCmd::Focus]
    }

    /// An Accept prompt arrived: it must be seen, wherever the window is.
    pub fn incoming_request(&mut self) -> Vec<WindowCmd> {
        let mut cmds = self.open();
        cmds.push(WindowCmd::Attention);
        cmds
    }

    /// Tray "Quit": close for real.
    pub fn quit(&mut self) -> Vec<WindowCmd> {
        self.quitting = true;
        vec![WindowCmd::Close]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_goes_to_tray_only_when_configured_and_possible() {
        let mut s = Shell::new(true);
        assert_eq!(
            s.close_requested(true),
            [WindowCmd::CancelClose, WindowCmd::ToTray]
        );
        assert_eq!(s.mode(), WindowMode::InTray);
        // Already in the tray: still vetoed, nothing else to do.
        assert_eq!(s.close_requested(true), [WindowCmd::CancelClose]);
        assert_eq!(s.open(), [WindowCmd::Restore, WindowCmd::Focus]);
        assert!(
            s.close_requested(false).is_empty(),
            "close-to-tray off quits"
        );

        let mut no_tray = Shell::new(false);
        assert!(
            no_tray.close_requested(true).is_empty(),
            "no tray: never strand"
        );
        assert!(no_tray.startup(true).is_empty());
        assert_eq!(no_tray.mode(), WindowMode::Shown);
    }

    #[test]
    fn quit_lets_the_close_through() {
        let mut s = Shell::new(true);
        assert_eq!(s.quit(), [WindowCmd::Close]);
        assert!(s.quitting());
        assert!(s.close_requested(true).is_empty());
    }

    #[test]
    fn start_minimized_and_incoming_requests() {
        let mut s = Shell::new(true);
        assert_eq!(s.startup(true), [WindowCmd::ToTray]);
        assert_eq!(s.mode(), WindowMode::InTray);
        assert_eq!(
            s.incoming_request(),
            [WindowCmd::Restore, WindowCmd::Focus, WindowCmd::Attention]
        );
        assert_eq!(s.mode(), WindowMode::Shown);
        assert!(Shell::new(true).startup(false).is_empty());
    }
}
