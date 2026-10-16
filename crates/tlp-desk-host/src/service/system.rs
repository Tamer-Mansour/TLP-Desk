//! The `system` session feature: Ctrl+Alt+Del, lock workstation and the secure-desktop state.
//!
//! Registered in `HostConfig::features` by the service (not in attended mode: without the
//! service, `SendSAS` and the secure desktop are out of reach). The host core already drops
//! `SendCtrlAltDel` / `LockWorkstation` unless the session holds `Permissions::SYSTEM`
//! (`Message::required_permissions`); this feature checks again before acting, because the
//! permission can be revoked between routing and handling.
//!
//! Every session also receives `Message::DesktopState` when it starts and whenever the secure
//! desktop or lock state changes, so the viewer can show "the remote PC is locked / showing a
//! UAC prompt" and offer Ctrl+Alt+Del.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tlp_desk_net::{Feature, FeatureCtx};
use tlp_desk_proto::{Message, Permissions};
use tokio::sync::{mpsc, watch};

use super::DesktopStatus;
use crate::HostError;

/// [`Feature::name`] of [`SystemControl`] (and of its [`SystemEvent`]s).
pub const SYSTEM_FEATURE: &str = "system";

/// Least time between two system actions of one session (a viewer holding the button must
/// not fire `SendSAS` dozens of times).
pub const MIN_ACTION_INTERVAL: Duration = Duration::from_secs(1);

/// The privileged operations, implemented by the service (Windows) or a fake (tests).
pub trait SystemActions: Send + Sync + 'static {
    /// Secure attention sequence (`SendSAS`), as if Ctrl+Alt+Del was pressed on the host.
    fn send_ctrl_alt_del(&self) -> Result<(), HostError>;
    /// Locks the console session (`LockWorkStation` in the session agent).
    fn lock_workstation(&self) -> Result<(), HostError>;
}

/// A system action, as reported to the UI ([`SystemEvent`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemAction {
    CtrlAltDel,
    LockWorkstation,
}

/// Payload of `HostEvent::Feature` from [`SystemControl`] (for the log / banner).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemEvent {
    pub action: SystemAction,
    /// `Err` holds the error text (for the log only).
    pub result: Result<(), String>,
}

/// What to do with a message routed to the feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    Run(SystemAction),
    /// The session lacks `SYSTEM` (revoked after routing).
    Denied,
    /// Too soon after the previous action.
    Throttled,
    /// Not a system message.
    Ignore,
}

/// Pure gate: permission, throttle, and which action.
pub(crate) fn decide(
    msg: &Message,
    perms: Permissions,
    last: Option<Instant>,
    now: Instant,
) -> Decision {
    let action = match msg {
        Message::SendCtrlAltDel => SystemAction::CtrlAltDel,
        Message::LockWorkstation => SystemAction::LockWorkstation,
        _ => return Decision::Ignore,
    };
    if !perms.contains(Permissions::SYSTEM) {
        return Decision::Denied;
    }
    if last.is_some_and(|t| now.saturating_duration_since(t) < MIN_ACTION_INTERVAL) {
        return Decision::Throttled;
    }
    Decision::Run(action)
}

/// The `system` feature (see the module docs).
#[derive(Clone)]
pub struct SystemControl {
    actions: Arc<dyn SystemActions>,
    desktop: watch::Receiver<DesktopStatus>,
}

impl std::fmt::Debug for SystemControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemControl")
            .field("desktop", &*self.desktop.borrow())
            .finish_non_exhaustive()
    }
}

impl SystemControl {
    /// `desktop` carries the current secure-desktop / lock state (the service updates it).
    pub fn new(actions: Arc<dyn SystemActions>, desktop: watch::Receiver<DesktopStatus>) -> Self {
        Self { actions, desktop }
    }
}

impl Feature for SystemControl {
    fn name(&self) -> &'static str {
        SYSTEM_FEATURE
    }

    fn handles(&self, msg: &Message) -> bool {
        matches!(msg, Message::SendCtrlAltDel | Message::LockWorkstation)
    }

    fn start(&self, ctx: FeatureCtx, inbox: mpsc::UnboundedReceiver<Message>) {
        tokio::spawn(run(ctx, inbox, self.actions.clone(), self.desktop.clone()));
    }
}

async fn run(
    ctx: FeatureCtx,
    mut inbox: mpsc::UnboundedReceiver<Message>,
    actions: Arc<dyn SystemActions>,
    mut desktop: watch::Receiver<DesktopStatus>,
) {
    let initial = *desktop.borrow_and_update();
    if ctx.session.send(&initial.to_message()).await.is_err() {
        return;
    }
    let mut last: Option<Instant> = None;
    let mut desktop_open = true;
    loop {
        tokio::select! {
            changed = desktop.changed(), if desktop_open => {
                if changed.is_err() {
                    desktop_open = false;
                    continue;
                }
                let status = *desktop.borrow_and_update();
                tracing::debug!(session = ctx.session_key, ?status, "desktop state changed");
                if ctx.session.send(&status.to_message()).await.is_err() {
                    break;
                }
            }
            msg = inbox.recv() => {
                let Some(msg) = msg else { break };
                let perms = *ctx.permissions.borrow();
                let now = Instant::now();
                match decide(&msg, perms, last, now) {
                    Decision::Run(action) => {
                        last = Some(now);
                        let result = perform(actions.clone(), action).await;
                        match &result {
                            Ok(()) => tracing::info!(session = ctx.session_key, ?action, "system action done"),
                            Err(e) => tracing::warn!(session = ctx.session_key, ?action, error = %e, "system action failed"),
                        }
                        ctx.emit(SYSTEM_FEATURE, SystemEvent { action, result });
                    }
                    Decision::Denied => tracing::warn!(session = ctx.session_key, "system action refused: SYSTEM permission not held"),
                    Decision::Throttled => tracing::debug!(session = ctx.session_key, "system action throttled"),
                    Decision::Ignore => {}
                }
            }
        }
    }
}

/// Runs the action off the async runtime (it calls into the OS / the agent pipe).
async fn perform(actions: Arc<dyn SystemActions>, action: SystemAction) -> Result<(), String> {
    let res = tokio::task::spawn_blocking(move || match action {
        SystemAction::CtrlAltDel => actions.send_ctrl_alt_del(),
        SystemAction::LockWorkstation => actions.lock_workstation(),
    })
    .await;
    match res {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(e) => Err(format!("system action task failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_requires_system_permission() {
        let now = Instant::now();
        let all = Permissions::VIEW | Permissions::SYSTEM;
        assert_eq!(
            decide(&Message::SendCtrlAltDel, all, None, now),
            Decision::Run(SystemAction::CtrlAltDel)
        );
        assert_eq!(
            decide(&Message::LockWorkstation, all, None, now),
            Decision::Run(SystemAction::LockWorkstation)
        );
        let no_system = Permissions::VIEW | Permissions::CONTROL | Permissions::KEYBOARD;
        assert_eq!(
            decide(&Message::SendCtrlAltDel, no_system, None, now),
            Decision::Denied
        );
        assert_eq!(
            decide(&Message::RequestKeyframe, all, None, now),
            Decision::Ignore
        );
    }

    #[test]
    fn gate_throttles_bursts() {
        let t0 = Instant::now();
        let p = Permissions::VIEW | Permissions::SYSTEM;
        assert_eq!(
            decide(
                &Message::SendCtrlAltDel,
                p,
                Some(t0),
                t0 + Duration::from_millis(200)
            ),
            Decision::Throttled
        );
        assert_eq!(
            decide(
                &Message::SendCtrlAltDel,
                p,
                Some(t0),
                t0 + MIN_ACTION_INTERVAL
            ),
            Decision::Run(SystemAction::CtrlAltDel)
        );
    }

    #[test]
    fn the_core_also_requires_system_for_these_messages() {
        // Defense in depth: the host core routes only with this permission.
        assert_eq!(
            Message::SendCtrlAltDel.required_permissions(),
            Permissions::SYSTEM
        );
        assert_eq!(
            Message::LockWorkstation.required_permissions(),
            Permissions::SYSTEM
        );
    }
}
