//! Long-session soak: two viewers stay connected to one host with full control for the whole
//! run (the "two colleagues for two hours" scenario, compressed), both moving the mouse and
//! typing continuously. Fake capture, real OpenH264 encode and decode, loopback QUIC.
//!
//! Checks that nothing ends a long session (keep-alives, timers, banners), that video keeps
//! flowing to both viewers the whole time, that both inputs keep arriving, and that resident
//! memory stays flat.
//!
//! Ignored by default (it runs for minutes):
//!
//! ```text
//! $env:TLP_SOAK_SECS = "600"   # default 600 (10 minutes)
//! cargo test --release -p tlp-desk-host --test multiviewer_soak -- --ignored --nocapture
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use tlp_desk_proto::{InputEvent, Permissions};
use tlp_desk_testkit::pair::{endpoint, viewer_config, TestPair};
use tlp_desk_testkit::resources::{self, mib, Sample};
use tlp_desk_viewer::Viewer;

/// How often memory, frames and inputs are sampled.
const SAMPLE_EVERY: Duration = Duration::from_secs(5);
/// Allowed growth of resident memory between the first and the last third of the run.
const MAX_GROWTH: i64 = 48 * 1024 * 1024;

fn soak_duration() -> Duration {
    let secs = std::env::var("TLP_SOAK_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    Duration::from_secs(secs)
}

fn full() -> Permissions {
    Permissions::VIEW | Permissions::CONTROL | Permissions::KEYBOARD
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "soak: runs for TLP_SOAK_SECS (default 600 s); see the file header"]
async fn two_viewers_stay_connected_and_in_control_for_a_long_session() {
    let total = soak_duration();
    let mut pair = TestPair::start().await;
    let a = pair.connect(full()).await;
    pair.viewer = Arc::new(Viewer::new(endpoint(false, "زميل-2"), viewer_config()).unwrap());
    let b = pair.connect(full()).await;
    assert_eq!(pair.host.sessions().len(), 2);

    let start = Instant::now();
    let mut samples: Vec<Sample> = Vec::new();
    let mut next_sample = Instant::now() + SAMPLE_EVERY;
    let mut last = (0u64, 0u64);
    let mut i = 0u32;
    while start.elapsed() < total {
        i = i.wrapping_add(1);
        // Viewer A moves in the low half of the range, B in the high half, so the injected
        // log shows both are still in control.
        let x = u16::try_from(i % 30_000).unwrap_or(0);
        a.session.send_input(InputEvent::MouseMove { x, y: 100 });
        b.session.send_input(InputEvent::MouseMove {
            x: x.saturating_add(32_000),
            y: 200,
        });
        if i % 10 == 0 {
            let down = i % 20 == 0;
            a.session.send_input(InputEvent::Key {
                scancode: 0x1E,
                extended: false,
                down,
            });
            b.session.send_input(InputEvent::Key {
                scancode: 0x30,
                extended: false,
                down,
            });
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        if Instant::now() < next_sample {
            continue;
        }
        next_sample += SAMPLE_EVERY;
        let s = Sample::now(start.elapsed());
        let now = (
            a.session.stats().frames_decoded,
            b.session.stats().frames_decoded,
        );
        assert!(
            now.0 > last.0 && now.1 > last.1,
            "video stalled at {:?}: {last:?} -> {now:?}",
            s.at
        );
        assert!(
            a.session.end_reason().is_none() && b.session.end_reason().is_none(),
            "a session ended at {:?}: {:?} / {:?}",
            s.at,
            a.session.end_reason(),
            b.session.end_reason()
        );
        let injected = pair.injected.events();
        pair.injected.clear();
        let from_a = injected
            .iter()
            .filter(|e| matches!(e, InputEvent::MouseMove { x, .. } if *x < 32_000))
            .count();
        let from_b = injected
            .iter()
            .filter(|e| matches!(e, InputEvent::MouseMove { x, .. } if *x >= 32_000))
            .count();
        assert!(
            from_a > 0 && from_b > 0,
            "input stopped at {:?}: A {from_a}, B {from_b}",
            s.at
        );
        println!(
            "{:>5}s  rss {}  tasks {}  decoded A {} B {}  moves A {} B {}  rtt A {:.1} ms B {:.1} ms",
            s.at.as_secs(),
            mib(s.resident),
            s.tasks,
            now.0,
            now.1,
            from_a,
            from_b,
            a.session.stats().rtt_ms,
            b.session.stats().rtt_ms,
        );
        last = now;
        samples.push(s);
    }
    assert_eq!(pair.host.sessions().len(), 2, "both viewers still listed");
    let growth = resources::memory_growth(&samples);
    println!(
        "multiviewer soak: {:?}, decoded A {} B {}, memory growth {:?} bytes",
        start.elapsed(),
        last.0,
        last.1,
        growth
    );
    if let Some(g) = growth {
        assert!(
            g < MAX_GROWTH,
            "resident memory grew by {}",
            mib(g.max(0) as usize)
        );
    }
    a.session.disconnect();
    b.session.disconnect();
    pair.all_released().await;
}
