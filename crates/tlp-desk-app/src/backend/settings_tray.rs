//! TLP-settings-tray glue for the real backend: applying [`HostSettings`] to the endpoint and
//! host, and mapping host/viewer outcomes to connection-log entries.

use tlp_desk_host::{EndReason as HostEnd, Host, HostConfig};
use tlp_desk_net::Endpoint;
use tlp_desk_proto::{ByeReason, RejectReason};
use tlp_desk_viewer::EndReason as ViewerEnd;

use crate::connection_log::LogEnd;
use crate::settings::HostSettings;

/// `template` with the settings' live options filled in (used before the host starts, so the
/// first connection already sees them).
pub fn host_config(template: HostConfig, settings: &HostSettings) -> HostConfig {
    let live = settings.live_options();
    HostConfig {
        default_permissions: live.default_permissions,
        accept_timeout: live.accept_timeout,
        allow_one_time_code: live.allow_one_time_code,
        local_input_grace: live.local_input_grace,
        remote_pointer_marker: live.remote_pointer_marker,
        ..template
    }
}

/// Apply settings to a running endpoint and host.
pub fn apply(endpoint: Option<&Endpoint>, host: Option<&Host>, settings: &HostSettings) {
    if let Some(ep) = endpoint {
        ep.set_allow_list(settings.effective_allow_list());
    }
    if let Some(h) = host {
        h.set_live_options(settings.live_options());
    }
    tracing::info!(
        incoming = settings.incoming_enabled,
        ranges = settings.allow_list.len(),
        "host settings applied"
    );
}

/// Log outcome of a host session end.
pub fn host_end(reason: &HostEnd) -> (LogEnd, Option<String>) {
    match reason {
        HostEnd::Local => (LogEnd::ClosedHere, None),
        HostEnd::Remote(_) => (LogEnd::ClosedRemotely, None),
        HostEnd::PermissionRevoked => (LogEnd::PermissionRevoked, None),
        HostEnd::ConnectionLost(e) | HostEnd::Error(e) => (LogEnd::Error, Some(e.clone())),
    }
}

/// Log outcome of a viewer session end.
pub fn viewer_end(reason: &ViewerEnd) -> (LogEnd, Option<String>) {
    match reason {
        ViewerEnd::Local => (LogEnd::ClosedHere, None),
        ViewerEnd::Remote(ByeReason::PermissionRevoked) => (LogEnd::PermissionRevoked, None),
        ViewerEnd::Remote(_) => (LogEnd::ClosedRemotely, None),
        ViewerEnd::ConnectionLost(e) | ViewerEnd::Error(e) => (LogEnd::Error, Some(e.clone())),
    }
}

/// Log outcome of a refused incoming attempt.
pub fn refusal(reason: RejectReason) -> LogEnd {
    match reason {
        RejectReason::UserDeclined => LogEnd::Declined,
        RejectReason::WrongCredentials => LogEnd::WrongCredentials,
        RejectReason::RateLimited => LogEnd::RateLimited,
        RejectReason::NotAllowed | RejectReason::VersionMismatch => LogEnd::NotAllowed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tlp_desk_proto::Permissions;

    #[test]
    fn host_config_takes_the_settings() {
        let mut s = HostSettings {
            accept_timeout_secs: 25,
            allow_one_time_code: false,
            local_input_grace_ms: 0,
            remote_pointer_marker: false,
            ..HostSettings::default()
        };
        s.default_permissions.set(Permissions::FILES, true);
        let cfg = host_config(HostConfig::default(), &s);
        assert_eq!(cfg.accept_timeout, Duration::from_secs(25));
        assert!(!cfg.allow_one_time_code);
        assert_eq!(cfg.local_input_grace, Duration::ZERO);
        assert!(!cfg.remote_pointer_marker);
        assert!(cfg.default_permissions.contains(Permissions::FILES));
    }

    #[test]
    fn outcomes_map_to_log_ends() {
        assert_eq!(host_end(&HostEnd::Local), (LogEnd::ClosedHere, None));
        assert_eq!(
            host_end(&HostEnd::ConnectionLost("انقطع".into())),
            (LogEnd::Error, Some("انقطع".into()))
        );
        assert_eq!(
            viewer_end(&ViewerEnd::Remote(ByeReason::PermissionRevoked)).0,
            LogEnd::PermissionRevoked
        );
        assert_eq!(
            viewer_end(&ViewerEnd::Remote(ByeReason::UserClosed)).0,
            LogEnd::ClosedRemotely
        );
        assert_eq!(refusal(RejectReason::UserDeclined), LogEnd::Declined);
        assert_eq!(refusal(RejectReason::VersionMismatch), LogEnd::NotAllowed);
    }
}
