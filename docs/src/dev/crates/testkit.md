# tlp-desk-testkit

**Dev-only test harness**: fakes for every hardware-facing piece of a session and a real host +
viewer pair over loopback QUIC in one call. Never shipped (`publish = false`); every crate uses it
from `[dev-dependencies]` only.

| | |
|---|---|
| Path | `crates/tlp-desk-testkit` |
| Depends on | `proto`, `crypto`, `net`, `capture`, `codec`, `input`, `host`, `viewer`, `tokio`, `tracing-subscriber`, `memory-stats` |
| Used by | tests of `host` (and any lane's end-to-end tests) |
| Unsafe | `#![forbid(unsafe_code)]` |

Nothing in it touches the real desktop, mouse, keyboard, clipboard, speakers or network beyond
`127.0.0.1`.

## Modules

| Module | What it gives you |
|---|---|
| `fakes` | `FakeCapturer` (a moving bar, N monitors with Arabic names), `FakeEncoder` (no video work), `RecordingInjector` + `Recorded`, `FakeLocalActivity` (`touch()` = the host user moved the mouse), `FakePointerMarker`, `FakeClipboard` (text / image / HTML / files + a sequence number), `FakeAudioSource` (deterministic sine or silence in 20 ms chunks). `*_factory` helpers return the `HostConfig` factory types. Every fake counts itself in `Alive`. |
| `pair` | `TestPair::start()`, `start_with(\|host, viewer\| ...)`, `start_with_fake_encoder(...)`; `connect(permissions) -> Connected { session, events, id }`; `spawn_connect(auth)`; `host_event(pred)`; `all_released()`. |
| `probe` | `ProbeFeature::all(name)` / `matching(name, filter)`: a `Feature` that records the control messages routed to it and the live permissions it sees. |
| `wait` | `within`, `until`, `quiet`, `next_frame`, `host_event`, `viewer_event`, `init_tracing` — every wait has a deadline (15 s). |
| `resources` | `resident_bytes`, `alive_tasks`, `Sample`, `memory_growth` for soak tests. |

## Example

```rust,ignore
use tlp_desk_proto::{Message, Permissions};
use tlp_desk_testkit::{pair::TestPair, probe::ProbeFeature, wait};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_reaches_the_host_feature() {
    let chat = ProbeFeature::matching("chat", |m| matches!(m, Message::ChatMessage { .. }));
    let probe = chat.clone();
    let mut pair = TestPair::start_with(move |host, _viewer| host.features.push(probe)).await;
    let conn = pair.connect(Permissions::VIEW | Permissions::CHAT).await;
    conn.session.send_message(Message::ChatMessage {
        id: 1,
        text: "مرحبا".into(),
        sent_unix_ms: 0,
    });
    wait::until("chat to arrive", || chat.received().len() == 1).await;
    conn.session.disconnect();
    pair.all_released().await; // no session, no live fake = per-session threads ended
}
```

A lane registers its real `Feature` the same way (`host.features.push(...)` /
`viewer.features.push(...)`) and keeps the fakes for everything else.

## Tips

- Input sent before the first decoded frame is dropped by the host (the injector is created
  once the pipeline knows the monitor): `wait::next_frame(&conn.session).await` first.
- To check that something passed the host's permission gate, send a `VIEW`-only message (e.g.
  `LatencyProbe`) after it and wait for the probe to receive that: the control stream is ordered.
- `FakeEncoder` frames are not H.264; use it when no decoded picture is needed (soaks, wire
  checks).

## Tests

- Unit tests for every fake (`fakes::*::tests`, `resources::tests`).
- `tests/pair.rs`: `pair_streams_injects_routes_and_releases`,
  `fake_encoder_pair_runs_and_releases`.
