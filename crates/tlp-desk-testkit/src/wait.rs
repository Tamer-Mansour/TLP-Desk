//! Bounded waits: every await in a test has a deadline, so a hang fails instead of stalling.

use std::future::Future;
use std::time::Duration;

use tlp_desk_host::{HostEvent, HostEvents};
use tlp_desk_viewer::{DecodedFrame, ViewerEvent, ViewerEvents, ViewerSession};

/// Default deadline for one expected thing to happen.
pub const WAIT: Duration = Duration::from_secs(15);

/// Poll interval of [`until`] and [`next_frame`].
pub const POLL: Duration = Duration::from_millis(10);

/// Installs a test-friendly `tracing` subscriber once (`RUST_LOG` overrides the default
/// `warn`). Safe to call from every test.
pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_test_writer()
        .try_init();
}

/// Awaits `fut`, panicking with "timed out waiting for {what}" after [`WAIT`].
pub async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    match tokio::time::timeout(WAIT, fut).await {
        Ok(v) => v,
        Err(_) => panic!("timed out waiting for {what}"),
    }
}

/// `Some(value)` if `fut` completes within `dur`, `None` otherwise. For asserting that
/// something does *not* happen: `assert!(quiet(dur, rx.recv()).await.is_none())`.
pub async fn quiet<T>(dur: Duration, fut: impl Future<Output = T>) -> Option<T> {
    tokio::time::timeout(dur, fut).await.ok()
}

/// Polls `cond` every [`POLL`] until it holds; panics after [`WAIT`].
pub async fn until(what: &str, mut cond: impl FnMut() -> bool) {
    within(what, async {
        while !cond() {
            tokio::time::sleep(POLL).await;
        }
    })
    .await;
}

/// The next decoded frame of a viewer session.
pub async fn next_frame(session: &ViewerSession) -> DecodedFrame {
    within("a decoded frame", async {
        loop {
            if let Some(f) = session.take_frame() {
                return f;
            }
            tokio::time::sleep(POLL).await;
        }
    })
    .await
}

/// The next host event matching `pred` (others are skipped).
pub async fn host_event(events: &mut HostEvents, pred: impl Fn(&HostEvent) -> bool) -> HostEvent {
    within("a host event", async {
        loop {
            match events.recv().await {
                Some(ev) if pred(&ev) => return ev,
                Some(_) => {}
                None => panic!("host event channel closed"),
            }
        }
    })
    .await
}

/// The next viewer event matching `pred` (others are skipped).
pub async fn viewer_event(
    events: &mut ViewerEvents,
    pred: impl Fn(&ViewerEvent) -> bool,
) -> ViewerEvent {
    within("a viewer event", async {
        loop {
            match events.recv().await {
                Some(ev) if pred(&ev) => return ev,
                Some(_) => {}
                None => panic!("viewer event channel closed"),
            }
        }
    })
    .await
}
