//! Viewer side of the whiteboard: receives what the host sends (its user cleared the drawings,
//! or host-side strokes) and reports it to the UI as [`WhiteboardEvent`]s.
//!
//! Drawing itself happens in the UI with `tlp_desk_input::whiteboard::Editor`, whose ops are
//! sent with `ViewerSession::send_message`; the host enforces `WHITEBOARD` on them.

use tlp_desk_input::whiteboard::{WhiteboardOp, FEATURE_NAME};
use tlp_desk_net::{Feature, FeatureCtx};
use tlp_desk_proto::{Message, Permissions};
use tokio::sync::mpsc;

/// What the host's whiteboard did (payload of `ViewerEvent::Feature` with
/// `feature == tlp_desk_input::whiteboard::FEATURE_NAME`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhiteboardEvent {
    /// The host sent an op: `Clear` (the host user wiped the screen) or a stroke.
    Remote(WhiteboardOp),
}

/// The viewer's whiteboard feature. Register it in `ViewerConfig::features`.
#[derive(Debug, Default, Clone, Copy)]
pub struct WhiteboardViewer;

impl Feature for WhiteboardViewer {
    fn name(&self) -> &'static str {
        FEATURE_NAME
    }

    fn handles(&self, msg: &Message) -> bool {
        matches!(
            msg,
            Message::WhiteboardStroke { .. } | Message::WhiteboardClear
        )
    }

    fn start(&self, ctx: FeatureCtx, inbox: mpsc::UnboundedReceiver<Message>) {
        tokio::spawn(run(ctx, inbox));
    }
}

async fn run(ctx: FeatureCtx, mut inbox: mpsc::UnboundedReceiver<Message>) {
    while let Some(msg) = inbox.recv().await {
        // A clear always applies (it only removes things); strokes need the permission.
        let op = WhiteboardOp::from_message(&msg);
        match op {
            Some(WhiteboardOp::Stroke(_)) if !ctx.allowed(Permissions::WHITEBOARD) => {
                tracing::debug!("whiteboard stroke from the host without WHITEBOARD; ignored");
            }
            Some(op) => ctx.emit(FEATURE_NAME, WhiteboardEvent::Remote(op)),
            None => tracing::debug!("malformed whiteboard message from the host; ignored"),
        }
    }
}
