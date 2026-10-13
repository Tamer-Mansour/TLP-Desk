//! Dev-only test harness for TLP-Desk (never shipped; use it from `[dev-dependencies]`).
//!
//! * [`fakes`]: a synthetic screen ([`fakes::FakeCapturer`]), a fake encoder, a recording
//!   input injector, a controllable "local user activity" source, a pointer marker without a
//!   window, an in-memory clipboard and a deterministic audio source. Nothing touches the real
//!   desktop, mouse, keyboard, clipboard or speakers. Every fake counts its live instances in
//!   [`fakes::Alive`], so a test can prove the host released them (= its per-session threads
//!   ended).
//! * [`pair`]: a real [`tlp_desk_host::Host`] and a real [`tlp_desk_viewer::Viewer`] over QUIC
//!   on `127.0.0.1` in one call: [`pair::TestPair::start`], then [`pair::TestPair::connect`].
//! * [`probe`]: a [`tlp_desk_net::Feature`] that records the control messages routed to it,
//!   to check what reaches a feature (and what the permission gate stops).
//! * [`wait`]: bounded waits ([`wait::within`], [`wait::until`]) so a hang fails the test
//!   instead of stalling CI, and [`wait::init_tracing`].
//! * [`resources`]: memory and task counts for soak/leak tests.
//!
//! ```no_run
//! # async fn demo() {
//! use tlp_desk_testkit::pair::TestPair;
//! use tlp_desk_proto::Permissions;
//! let mut pair = TestPair::start().await;
//! let conn = pair.connect(Permissions::ALL).await;
//! let frame = tlp_desk_testkit::wait::next_frame(&conn.session).await;
//! assert_eq!(frame.width, tlp_desk_testkit::fakes::DEFAULT_W);
//! # }
//! ```
//!
//! See `docs/src/dev/testing.md`, "The test kit".
#![forbid(unsafe_code)]

pub mod fakes;
pub mod pair;
pub mod probe;
pub mod resources;
pub mod wait;
