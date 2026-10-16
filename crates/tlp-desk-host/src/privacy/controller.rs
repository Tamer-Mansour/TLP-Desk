//! Process-wide privacy state: which sessions asked for privacy mode, and the one shield that
//! covers the host's screens. Synchronous and OS-free (the shield comes from a factory), so
//! every transition is unit-tested with a fake shield.
//!
//! Rules:
//! * The screen is private while **at least one** connected session with `PRIVACY_MODE` wants
//!   it. A session can only withdraw its own request.
//! * Every registered session is told the actual state whenever it changes.
//! * A session that ends, disconnects or loses `PRIVACY_MODE` withdraws its request.
//! * A shield signal (local emergency exit, secure desktop, failure) clears every request.

use std::collections::BTreeMap;

use tlp_desk_input::{InputError, PrivacyScreenText, PrivacyShield, ShieldSignal, ShieldSignalFn};
use tokio::sync::mpsc;

use super::{PrivacyCause, PrivacyStatus, PrivacyTextFn, PrivacyViewer, ShieldFactory};

/// One connected session.
struct Entry {
    viewer: PrivacyViewer,
    wants: bool,
    updates: mpsc::UnboundedSender<PrivacyStatus>,
}

/// Result of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Outcome {
    /// Whether the screen is private now.
    pub active: bool,
    /// The state changed (every session, the requester included, was notified).
    pub changed: bool,
}

pub(crate) struct Controller {
    factory: ShieldFactory,
    text: PrivacyTextFn,
    on_signal: ShieldSignalFn,
    shield: Option<Box<dyn PrivacyShield>>,
    sessions: BTreeMap<u64, Entry>,
    active: bool,
}

impl Controller {
    pub(crate) fn new(
        factory: ShieldFactory,
        text: PrivacyTextFn,
        on_signal: ShieldSignalFn,
    ) -> Self {
        Self {
            factory,
            text,
            on_signal,
            shield: None,
            sessions: BTreeMap::new(),
            active: false,
        }
    }

    /// Whether the screen is private right now.
    pub(crate) fn is_active(&self) -> bool {
        self.active
    }

    /// A session started. If the screen is already private, the overlay text now lists it
    /// too (the "who is connected" rule) and the session is told the state.
    pub(crate) fn register(
        &mut self,
        key: u64,
        viewer: PrivacyViewer,
        updates: mpsc::UnboundedSender<PrivacyStatus>,
    ) {
        if self.active {
            let _ = updates.send(PrivacyStatus {
                active: true,
                cause: PrivacyCause::Requested,
            });
        }
        self.sessions.insert(
            key,
            Entry {
                viewer,
                wants: false,
                updates,
            },
        );
        self.refresh_text();
    }

    /// A session ended (for any reason): its request is withdrawn.
    pub(crate) fn unregister(&mut self, key: u64) {
        if let Some(e) = self.sessions.remove(&key) {
            if e.wants {
                tracing::info!(
                    session = key,
                    "privacy mode request withdrawn: session ended"
                );
            }
        }
        // Only ever turns privacy off here, which cannot fail.
        let _ = self.apply(PrivacyCause::SessionEnded);
        self.refresh_text();
    }

    /// The session asks for privacy mode on or off. The caller has checked `PRIVACY_MODE`.
    pub(crate) fn request(&mut self, key: u64, enable: bool) -> Result<Outcome, InputError> {
        let Some(e) = self.sessions.get_mut(&key) else {
            return Ok(Outcome {
                active: self.active,
                changed: false,
            });
        };
        e.wants = enable;
        let cause = if enable {
            PrivacyCause::Requested
        } else {
            PrivacyCause::Withdrawn
        };
        let before = self.active;
        match self.apply(cause) {
            Ok(()) => Ok(Outcome {
                active: self.active,
                changed: self.active != before,
            }),
            Err(err) => {
                if let Some(e) = self.sessions.get_mut(&key) {
                    e.wants = false;
                }
                Err(err)
            }
        }
    }

    /// The session lost `PRIVACY_MODE`: its request is withdrawn.
    pub(crate) fn revoke(&mut self, key: u64) {
        let had = self
            .sessions
            .get_mut(&key)
            .is_some_and(|e| std::mem::take(&mut e.wants));
        if had {
            tracing::info!(
                session = key,
                "privacy mode request withdrawn: permission revoked"
            );
            let _ = self.apply(PrivacyCause::PermissionRevoked);
        }
    }

    /// The shield turned itself off (emergency exit, secure desktop, failure): every request
    /// is cleared and every session told.
    pub(crate) fn force_off(&mut self, signal: ShieldSignal) {
        let cause = match signal {
            ShieldSignal::EmergencyExit => PrivacyCause::EmergencyExit,
            ShieldSignal::SecureDesktop => PrivacyCause::SecureDesktop,
            ShieldSignal::Failed => PrivacyCause::Failed,
        };
        match signal {
            ShieldSignal::EmergencyExit => tracing::warn!(
                viewers = ?self.names(),
                "privacy mode ended by the local emergency exit (Ctrl+Alt+Shift+P)"
            ),
            ShieldSignal::SecureDesktop => tracing::warn!(
                viewers = ?self.names(),
                "privacy mode ended: the secure desktop (Ctrl+Alt+Del, lock screen or UAC) cannot be covered"
            ),
            ShieldSignal::Failed => {
                tracing::error!(viewers = ?self.names(), "privacy shield stopped unexpectedly")
            }
        }
        for e in self.sessions.values_mut() {
            e.wants = false;
        }
        let _ = self.apply(cause);
    }

    fn names(&self) -> Vec<String> {
        self.sessions
            .values()
            .map(|e| e.viewer.name.clone())
            .collect()
    }

    fn screen_text(&self) -> PrivacyScreenText {
        let viewers: Vec<PrivacyViewer> =
            self.sessions.values().map(|e| e.viewer.clone()).collect();
        (self.text)(&viewers)
    }

    fn refresh_text(&mut self) {
        if !self.active {
            return;
        }
        let text = self.screen_text();
        if let Some(s) = self.shield.as_mut() {
            s.set_text(&text);
        }
    }

    /// Engages or releases the shield to match the requests; notifies on change.
    fn apply(&mut self, cause: PrivacyCause) -> Result<(), InputError> {
        let want = self.sessions.values().any(|e| e.wants);
        let mut result = Ok(());
        if want && !self.active {
            let text = self.screen_text();
            let shield = match self.shield.take() {
                Some(s) => Ok(s),
                None => (self.factory)(self.on_signal.clone()),
            };
            match shield {
                Ok(mut s) => match s.engage(&text) {
                    Ok(()) => {
                        self.shield = Some(s);
                        self.active = true;
                        tracing::info!(viewers = ?self.names(), "privacy mode on: host screen blanked, local input blocked");
                    }
                    Err(e) => {
                        s.release();
                        self.shield = Some(s);
                        tracing::warn!(%e, "privacy mode could not start");
                        result = Err(e);
                    }
                },
                Err(e) => {
                    tracing::warn!(%e, "privacy mode unavailable on this host");
                    result = Err(e);
                }
            }
        } else if !want && self.active {
            if let Some(s) = self.shield.as_mut() {
                s.release();
            }
            self.active = false;
            tracing::info!(
                ?cause,
                "privacy mode off: host screen and local input restored"
            );
        } else {
            return Ok(());
        }
        if result.is_ok() {
            self.notify(PrivacyStatus {
                active: self.active,
                cause,
            });
        }
        result
    }

    fn notify(&self, status: PrivacyStatus) {
        for e in self.sessions.values() {
            let _ = e.updates.send(status);
        }
    }
}

impl Drop for Controller {
    /// Belt and braces: whatever happens, input blocking never outlives the controller.
    fn drop(&mut self) {
        if let Some(s) = self.shield.as_mut() {
            s.release();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tlp_desk_input::ScreenLine;

    /// What the fake shield was asked to do.
    #[derive(Default)]
    struct Log {
        calls: Vec<String>,
        engaged: bool,
        fail_engage: bool,
        texts: Vec<PrivacyScreenText>,
    }

    struct FakeShield(Arc<Mutex<Log>>);

    impl PrivacyShield for FakeShield {
        fn engage(&mut self, text: &PrivacyScreenText) -> Result<(), InputError> {
            let mut l = self.0.lock().unwrap();
            l.calls.push("engage".into());
            if l.fail_engage {
                return Err(InputError::Platform("capture exclusion unsupported".into()));
            }
            l.engaged = true;
            l.texts.push(text.clone());
            Ok(())
        }
        fn set_text(&mut self, text: &PrivacyScreenText) {
            let mut l = self.0.lock().unwrap();
            l.calls.push("text".into());
            l.texts.push(text.clone());
        }
        fn release(&mut self) {
            let mut l = self.0.lock().unwrap();
            if l.engaged {
                l.calls.push("release".into());
            }
            l.engaged = false;
        }
        fn is_engaged(&self) -> bool {
            self.0.lock().unwrap().engaged
        }
    }

    fn text_fn() -> PrivacyTextFn {
        Arc::new(|viewers: &[PrivacyViewer]| PrivacyScreenText {
            lines: std::iter::once(ScreenLine::title("هذا الحاسوب قيد الاستخدام عن بُعد"))
                .chain(
                    viewers
                        .iter()
                        .map(|v| ScreenLine::body(format!("{} ({})", v.name, v.device_id))),
                )
                .collect(),
        })
    }

    fn controller() -> (Controller, Arc<Mutex<Log>>) {
        let log = Arc::new(Mutex::new(Log::default()));
        let l = log.clone();
        let factory: ShieldFactory =
            Arc::new(move |_| Ok(Box::new(FakeShield(l.clone())) as Box<dyn PrivacyShield>));
        (Controller::new(factory, text_fn(), Arc::new(|_| {})), log)
    }

    fn viewer(name: &str) -> PrivacyViewer {
        PrivacyViewer {
            name: name.into(),
            device_id: "123 456 789".into(),
        }
    }

    fn session(c: &mut Controller, key: u64, name: &str) -> mpsc::UnboundedReceiver<PrivacyStatus> {
        let (tx, rx) = mpsc::unbounded_channel();
        c.register(key, viewer(name), tx);
        rx
    }

    fn drain(rx: &mut mpsc::UnboundedReceiver<PrivacyStatus>) -> Vec<PrivacyStatus> {
        let mut v = Vec::new();
        while let Ok(s) = rx.try_recv() {
            v.push(s);
        }
        v
    }

    fn st(active: bool, cause: PrivacyCause) -> PrivacyStatus {
        PrivacyStatus { active, cause }
    }

    #[test]
    fn enable_then_disable_engages_and_releases() {
        let (mut c, log) = controller();
        let mut rx = session(&mut c, 1, "مكتب-Office");
        let o = c.request(1, true).unwrap();
        assert_eq!(
            o,
            Outcome {
                active: true,
                changed: true
            }
        );
        assert!(c.is_active());
        assert_eq!(drain(&mut rx), vec![st(true, PrivacyCause::Requested)]);
        // The overlay names the connected viewer (who-is-connected rule), Arabic intact.
        let shown = log.lock().unwrap().texts[0].clone();
        assert!(shown
            .lines
            .iter()
            .any(|l| l.text == "مكتب-Office (123 456 789)"));
        // Asking again changes nothing.
        assert_eq!(
            c.request(1, true).unwrap(),
            Outcome {
                active: true,
                changed: false
            }
        );
        let o = c.request(1, false).unwrap();
        assert_eq!(
            o,
            Outcome {
                active: false,
                changed: true
            }
        );
        assert_eq!(drain(&mut rx), vec![st(false, PrivacyCause::Withdrawn)]);
        assert_eq!(log.lock().unwrap().calls, vec!["engage", "release"]);
    }

    #[test]
    fn session_end_turns_privacy_off() {
        let (mut c, log) = controller();
        let _rx = session(&mut c, 1, "a");
        c.request(1, true).unwrap();
        c.unregister(1);
        assert!(!c.is_active());
        assert!(!log.lock().unwrap().engaged);
    }

    #[test]
    fn permission_revoke_turns_privacy_off() {
        let (mut c, log) = controller();
        let mut rx = session(&mut c, 7, "a");
        c.request(7, true).unwrap();
        drain(&mut rx);
        c.revoke(7);
        assert!(!c.is_active());
        assert!(!log.lock().unwrap().engaged);
        assert_eq!(
            drain(&mut rx),
            vec![st(false, PrivacyCause::PermissionRevoked)]
        );
        // Revoking a session that never asked is silent.
        c.revoke(7);
        assert!(drain(&mut rx).is_empty());
    }

    #[test]
    fn emergency_exit_clears_every_request_and_tells_everyone() {
        let (mut c, log) = controller();
        let mut a = session(&mut c, 1, "a");
        let mut b = session(&mut c, 2, "b");
        c.request(1, true).unwrap();
        c.request(2, true).unwrap();
        drain(&mut a);
        drain(&mut b);
        c.force_off(ShieldSignal::EmergencyExit);
        assert!(!c.is_active());
        assert!(!log.lock().unwrap().engaged);
        for rx in [&mut a, &mut b] {
            assert_eq!(drain(rx), vec![st(false, PrivacyCause::EmergencyExit)]);
        }
        // Nobody wants it any more: a later withdraw is a no-op, a new request re-engages.
        assert_eq!(
            c.request(2, false).unwrap(),
            Outcome {
                active: false,
                changed: false
            }
        );
        assert!(c.request(1, true).unwrap().active);
        c.force_off(ShieldSignal::SecureDesktop);
        assert_eq!(
            drain(&mut a).last(),
            Some(&st(false, PrivacyCause::SecureDesktop))
        );
    }

    #[test]
    fn privacy_stays_on_while_any_session_wants_it() {
        let (mut c, log) = controller();
        let mut a = session(&mut c, 1, "a");
        let mut b = session(&mut c, 2, "b");
        c.request(1, true).unwrap();
        // B never asked: its "off" does not lift A's privacy.
        assert_eq!(
            c.request(2, false).unwrap(),
            Outcome {
                active: true,
                changed: false
            }
        );
        c.request(2, true).unwrap();
        c.request(1, false).unwrap();
        assert!(c.is_active());
        c.unregister(2);
        assert!(!c.is_active());
        assert_eq!(
            drain(&mut a),
            vec![
                st(true, PrivacyCause::Requested),
                st(false, PrivacyCause::SessionEnded)
            ]
        );
        assert_eq!(drain(&mut b), vec![st(true, PrivacyCause::Requested)]);
        assert_eq!(log.lock().unwrap().calls, vec!["engage", "release"]);
    }

    #[test]
    fn late_joiner_is_listed_and_told() {
        let (mut c, log) = controller();
        let _a = session(&mut c, 1, "first");
        c.request(1, true).unwrap();
        let mut late = session(&mut c, 2, "حاسوب-ثانٍ");
        assert_eq!(drain(&mut late), vec![st(true, PrivacyCause::Requested)]);
        let l = log.lock().unwrap();
        assert_eq!(l.calls, vec!["engage", "text"]);
        let last = l.texts.last().unwrap();
        assert!(last.lines.iter().any(|x| x.text.starts_with("حاسوب-ثانٍ")));
        assert!(last.lines.iter().any(|x| x.text.starts_with("first")));
    }

    #[test]
    fn engage_failure_is_reported_and_leaves_nothing_on() {
        let (mut c, log) = controller();
        log.lock().unwrap().fail_engage = true;
        let mut rx = session(&mut c, 1, "a");
        assert!(c.request(1, true).is_err());
        assert!(!c.is_active());
        assert!(drain(&mut rx).is_empty());
        // The failed request is not remembered: a later success needs a new request.
        log.lock().unwrap().fail_engage = false;
        c.unregister(99);
        assert!(!c.is_active());
        assert!(c.request(1, true).unwrap().active);
    }

    #[test]
    fn unsupported_platform_is_an_error_not_a_panic() {
        let factory: ShieldFactory = Arc::new(|_| Err(InputError::Unsupported));
        let mut c = Controller::new(factory, text_fn(), Arc::new(|_| {}));
        let _rx = session(&mut c, 1, "a");
        assert!(matches!(c.request(1, true), Err(InputError::Unsupported)));
        assert!(!c.is_active());
    }

    #[test]
    fn dropping_the_controller_releases_the_shield() {
        let (mut c, log) = controller();
        let _rx = session(&mut c, 1, "a");
        c.request(1, true).unwrap();
        drop(c);
        assert!(!log.lock().unwrap().engaged);
    }
}
