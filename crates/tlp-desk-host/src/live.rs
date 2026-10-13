//! Host options the app may change while the host runs (the Settings screen), without
//! restarting the endpoint or dropping sessions.

use std::time::Duration;

use tlp_desk_proto::Permissions;

use crate::HostConfig;

/// The part of [`HostConfig`] that [`crate::Host::set_live_options`] can replace at runtime.
///
/// Changes apply to the *next* prompt / authentication / session; running sessions keep what
/// they were granted (the host user changes those with [`crate::Host::set_permissions`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveOptions {
    /// See [`HostConfig::default_permissions`].
    pub default_permissions: Permissions,
    /// See [`HostConfig::accept_timeout`].
    pub accept_timeout: Duration,
    /// See [`HostConfig::allow_one_time_code`].
    pub allow_one_time_code: bool,
    /// See [`HostConfig::local_input_grace`].
    pub local_input_grace: Duration,
    /// See [`HostConfig::remote_pointer_marker`].
    pub remote_pointer_marker: bool,
}

impl LiveOptions {
    /// The live options a host starts with.
    pub fn from_config(cfg: &HostConfig) -> Self {
        Self {
            default_permissions: cfg.default_permissions,
            accept_timeout: cfg.accept_timeout,
            allow_one_time_code: cfg.allow_one_time_code,
            local_input_grace: cfg.local_input_grace,
            remote_pointer_marker: cfg.remote_pointer_marker,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_options_mirror_the_config() {
        let cfg = HostConfig {
            accept_timeout: Duration::from_secs(15),
            allow_one_time_code: false,
            remote_pointer_marker: false,
            ..HostConfig::default()
        };
        let live = LiveOptions::from_config(&cfg);
        assert_eq!(live.accept_timeout, Duration::from_secs(15));
        assert!(!live.allow_one_time_code);
        assert!(!live.remote_pointer_marker);
        assert_eq!(live.default_permissions, crate::DEFAULT_PERMISSIONS);
        assert_eq!(live.local_input_grace, Duration::from_millis(1000));
    }
}
