//! Following the input desktop: `Default` (the user's desktop) and the secure desktop
//! `Winlogon` (lock screen, login screen, Ctrl+Alt+Del screen, UAC prompts on the secure
//! desktop), plus `Screen-saver`.
//!
//! A thread can capture and inject input only on the desktop it is attached to, and Windows
//! switches the *input* desktop without telling anyone. The agent therefore polls
//! `OpenInputDesktop` (cheap) and, when the name changes, re-attaches its capture/input
//! threads with `SetThreadDesktop` and re-creates the capturer and injector
//! ([`DesktopFollower`]); a separate [`DesktopWatcher`] reports changes to the service, which
//! tells viewers with `Message::DesktopState`.
//!
//! The OS calls are behind [`DesktopSource`] (platform module), so the logic is tested with a
//! fake desktop source.

use std::time::{Duration, Instant};

use tlp_desk_proto::Message;

use crate::HostError;

/// Name of the normal interactive desktop.
pub const DEFAULT_DESKTOP: &str = "Default";

/// Name reported when the input desktop cannot be opened at all (only SYSTEM may open
/// `Winlogon`, so for anyone else this means "a secure desktop is showing").
pub const INACCESSIBLE_DESKTOP: &str = "<inaccessible>";

/// Whether a desktop name is a secure desktop (anything but `Default`).
pub fn is_secure_desktop(name: &str) -> bool {
    !name.eq_ignore_ascii_case(DEFAULT_DESKTOP)
}

/// Access to the input desktop of the current session.
pub trait DesktopSource: Send {
    /// Name of the desktop currently receiving user input.
    fn input_desktop(&mut self) -> Result<String, HostError>;

    /// Attaches the **calling thread** to the current input desktop (`SetThreadDesktop`) and
    /// returns its name. Fails if the thread owns windows or hooks on its current desktop.
    fn attach_thread(&mut self) -> Result<String, HostError>;
}

/// A change of the input desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopChange {
    pub name: String,
    pub secure: bool,
}

/// Reports input-desktop changes (agent -> service events).
#[derive(Debug)]
pub struct DesktopWatcher<S> {
    source: S,
    current: Option<String>,
}

impl<S: DesktopSource> DesktopWatcher<S> {
    pub fn new(source: S) -> Self {
        Self {
            source,
            current: None,
        }
    }

    /// The last desktop seen.
    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// Reads the input desktop; `Some` on the first call and whenever it changed. A desktop
    /// that cannot be opened counts as secure ([`INACCESSIBLE_DESKTOP`]).
    pub fn poll(&mut self) -> Option<DesktopChange> {
        let name = match self.source.input_desktop() {
            Ok(n) => n,
            Err(e) => {
                tracing::debug!(%e, "cannot open the input desktop; assuming a secure desktop");
                INACCESSIBLE_DESKTOP.to_owned()
            }
        };
        if self
            .current
            .as_deref()
            .is_some_and(|c| c.eq_ignore_ascii_case(&name))
        {
            return None;
        }
        let secure = is_secure_desktop(&name);
        self.current = Some(name.clone());
        Some(DesktopChange { name, secure })
    }
}

/// Keeps the calling thread on the input desktop: [`DesktopFollower::check`] is called before
/// each capture/inject step and says when the thread moved (the caller then re-creates its
/// capturer or injector, which belong to the old desktop).
#[derive(Debug)]
pub struct DesktopFollower<S> {
    source: S,
    attached: Option<String>,
    last_check: Option<Instant>,
    every: Duration,
}

impl<S: DesktopSource> DesktopFollower<S> {
    /// Re-checks the input desktop at most once per `every` (it is a system call per check).
    pub fn new(source: S, every: Duration) -> Self {
        Self {
            source,
            attached: None,
            last_check: None,
            every,
        }
    }

    /// The desktop the thread is attached to.
    pub fn attached(&self) -> Option<&str> {
        self.attached.as_deref()
    }

    /// Whether the thread is on a secure desktop.
    pub fn on_secure_desktop(&self) -> bool {
        self.attached.as_deref().is_some_and(is_secure_desktop)
    }

    /// Attaches the thread on the first call; afterwards re-attaches if the input desktop
    /// changed. `Ok(Some(name))` means "moved to `name`: re-create what you hold".
    pub fn check(&mut self, now: Instant) -> Result<Option<String>, HostError> {
        if self.attached.is_some() {
            if self
                .last_check
                .is_some_and(|t| now.saturating_duration_since(t) < self.every)
            {
                return Ok(None);
            }
            self.last_check = Some(now);
            match self.source.input_desktop() {
                Ok(name)
                    if self
                        .attached
                        .as_deref()
                        .is_some_and(|a| a.eq_ignore_ascii_case(&name)) =>
                {
                    return Ok(None)
                }
                Ok(_) => {}
                // Cannot even read the name: keep what we have and try again next time.
                Err(e) => {
                    tracing::debug!(%e, "input desktop check failed");
                    return Ok(None);
                }
            }
        }
        self.last_check = Some(now);
        let name = self.source.attach_thread()?;
        tracing::info!(desktop = %name, "thread attached to the input desktop");
        self.attached = Some(name.clone());
        Ok(Some(name))
    }

    /// Forces a re-attach on the next [`DesktopFollower::check`] (e.g. after the capturer
    /// reported a lost surface).
    pub fn invalidate(&mut self) {
        self.last_check = None;
        if let Some(a) = self.attached.as_mut() {
            // Any name that cannot match makes the next check re-attach.
            a.clear();
        }
    }
}

/// What viewers are told about the host's desktop (`Message::DesktopState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DesktopStatus {
    /// A secure desktop (lock/login screen, UAC, Ctrl+Alt+Del screen) receives input.
    pub secure_desktop: bool,
    /// The console session is locked.
    pub locked: bool,
}

impl DesktopStatus {
    /// The v4 wire message.
    pub fn to_message(self) -> Message {
        Message::DesktopState {
            secure_desktop: self.secure_desktop,
            locked: self.locked,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Shared fake: the "input desktop" the OS would report, and the thread's desktop.
    #[derive(Default)]
    struct State {
        input: String,
        thread: String,
        attach_calls: u32,
        reads: u32,
        fail_read: bool,
        fail_attach: bool,
    }

    #[derive(Clone)]
    struct Fake(Arc<Mutex<State>>);

    impl Fake {
        fn new(input: &str) -> Self {
            Self(Arc::new(Mutex::new(State {
                input: input.into(),
                ..State::default()
            })))
        }
        fn switch(&self, to: &str) {
            self.0.lock().unwrap().input = to.into();
        }
    }

    impl DesktopSource for Fake {
        fn input_desktop(&mut self) -> Result<String, HostError> {
            let mut s = self.0.lock().unwrap();
            s.reads += 1;
            if s.fail_read {
                return Err(HostError::Os {
                    call: "OpenInputDesktop",
                    code: 5,
                });
            }
            Ok(s.input.clone())
        }
        fn attach_thread(&mut self) -> Result<String, HostError> {
            let mut s = self.0.lock().unwrap();
            if s.fail_attach {
                return Err(HostError::Os {
                    call: "SetThreadDesktop",
                    code: 170,
                });
            }
            s.attach_calls += 1;
            s.thread = s.input.clone();
            Ok(s.thread.clone())
        }
    }

    #[test]
    fn secure_desktop_names() {
        assert!(!is_secure_desktop("Default"));
        assert!(!is_secure_desktop("default"));
        assert!(is_secure_desktop("Winlogon"));
        assert!(is_secure_desktop("Screen-saver"));
        assert!(is_secure_desktop(INACCESSIBLE_DESKTOP));
    }

    #[test]
    fn watcher_reports_lock_uac_and_back() {
        let fake = Fake::new("Default");
        let mut w = DesktopWatcher::new(fake.clone());
        assert_eq!(
            w.poll(),
            Some(DesktopChange {
                name: "Default".into(),
                secure: false
            })
        );
        assert_eq!(w.poll(), None);
        // UAC prompt on the secure desktop.
        fake.switch("Winlogon");
        assert_eq!(
            w.poll(),
            Some(DesktopChange {
                name: "Winlogon".into(),
                secure: true
            })
        );
        assert_eq!(w.poll(), None);
        // Case differences are not changes.
        fake.switch("WINLOGON");
        assert_eq!(w.poll(), None);
        fake.switch("Default");
        assert_eq!(w.poll().map(|c| c.secure), Some(false));
        assert_eq!(w.current(), Some("Default"));
    }

    #[test]
    fn unreadable_desktop_counts_as_secure() {
        let fake = Fake::new("Default");
        let mut w = DesktopWatcher::new(fake.clone());
        w.poll();
        fake.0.lock().unwrap().fail_read = true;
        assert_eq!(
            w.poll(),
            Some(DesktopChange {
                name: INACCESSIBLE_DESKTOP.into(),
                secure: true
            })
        );
        assert_eq!(w.poll(), None);
    }

    #[test]
    fn follower_attaches_once_then_on_switch_only() {
        let fake = Fake::new("Default");
        let every = Duration::from_millis(200);
        let mut f = DesktopFollower::new(fake.clone(), every);
        let t0 = Instant::now();
        assert_eq!(f.check(t0).unwrap(), Some("Default".into()));
        assert!(!f.on_secure_desktop());
        // Within the interval: no system call at all.
        let reads = fake.0.lock().unwrap().reads;
        fake.switch("Winlogon");
        assert_eq!(f.check(t0 + Duration::from_millis(50)).unwrap(), None);
        assert_eq!(fake.0.lock().unwrap().reads, reads);
        // After the interval the switch is noticed and the thread re-attached.
        assert_eq!(f.check(t0 + every).unwrap(), Some("Winlogon".into()));
        assert!(f.on_secure_desktop());
        assert_eq!(fake.0.lock().unwrap().thread, "Winlogon");
        // Stable: no further attach.
        assert_eq!(f.check(t0 + every * 2).unwrap(), None);
        assert_eq!(fake.0.lock().unwrap().attach_calls, 2);
    }

    #[test]
    fn follower_invalidate_forces_reattach() {
        let fake = Fake::new("Default");
        let mut f = DesktopFollower::new(fake.clone(), Duration::from_secs(10));
        let t0 = Instant::now();
        f.check(t0).unwrap();
        f.invalidate();
        assert_eq!(f.check(t0).unwrap(), Some("Default".into()));
        assert_eq!(fake.0.lock().unwrap().attach_calls, 2);
    }

    #[test]
    fn follower_errors_are_reported_and_retried() {
        let fake = Fake::new("Default");
        let mut f = DesktopFollower::new(fake.clone(), Duration::ZERO);
        fake.0.lock().unwrap().fail_attach = true;
        let t0 = Instant::now();
        assert!(f.check(t0).is_err());
        fake.0.lock().unwrap().fail_attach = false;
        assert_eq!(f.check(t0).unwrap(), Some("Default".into()));
        // A failing read keeps the current attachment.
        fake.0.lock().unwrap().fail_read = true;
        assert_eq!(f.check(t0).unwrap(), None);
        assert_eq!(f.attached(), Some("Default"));
    }

    #[test]
    fn status_maps_to_the_v4_message() {
        let s = DesktopStatus {
            secure_desktop: true,
            locked: false,
        };
        assert_eq!(
            s.to_message(),
            Message::DesktopState {
                secure_desktop: true,
                locked: false
            }
        );
    }
}
