//! Privacy mode, host side (M3): on the viewer's request (`Message::PrivacyMode`, needs
//! `PRIVACY_MODE`) blank the host's physical screens and block its physical keyboard and
//! mouse, while the viewer keeps seeing and controlling the real desktop.
//!
//! Register a [`PrivacyFeature`] in `HostConfig::features`. It is a session [`Feature`]:
//!
//! * `PrivacyMode { enabled }` from a viewer is answered with `PrivacyMode { enabled: actual }`;
//!   every session is told whenever the actual state changes.
//! * **Auto-off**: when the session ends (disconnect, network loss, host shutdown, the
//!   session task dying) or `PRIVACY_MODE` is revoked live, its request is withdrawn; the
//!   screen comes back as soon as no session wants privacy any more.
//! * **Watchdog**: the overlay windows and input hooks belong to a thread of this process
//!   (`tlp_desk_input::PrivacyShield`), so if the process dies Windows removes both. Nothing
//!   persists.
//! * **Local emergency exit**: holding Ctrl+Alt+Shift+P for 3 s at the host, or pressing
//!   Ctrl+Alt+Del, ends privacy mode for everyone (logged as a warning, viewers told).
//! * **Who is connected**: the host's normal banner is hidden under the overlay, so the
//!   overlay itself lists every connected viewer (text from [`PrivacyTextFn`]).
//! * Local input priority is suspended while input is blocked (see
//!   `tlp_desk_input::local_input_blocked`).
//!
//! UI events: [`PrivacyStatus`] as the payload of a feature event named [`FEATURE_NAME`].

mod controller;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use tlp_desk_input::{InputError, PrivacyScreenText, PrivacyShield, ShieldSignal, ShieldSignalFn};
use tlp_desk_net::{Feature, FeatureCtx};
use tlp_desk_proto::{Message, Permissions};
use tokio::sync::mpsc;

use controller::Controller;

/// [`Feature::name`] of privacy mode, also the `FeatureEvent::feature` of its UI events.
pub const FEATURE_NAME: &str = "privacy";

/// Longest viewer name put on the privacy screen, in characters.
const MAX_NAME_CHARS: usize = 64;

/// A connected viewer as the privacy screen shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivacyViewer {
    /// Self-reported device name (sanitized: no control or bidi-override characters, capped).
    pub name: String,
    /// Authenticated 9-digit device ID, formatted `123 456 789`.
    pub device_id: String,
}

/// Builds the privacy-screen text for the currently connected viewers (localized by the
/// caller: the app uses its i18n table, the CLI its own strings).
pub type PrivacyTextFn = Arc<dyn Fn(&[PrivacyViewer]) -> PrivacyScreenText + Send + Sync>;

/// Creates the OS shield (default: `tlp_desk_input::start_privacy_shield`). Tests pass fakes.
pub type ShieldFactory =
    Arc<dyn Fn(ShieldSignalFn) -> Result<Box<dyn PrivacyShield>, InputError> + Send + Sync>;

/// Why the privacy state changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivacyCause {
    /// A viewer turned it on.
    Requested,
    /// The viewer(s) turned it off.
    Withdrawn,
    /// The last requesting session ended.
    SessionEnded,
    /// The host user revoked `PRIVACY_MODE`.
    PermissionRevoked,
    /// The host user held Ctrl+Alt+Shift+P.
    EmergencyExit,
    /// Ctrl+Alt+Del, the lock screen or a UAC prompt: the secure desktop cannot be covered.
    SecureDesktop,
    /// The shield stopped unexpectedly.
    Failed,
    /// The request was refused (no permission, or this host cannot do privacy mode).
    Refused,
}

/// Payload of the host privacy feature's UI events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrivacyStatus {
    /// Whether the host screen is blanked and its local input blocked.
    pub active: bool,
    /// Why it changed.
    pub cause: PrivacyCause,
}

fn lock(c: &Mutex<Controller>) -> MutexGuard<'_, Controller> {
    c.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The privacy-mode session feature (host side). Cheap to clone; all clones share one
/// shield, because there is only one physical screen.
#[derive(Clone)]
pub struct PrivacyFeature {
    ctl: Arc<Mutex<Controller>>,
}

impl std::fmt::Debug for PrivacyFeature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrivacyFeature")
            .field("active", &self.is_active())
            .finish_non_exhaustive()
    }
}

impl PrivacyFeature {
    /// Privacy mode with the platform shield (Windows; refused elsewhere).
    pub fn new(text: PrivacyTextFn) -> Self {
        Self::with_shield_factory(text, Arc::new(tlp_desk_input::start_privacy_shield))
    }

    /// Privacy mode with a custom shield (tests, other platforms).
    pub fn with_shield_factory(text: PrivacyTextFn, factory: ShieldFactory) -> Self {
        let ctl = Arc::new_cyclic(|weak: &Weak<Mutex<Controller>>| {
            let weak = weak.clone();
            // Called on the shield's own thread: never block it, never call back into the
            // shield from it (releasing joins that very thread). A short-lived thread takes
            // the controller lock instead.
            let on_signal: ShieldSignalFn = Arc::new(move |signal: ShieldSignal| {
                let weak = weak.clone();
                let spawned = std::thread::Builder::new()
                    .name("tlp-desk-privacy-signal".into())
                    .spawn(move || {
                        if let Some(ctl) = weak.upgrade() {
                            lock(&ctl).force_off(signal);
                        }
                    });
                if let Err(e) = spawned {
                    tracing::error!(%e, ?signal, "cannot handle privacy shield signal");
                }
            });
            Mutex::new(Controller::new(factory, text, on_signal))
        });
        Self { ctl }
    }

    /// Whether the host screen is currently private.
    pub fn is_active(&self) -> bool {
        lock(&self.ctl).is_active()
    }
}

impl Feature for PrivacyFeature {
    fn name(&self) -> &'static str {
        FEATURE_NAME
    }

    fn handles(&self, msg: &Message) -> bool {
        matches!(msg, Message::PrivacyMode { .. })
    }

    fn start(&self, ctx: FeatureCtx, inbox: mpsc::UnboundedReceiver<Message>) {
        tokio::spawn(run_session(self.ctl.clone(), ctx, inbox));
    }
}

/// Clean, capped viewer name for the privacy screen.
fn screen_name(name: &str, device_id: &str) -> String {
    let n: String = tlp_desk_input::privacy::clean_line(name)
        .chars()
        .take(MAX_NAME_CHARS)
        .collect();
    if n.is_empty() {
        device_id.to_owned()
    } else {
        n
    }
}

/// Withdraws the session from the controller however its task ends (normal end, abort or
/// panic), so privacy mode can never outlive the session that asked for it.
struct Registration {
    ctl: Arc<Mutex<Controller>>,
    key: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        let ctl = self.ctl.clone();
        let key = self.key;
        // Releasing the shield joins its thread (a few ms, at most the 2 s drain): keep that
        // off the async runtime.
        let spawned = std::thread::Builder::new()
            .name("tlp-desk-privacy-end".into())
            .spawn(move || lock(&ctl).unregister(key));
        if spawned.is_err() {
            lock(&self.ctl).unregister(self.key);
        }
    }
}

/// Runs a controller call off the async runtime.
async fn blocking<T: Send + 'static>(
    ctl: &Arc<Mutex<Controller>>,
    f: impl FnOnce(&mut Controller) -> T + Send + 'static,
) -> Option<T> {
    let ctl = ctl.clone();
    tokio::task::spawn_blocking(move || f(&mut lock(&ctl)))
        .await
        .ok()
}

async fn run_session(
    ctl: Arc<Mutex<Controller>>,
    ctx: FeatureCtx,
    mut inbox: mpsc::UnboundedReceiver<Message>,
) {
    let key = ctx.session_key;
    let peer = ctx.session.peer();
    let device_id = peer.device_id.to_string();
    let viewer = PrivacyViewer {
        name: screen_name(&peer.name, &device_id),
        device_id,
    };
    let (tx, mut updates) = mpsc::unbounded_channel();
    // The registration guard exists before the call, so even a lost race unregisters.
    let _registration = Registration {
        ctl: ctl.clone(),
        key,
    };
    let _ = blocking(&ctl, move |c| c.register(key, viewer, tx)).await;
    let mut perms = ctx.permissions.clone();
    let mut perms_open = true;
    loop {
        tokio::select! {
            msg = inbox.recv() => match msg {
                Some(Message::PrivacyMode { enabled }) => {
                    handle_request(&ctl, &ctx, key, enabled).await;
                }
                Some(_) => {}
                // The session ended: `_registration` withdraws the request.
                None => break,
            },
            changed = perms.changed(), if perms_open => {
                if changed.is_err() {
                    perms_open = false;
                } else if !perms.borrow_and_update().contains(Permissions::PRIVACY_MODE) {
                    let _ = blocking(&ctl, move |c| c.revoke(key)).await;
                }
            }
            Some(status) = updates.recv() => {
                tracing::debug!(session = key, ?status, "privacy state sent to viewer");
                if ctx.session.send(&Message::PrivacyMode { enabled: status.active }).await.is_err() {
                    break;
                }
                ctx.emit(FEATURE_NAME, status);
            }
        }
    }
}

async fn handle_request(ctl: &Arc<Mutex<Controller>>, ctx: &FeatureCtx, key: u64, enabled: bool) {
    let reply = |active: bool| Message::PrivacyMode { enabled: active };
    if enabled && !ctx.allowed(Permissions::PRIVACY_MODE) {
        // The host core already filters by permission; a race with a live revoke lands here.
        tracing::warn!(
            session = key,
            "privacy mode refused: permission not granted"
        );
        let active = blocking(ctl, |c| c.is_active()).await.unwrap_or(false);
        let _ = ctx.session.send(&reply(active)).await;
        return;
    }
    tracing::info!(session = key, enabled, "viewer requested privacy mode");
    match blocking(ctl, move |c| c.request(key, enabled)).await {
        // On a change every session (this one included) was notified through `updates`.
        Some(Ok(o)) if o.changed => {}
        Some(Ok(o)) => {
            let _ = ctx.session.send(&reply(o.active)).await;
        }
        Some(Err(e)) => {
            tracing::warn!(session = key, %e, "privacy mode refused");
            let active = blocking(ctl, |c| c.is_active()).await.unwrap_or(false);
            let _ = ctx.session.send(&reply(active)).await;
            ctx.emit(
                FEATURE_NAME,
                PrivacyStatus {
                    active,
                    cause: PrivacyCause::Refused,
                },
            );
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_names_are_clean_and_capped() {
        assert_eq!(
            screen_name("  مكتب\u{202E}-Office\n", "123 456 789"),
            "مكتب-Office"
        );
        assert_eq!(
            screen_name("\u{2066}\u{2069} ", "123 456 789"),
            "123 456 789"
        );
        assert_eq!(
            screen_name(&"ب".repeat(500), "1").chars().count(),
            MAX_NAME_CHARS
        );
    }
}
