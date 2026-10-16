//! Which Windows session the agent belongs in, driven by `SERVICE_CONTROL_SESSIONCHANGE`.
//!
//! The agent always runs in the **active console session** (the one attached to the physical
//! monitor, keyboard and mouse; `WTSGetActiveConsoleSessionId`). Notifications only say *what*
//! happened; the active console session queried right after the notification says *where*
//! the agent must be. [`SessionTracker::on_change`] combines both into [`SessionAction`]s.
//!
//! What happens on a real machine (Microsoft Learn, "WM_WTSSESSION_CHANGE"):
//!
//! | Situation | Notifications |
//! |---|---|
//! | Boot to the login screen | console connect (session 1) |
//! | User logs on | logon (same session) |
//! | Win+L / lock | lock; unlock later |
//! | Log off | logoff, session terminate; a new session gets console connect |
//! | Fast user switching | console disconnect (old), console connect (new), logon |
//! | RDP into the console user (Pro) | console disconnect, remote connect (session leaves the console) |

/// A session-change notification reason (`WTS_*` codes of `WM_WTSSESSION_CHANGE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionChange {
    ConsoleConnect,
    ConsoleDisconnect,
    RemoteConnect,
    RemoteDisconnect,
    Logon,
    Logoff,
    Lock,
    Unlock,
    RemoteControl,
    Create,
    Terminate,
}

/// What the service must do after a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAction {
    /// Run the agent in this session (stopping one in another session first).
    StartAgent(u32),
    /// Restart the agent in the same session (logon/logoff changed who owns the desktop).
    RestartAgent(u32),
    /// No console session right now (switch in progress): stop the agent.
    StopAgent,
    /// The console session was locked (`true`) or unlocked (`false`).
    LockChanged(bool),
}

/// Tracks the agent's target session and the console lock state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionTracker {
    target: Option<u32>,
    locked: bool,
}

/// `WTSGetActiveConsoleSessionId` returns this when no session is attached to the console.
pub const NO_CONSOLE_SESSION: u32 = 0xFFFF_FFFF;

/// Maps the raw value of `WTSGetActiveConsoleSessionId` to an option. Session 0 never hosts the
/// console since Windows Vista (session 0 isolation), so it is treated as "none" too.
pub fn console_session(raw: u32) -> Option<u32> {
    (raw != NO_CONSOLE_SESSION && raw != 0).then_some(raw)
}

impl SessionTracker {
    /// A tracker that has not started an agent yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The session the agent should run in (`None` = no agent).
    pub fn target(&self) -> Option<u32> {
        self.target
    }

    /// Whether the console session is locked (as far as notifications told us).
    pub fn locked(&self) -> bool {
        self.locked
    }

    /// Sets the lock state from a direct query (`WTSSessionInfoEx` after starting an agent in a
    /// session that may already be locked, e.g. switching back to a locked user).
    pub fn set_locked(&mut self, locked: bool) -> Option<SessionAction> {
        (self.locked != locked).then(|| {
            self.locked = locked;
            SessionAction::LockChanged(locked)
        })
    }

    /// Initial decision when the service starts: run the agent in the console session, if any.
    pub fn start(&mut self, active_console: Option<u32>) -> Vec<SessionAction> {
        self.retarget(active_console)
    }

    /// Handles one notification about `session`; `active_console` is the console session
    /// queried right after it (see [`console_session`]).
    pub fn on_change(
        &mut self,
        change: SessionChange,
        session: u32,
        active_console: Option<u32>,
    ) -> Vec<SessionAction> {
        let mut actions = Vec::new();
        let about_console = active_console == Some(session) || self.target == Some(session);
        match change {
            SessionChange::Lock if about_console => {
                if !self.locked {
                    self.locked = true;
                    actions.push(SessionAction::LockChanged(true));
                }
            }
            SessionChange::Unlock if about_console => {
                if self.locked {
                    self.locked = false;
                    actions.push(SessionAction::LockChanged(false));
                }
            }
            // A logon or logoff in the console session changes the desktop's owner: restart
            // the agent there (it re-opens its desktops and picks up the new user).
            SessionChange::Logon | SessionChange::Logoff
                if about_console && self.target == active_console =>
            {
                if let Some(s) = self.target {
                    actions.push(SessionAction::RestartAgent(s));
                }
                if change == SessionChange::Logon && self.locked {
                    self.locked = false;
                    actions.push(SessionAction::LockChanged(false));
                }
                return actions;
            }
            _ => {}
        }
        actions.extend(self.retarget(active_console));
        actions
    }

    /// Moves the target to `active_console` if it differs.
    fn retarget(&mut self, active_console: Option<u32>) -> Vec<SessionAction> {
        if self.target == active_console {
            return Vec::new();
        }
        let mut actions = Vec::new();
        // A new console session starts unlocked (its login screen is the secure desktop, which
        // the agent reports separately).
        if self.locked {
            self.locked = false;
            actions.push(SessionAction::LockChanged(false));
        }
        self.target = active_console;
        actions.push(match active_console {
            Some(s) => SessionAction::StartAgent(s),
            None => SessionAction::StopAgent,
        });
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::SessionAction::*;
    use super::SessionChange::*;
    use super::*;

    #[test]
    fn console_session_mapping() {
        assert_eq!(console_session(NO_CONSOLE_SESSION), None);
        assert_eq!(console_session(0), None);
        assert_eq!(console_session(1), Some(1));
    }

    #[test]
    fn boot_logon_lock_unlock() {
        let mut t = SessionTracker::new();
        assert_eq!(t.start(Some(1)), vec![StartAgent(1)]);
        // User logs on at the login screen: restart in the same session.
        assert_eq!(t.on_change(Logon, 1, Some(1)), vec![RestartAgent(1)]);
        assert_eq!(t.on_change(Lock, 1, Some(1)), vec![LockChanged(true)]);
        assert!(t.locked());
        // Duplicate lock notifications are idempotent.
        assert_eq!(t.on_change(Lock, 1, Some(1)), vec![]);
        assert_eq!(t.on_change(Unlock, 1, Some(1)), vec![LockChanged(false)]);
        assert_eq!(t.target(), Some(1));
    }

    #[test]
    fn service_started_without_console_waits_for_connect() {
        let mut t = SessionTracker::new();
        assert_eq!(t.start(None), vec![]);
        assert_eq!(t.on_change(ConsoleConnect, 1, Some(1)), vec![StartAgent(1)]);
    }

    #[test]
    fn logoff_moves_to_the_new_login_session() {
        let mut t = SessionTracker::new();
        t.start(Some(1));
        // Logoff while the session is still the console: restart there first...
        assert_eq!(t.on_change(Logoff, 1, Some(1)), vec![RestartAgent(1)]);
        // ...then the session ends and Windows attaches a fresh session to the console.
        assert_eq!(t.on_change(ConsoleDisconnect, 1, None), vec![StopAgent]);
        assert_eq!(t.on_change(Terminate, 1, None), vec![]);
        assert_eq!(t.on_change(ConsoleConnect, 2, Some(2)), vec![StartAgent(2)]);
        assert_eq!(t.target(), Some(2));
    }

    #[test]
    fn fast_user_switching_follows_the_console() {
        let mut t = SessionTracker::new();
        t.start(Some(1));
        t.on_change(Lock, 1, Some(1));
        // "Switch user": session 1 leaves the console, session 3 (new login) takes it.
        let a = t.on_change(ConsoleDisconnect, 1, None);
        assert_eq!(a, vec![LockChanged(false), StopAgent]);
        assert_eq!(t.on_change(ConsoleConnect, 3, Some(3)), vec![StartAgent(3)]);
        assert_eq!(t.on_change(Logon, 3, Some(3)), vec![RestartAgent(3)]);
        // Switching back to the first user (still logged on, locked).
        t.on_change(ConsoleDisconnect, 3, None);
        assert_eq!(t.on_change(ConsoleConnect, 1, Some(1)), vec![StartAgent(1)]);
        // Notifications about a session that is not the console never touch the agent.
        assert_eq!(t.on_change(Lock, 3, Some(1)), vec![]);
        assert_eq!(t.on_change(Logoff, 3, Some(1)), vec![]);
        assert!(!t.locked());
        // The service queries the session and learns it is still locked.
        assert_eq!(t.set_locked(true), Some(LockChanged(true)));
        assert_eq!(t.set_locked(true), None);
    }

    #[test]
    fn missed_notification_is_repaired_by_the_console_query() {
        let mut t = SessionTracker::new();
        t.start(Some(1));
        // We never saw the disconnect/connect pair, but the console is now session 5.
        assert_eq!(t.on_change(Unlock, 5, Some(5)), vec![StartAgent(5)]);
    }

    #[test]
    fn remote_desktop_takes_the_session_off_the_console() {
        let mut t = SessionTracker::new();
        t.start(Some(1));
        // RDP into the console user's session: it leaves the console (agent stops), and the
        // physical console shows a new login session.
        assert_eq!(t.on_change(RemoteConnect, 1, None), vec![StopAgent]);
        assert_eq!(t.on_change(ConsoleConnect, 2, Some(2)), vec![StartAgent(2)]);
        assert_eq!(t.on_change(RemoteDisconnect, 1, Some(2)), vec![]);
    }

    #[test]
    fn logon_clears_a_stale_lock() {
        let mut t = SessionTracker::new();
        t.start(Some(1));
        t.on_change(Lock, 1, Some(1));
        assert_eq!(
            t.on_change(Logon, 1, Some(1)),
            vec![RestartAgent(1), LockChanged(false)]
        );
    }
}
