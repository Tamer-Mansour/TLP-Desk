//! Session recording to a local file (viewer side).

/// Receives every video frame a session gets from the host, before decoding. Called on the
/// session's network task: it must return quickly and never block.
pub type VideoTap = std::sync::Arc<dyn Fn(&tlp_desk_proto::VideoFrame) + Send + Sync>;