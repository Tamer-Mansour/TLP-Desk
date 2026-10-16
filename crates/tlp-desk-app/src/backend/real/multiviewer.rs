//! Real backend, several viewers at once (TLP-multiviewer): the host user mutes one viewer's
//! input or makes it view-only from the banner while the others keep control.

use tlp_desk_proto::Permissions;

use super::Inner;
use crate::backend::SessionId;

/// What "view only" takes away: the mouse and the keyboard.
const INPUT: Permissions = Permissions(Permissions::CONTROL.0 | Permissions::KEYBOARD.0);

impl Inner {
    /// Pauses or resumes one viewer's input (permissions unchanged). No-op for an unknown
    /// session.
    pub(super) fn set_input_paused(&self, session: SessionId, paused: bool) {
        let Some(id) = self.state().host_sessions.get(&session).copied() else {
            return;
        };
        let Some(host) = &self.host else { return };
        tracing::info!(session = session.0, paused, "host user paused viewer input");
        host.set_input_paused(id, paused);
    }

    /// Takes mouse and keyboard away from one viewer, or gives both back. The change is
    /// pushed to the viewer and confirmed by the host's `PermissionsChanged`.
    pub(super) fn set_view_only(&self, session: SessionId, view_only: bool) {
        let Some(id) = self.state().host_sessions.get(&session).copied() else {
            return;
        };
        let Some(host) = &self.host else { return };
        let Some(current) = host
            .sessions()
            .into_iter()
            .find(|v| v.id == id)
            .map(|v| v.permissions)
        else {
            return;
        };
        let new = if view_only {
            current.difference(INPUT)
        } else {
            current.union(INPUT)
        };
        if new != current {
            tracing::info!(
                session = session.0,
                view_only,
                "host user changed view-only"
            );
            host.set_permissions(id, new);
        }
    }
}
