//! Leak/soak test: a host and a viewer on loopback with fake capture (real OpenH264 encode
//! and decode) for a long time. The viewer moves the mouse and types continuously, sends
//! control messages, and reconnects periodically. Checks that resident memory stays flat,
//! tokio tasks and per-session threads return to their baseline, and video keeps flowing.
//!
//! Ignored by default (it runs for minutes):
//!
//! ```text
//! $env:TLP_SOAK_SECS = "600"   # default 600 (10 minutes)
//! cargo test --release -p tlp-desk-host --test soak -- --ignored --nocapture
//! ```

use std::time::{Duration, Instant};

use tlp_desk_host::HostEvent;
use tlp_desk_proto::{InputEvent, Message, Permissions};
use tlp_desk_testkit::pair::TestPair;
use tlp_desk_testkit::resources::{self, mib, Sample};
use tlp_desk_testkit::wait;

/// How long one session lasts before the viewer reconnects.
const SESSION: Duration = Duration::from_secs(30);
/// How often memory and tasks are sampled.
const SAMPLE_EVERY: Duration = Duration::from_secs(5);
/// Allowed growth of resident memory between the first and the last third of the run.
const MAX_GROWTH: i64 = 48 * 1024 * 1024;
/// Allowed difference in alive tokio tasks between before and after (runtime internals).
const TASK_SLACK: usize = 4;

fn soak_duration() -> Duration {
    let secs = std::env::var("TLP_SOAK_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    Duration::from_secs(secs)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "soak: runs for TLP_SOAK_SECS (default 600 s); see the file header"]
async fn soak_host_and_viewer_stay_bounded() {
    let total = soak_duration();
    let mut pair = TestPair::start().await;
    let baseline_tasks = resources::alive_tasks();
    let start = Instant::now();
    let mut samples: Vec<Sample> = Vec::new();
    let mut sessions = 0u32;
    let mut frames = 0u64;
    let mut next_sample = Instant::now();
    while start.elapsed() < total {
        let conn = pair.connect(Permissions::ALL).await;
        sessions += 1;
        let session_start = Instant::now();
        let mut i = 0u32;
        let mut last_frames = conn.session.stats().frames_decoded;
        while session_start.elapsed() < SESSION && start.elapsed() < total {
            i = i.wrapping_add(1);
            let x = u16::try_from(i % 65_536).unwrap_or(0);
            conn.session.send_input(InputEvent::MouseMove {
                x,
                y: x.reverse_bits(),
            });
            if i % 10 == 0 {
                let down = i % 20 == 0;
                conn.session.send_input(InputEvent::Key {
                    scancode: 0x1E,
                    extended: false,
                    down,
                });
                conn.session.send_message(Message::LatencyProbe {
                    t_viewer_us: u64::from(i),
                });
            }
            if i % 500 == 0 {
                conn.session.select_monitor(i / 500 % 2);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
            if Instant::now() >= next_sample {
                next_sample += SAMPLE_EVERY;
                let s = Sample::now(start.elapsed());
                let decoded = conn.session.stats().frames_decoded;
                assert!(
                    decoded > last_frames || s.at < Duration::from_secs(3),
                    "video stalled at {:?} (decoded {decoded})",
                    s.at
                );
                last_frames = decoded;
                println!(
                    "{:>5}s  rss {}  tasks {}  sessions {}  injected {}  decoded {}",
                    s.at.as_secs(),
                    mib(s.resident),
                    s.tasks,
                    sessions,
                    pair.injected.len(),
                    decoded
                );
                samples.push(s);
            }
        }
        frames += conn.session.stats().frames_decoded;
        pair.injected.clear();
        conn.session.disconnect();
        pair.host_event(|e| matches!(e, HostEvent::SessionEnded { .. }))
            .await;
        pair.all_released().await;
    }

    // Everything the sessions created is gone.
    wait::until("viewer connections to close", || {
        pair.viewer_ep.open_connections() == 0
    })
    .await;
    wait::until("tokio tasks to return to the baseline", || {
        resources::alive_tasks() <= baseline_tasks + TASK_SLACK
    })
    .await;
    assert!(frames > 0, "no video was decoded");
    let growth = resources::memory_growth(&samples);
    println!(
        "soak: {:?}, {sessions} sessions, {frames} frames decoded, memory growth {:?} bytes, tasks {} -> {}",
        start.elapsed(),
        growth,
        baseline_tasks,
        resources::alive_tasks()
    );
    if let Some(g) = growth {
        assert!(
            g < MAX_GROWTH,
            "resident memory grew by {}",
            mib(g.max(0) as usize)
        );
    }
}
