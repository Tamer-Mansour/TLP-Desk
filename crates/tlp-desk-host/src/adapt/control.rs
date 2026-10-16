//! The session control loop's side of adaptive streaming: `SetQuality`, `ViewerFeedback` and
//! `LatencyProbe` from the viewer.

use std::sync::Arc;
use std::time::Instant;

use tlp_desk_net::{NetError, Session};
use tlp_desk_proto::{Message, Permissions};
use tokio::sync::watch;

use super::{AdaptShared, QualityTarget, ViewerReport};

/// Handles the quality / latency messages of one session.
pub(crate) struct PerfControl {
    pub(crate) shared: Arc<AdaptShared>,
    quality: watch::Sender<QualityTarget>,
    default: QualityTarget,
}

impl PerfControl {
    /// `default` applies until the viewer sends `SetQuality`. Returns the receiver for the
    /// pipeline.
    pub(crate) fn new(default: QualityTarget) -> (Self, watch::Receiver<QualityTarget>) {
        let (tx, rx) = watch::channel(default);
        (
            Self {
                shared: Arc::new(AdaptShared::new(Instant::now())),
                quality: tx,
                default,
            },
            rx,
        )
    }

    /// Handles `msg` if it is a quality / latency message (and the session holds the
    /// permission it needs); otherwise gives it back for the next handler.
    ///
    /// # Errors
    /// Sending the `LatencyEcho` failed (the connection is gone).
    pub(crate) async fn handle(
        &self,
        session: &Session,
        perms: Permissions,
        msg: Message,
    ) -> Result<Option<Message>, NetError> {
        let ours = matches!(
            msg,
            Message::SetQuality(_) | Message::ViewerFeedback { .. } | Message::LatencyProbe { .. }
        );
        if !ours {
            return Ok(Some(msg));
        }
        if !perms.contains(msg.required_permissions()) {
            tracing::debug!("quality message blocked by permissions");
            return Ok(None);
        }
        match msg {
            Message::SetQuality(q) => {
                let target = QualityTarget::resolve(&q, self.default);
                tracing::info!(requested = ?q, ?target, "viewer changed the quality");
                self.quality.send_replace(target);
            }
            Message::ViewerFeedback {
                decode_us,
                frames_dropped,
                rtt_us,
            } => self.shared.push_feedback(ViewerReport {
                decode_us,
                frames_dropped,
                rtt_us,
            }),
            Message::LatencyProbe { t_viewer_us } => {
                // The host's session clock right now: the timestamp a frame captured at this
                // instant carries, so the viewer can map frame timestamps to its own clock.
                let echo = Message::LatencyEcho {
                    t_viewer_us,
                    t_host_capture_us: self.shared.now_us(),
                };
                session.send(&echo).await?;
            }
            other => return Ok(Some(other)),
        }
        Ok(None)
    }
}
