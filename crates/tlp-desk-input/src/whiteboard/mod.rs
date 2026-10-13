//! Whiteboard / draw-on-screen: the viewer draws annotations that appear on the host's screen.
//!
//! * [`Stroke`], [`WhiteboardOp`] and [`Board`]: the shared model and its wire form
//!   (`Message::WhiteboardStroke` / `Message::WhiteboardClear`), with hard caps on everything a
//!   peer can send.
//!
//! The overlay window is excluded from screen capture: the viewer renders the same strokes
//! locally on top of the video, so they appear without waiting for a captured, encoded and
//! decoded frame (which would double the latency and blur thin lines).

mod stroke;

pub use stroke::{
    Board, Damage, NormPoint, NormRect, Rgba, Stroke, WhiteboardOp, MAX_POINTS_PER_MESSAGE,
    MAX_POINTS_PER_STROKE, MAX_STROKES, MAX_TOTAL_POINTS, MAX_WIDTH, MAX_WIRE_POINTS, MIN_WIDTH,
};

/// `Feature::name` of the whiteboard on both sides, used to route feature events to the UI.
pub const FEATURE_NAME: &str = "whiteboard";
