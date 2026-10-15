//! Whiteboard glue for the real backend: which features to register and how their events and
//! the UI's ops map to the wire.

use std::sync::Arc;

use tlp_desk_input::whiteboard::{WhiteboardOp, FEATURE_NAME};
use tlp_desk_net::FeatureEvent;
use tlp_desk_proto::Message;
use tlp_desk_viewer::{WhiteboardEvent, WhiteboardViewer};

/// The viewer-side whiteboard feature, for `ViewerConfig::features`.
pub fn viewer_feature() -> Arc<WhiteboardViewer> {
    Arc::new(WhiteboardViewer)
}

/// The op inside a viewer feature event, if it is a whiteboard event.
pub fn remote_op(event: &FeatureEvent) -> Option<WhiteboardOp> {
    if event.feature != FEATURE_NAME {
        return None;
    }
    match event.payload::<WhiteboardEvent>()? {
        WhiteboardEvent::Remote(op) => Some(op.clone()),
    }
}

/// The wire messages for `ops`, in order (long strokes are split into chunks).
pub fn messages(ops: &[WhiteboardOp]) -> Vec<Message> {
    ops.iter().flat_map(WhiteboardOp::to_messages).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tlp_desk_input::whiteboard::{Rgba, Stroke, MAX_POINTS_PER_MESSAGE};

    #[test]
    fn maps_only_whiteboard_events() {
        let ev = FeatureEvent::new(
            FEATURE_NAME,
            3,
            WhiteboardEvent::Remote(WhiteboardOp::Clear),
        );
        assert_eq!(remote_op(&ev), Some(WhiteboardOp::Clear));
        let other = FeatureEvent::new("chat", 3, WhiteboardEvent::Remote(WhiteboardOp::Clear));
        assert_eq!(remote_op(&other), None);
        let wrong_payload = FeatureEvent::new(FEATURE_NAME, 3, "مرحبا".to_owned());
        assert_eq!(remote_op(&wrong_payload), None);
    }

    #[test]
    fn long_strokes_become_bounded_messages() {
        let s = Stroke {
            id: 1,
            points: vec![(1, 1); MAX_POINTS_PER_MESSAGE * 2 + 1],
            color: Rgba::new(1, 2, 3, 4),
            width: 5,
        };
        let msgs = messages(&[WhiteboardOp::Clear, WhiteboardOp::Stroke(s)]);
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0], Message::WhiteboardClear);
    }
}
