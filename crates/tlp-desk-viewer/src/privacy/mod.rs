//! Privacy mode, viewer side: ask the host to blank its physical screen and block its local
//! input, and learn the actual state.
//!
//! * Turn it on/off with `ViewerSession::send_message(`[`privacy_request`]`(enabled))`. The
//!   host needs to have granted `PRIVACY_MODE`; otherwise it answers "off".
//! * Register [`PrivacyViewerFeature`] in `ViewerConfig::features`: every
//!   `Message::PrivacyMode` from the host (the answer, and any later change such as the host
//!   user's emergency exit, a revoked permission or the lock screen) arrives as a
//!   `ViewerEvent::Feature` named [`PRIVACY_FEATURE`] with a [`PrivacyState`] payload.

use tlp_desk_net::{Feature, FeatureCtx};
use tlp_desk_proto::Message;
use tokio::sync::mpsc;

/// `FeatureEvent::feature` of privacy-mode events (same name as the host side's feature).
pub const PRIVACY_FEATURE: &str = "privacy";

/// The host's actual privacy-mode state, as last reported by the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrivacyState {
    /// The host screen is blanked and its local input blocked.
    pub enabled: bool,
}

/// The control message that asks the host to turn privacy mode on or off.
pub fn privacy_request(enabled: bool) -> Message {
    Message::PrivacyMode { enabled }
}

/// Viewer-side privacy feature: turns the host's `PrivacyMode` messages into UI events.
#[derive(Debug, Clone, Copy, Default)]
pub struct PrivacyViewerFeature;

impl Feature for PrivacyViewerFeature {
    fn name(&self) -> &'static str {
        PRIVACY_FEATURE
    }

    fn handles(&self, msg: &Message) -> bool {
        matches!(msg, Message::PrivacyMode { .. })
    }

    fn start(&self, ctx: FeatureCtx, mut inbox: mpsc::UnboundedReceiver<Message>) {
        tokio::spawn(async move {
            while let Some(msg) = inbox.recv().await {
                if let Message::PrivacyMode { enabled } = msg {
                    tracing::info!(enabled, "host privacy mode state");
                    ctx.emit(PRIVACY_FEATURE, PrivacyState { enabled });
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_and_routing() {
        let f = PrivacyViewerFeature;
        assert_eq!(
            privacy_request(true),
            Message::PrivacyMode { enabled: true }
        );
        assert!(f.handles(&Message::PrivacyMode { enabled: false }));
        assert!(!f.handles(&Message::RequestKeyframe));
        assert_eq!(f.name(), PRIVACY_FEATURE);
    }
}
