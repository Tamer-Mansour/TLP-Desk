//! Real backend, unattended access (TLP-unattended): permanent password, remote system
//! actions, and reconnecting after a remote restart under the same [`SessionId`].

use std::sync::Arc;

use tlp_desk_crypto::{password_strength, PasswordStrength, PasswordVerifier};
use tlp_desk_viewer::{AttemptFn, ReconnectPolicy, ViewerError, ViewerEvents, ViewerSession};

use super::{lock, map_viewer_error, Bridge, Inner};
use crate::backend::unattended::{
    is_elevated, PasswordChange, PasswordError, PasswordState, SystemAction,
};
use crate::backend::{BackendEvent, EndReason, FrameSlot, SessionId};

impl Inner {
    pub(super) fn password_state(&self) -> PasswordState {
        self.state().password
    }

    fn set_password_state(&self, f: impl FnOnce(&mut PasswordState)) {
        let st = {
            let mut s = self.state();
            f(&mut s.password);
            s.password
        };
        self.emit(BackendEvent::PasswordState(st));
    }

    /// Startup: find out whether we run elevated (blocking `whoami`, off the runtime).
    pub(super) async fn probe_elevation(self: Arc<Self>) {
        let admin = tokio::task::spawn_blocking(is_elevated)
            .await
            .unwrap_or(false);
        self.set_password_state(|p| p.admin = admin);
    }

    pub(super) async fn change_password(
        self: Arc<Self>,
        current: Option<String>,
        new: Option<String>,
    ) {
        let result = self.clone().change_password_inner(current, new).await;
        if let Err(e) = &result {
            tracing::warn!(?e, "permanent password not changed");
        }
        self.emit(BackendEvent::PasswordChanged(result));
    }

    async fn change_password_inner(
        self: Arc<Self>,
        current: Option<String>,
        new: Option<String>,
    ) -> Result<PasswordChange, PasswordError> {
        let (Some(store), Some(host)) = (self.password_store.clone(), self.host.as_ref()) else {
            return Err(PasswordError::Unavailable);
        };
        let fp = host.fingerprint();
        if let Some(n) = &new {
            if password_strength(n) == PasswordStrength::TooShort {
                return Err(PasswordError::TooShort);
            }
        }
        let st = self.password_state();
        let need_current = st.set && !st.admin;
        let check_store = store.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            if need_current {
                let ok = match &current {
                    Some(c) => check_store
                        .check(c, &fp)
                        .map_err(|e| PasswordError::Save(e.to_string()))?,
                    None => false,
                };
                if !ok {
                    return Err(PasswordError::WrongCurrent);
                }
            }
            match new {
                Some(n) => {
                    let v = PasswordVerifier::derive(&n, &fp)
                        .map_err(|e| PasswordError::Save(e.to_string()))?;
                    check_store
                        .save(&v)
                        .map_err(|e| PasswordError::Save(e.to_string()))?;
                    Ok(Some(v))
                }
                None => {
                    check_store
                        .clear()
                        .map_err(|e| PasswordError::Save(e.to_string()))?;
                    Ok(None)
                }
            }
        })
        .await
        .map_err(|e| PasswordError::Save(e.to_string()))??;
        let set = outcome.is_some();
        host.set_password(outcome);
        tracing::info!(set, "permanent unattended password changed");
        self.set_password_state(|p| p.set = set);
        Ok(if set {
            PasswordChange::Set
        } else {
            PasswordChange::Removed
        })
    }

    pub(super) fn system_action(&self, session: SessionId, action: SystemAction) {
        let s = self.state().viewer_sessions.get(&session).cloned();
        let Some(s) = s else { return };
        if !s.permissions().contains(action.required()) {
            tracing::info!(
                session = session.0,
                ?action,
                "system action not permitted by the host"
            );
            return;
        }
        tracing::info!(session = session.0, ?action, "system action requested");
        s.send_message(action.message());
    }

    /// Called when a viewer session ended. Returns `true` if a reconnect started instead of
    /// ending the session for the UI (the host restarted and left us a ticket).
    pub(super) fn try_start_reconnect(
        self: &Arc<Self>,
        session: SessionId,
        old: &ViewerSession,
        reason: &tlp_desk_viewer::EndReason,
        dst: Option<Arc<FrameSlot>>,
    ) -> bool {
        if matches!(reason, tlp_desk_viewer::EndReason::Local) {
            self.keeper.discard(old.feature_key());
            return false;
        }
        let (Some(ticket), Some(dst)) = (self.keeper.take(old.feature_key()), dst) else {
            return false;
        };
        tracing::info!(session = session.0, host = %ticket.host, "remote PC restarting; waiting to reconnect");
        let me = Arc::clone(self);
        let mut st = self.state();
        let handle = self
            .rt
            .spawn(async move {
                let Some(viewer) = &me.viewer else { return };
                let progress = Arc::downgrade(&me);
                let on_attempt: AttemptFn = Arc::new(move |attempt| {
                    if let Some(me) = progress.upgrade() {
                        me.emit(BackendEvent::ReconnectAttempt { session, attempt });
                    }
                });
                let result = viewer
                    .reconnect_with_ticket(ticket, ReconnectPolicy::default(), Some(on_attempt))
                    .await;
                me.reconnect_done(session, dst, result);
            })
            .abort_handle();
        st.reconnects.insert(session, handle);
        drop(st);
        self.emit(BackendEvent::RemoteRestarting { session });
        true
    }

    fn reconnect_done(
        self: &Arc<Self>,
        session: SessionId,
        dst: Arc<FrameSlot>,
        result: Result<(ViewerSession, ViewerEvents), ViewerError>,
    ) {
        if self.state().reconnects.remove(&session).is_none() {
            return; // cancelled
        }
        match result {
            Ok((vs, events)) => {
                let vs = Arc::new(vs);
                let permissions = vs.permissions();
                self.state().viewer_sessions.insert(session, vs.clone());
                lock(&self.bridges).push(Bridge {
                    session,
                    src: vs.frames(),
                    dst,
                });
                tracing::info!(session = session.0, "reconnected after the remote restart");
                self.emit(BackendEvent::Reconnected {
                    session,
                    permissions,
                });
                self.rt.spawn(self.clone().pump_viewer(session, events));
            }
            Err(e) => {
                tracing::warn!(session = session.0, %e, "remote PC did not come back");
                self.emit(BackendEvent::ReconnectFailed {
                    session,
                    error: map_viewer_error(&e),
                });
                self.emit(BackendEvent::SessionEnded {
                    session,
                    reason: EndReason::Error(e.to_string()),
                });
            }
        }
    }

    /// Returns `true` if `session` was waiting for a reconnect (now cancelled).
    pub(super) fn cancel_reconnect(&self, session: SessionId) -> bool {
        let Some(h) = self.state().reconnects.remove(&session) else {
            return false;
        };
        h.abort();
        self.emit(BackendEvent::SessionEnded {
            session,
            reason: EndReason::Local,
        });
        true
    }
}
