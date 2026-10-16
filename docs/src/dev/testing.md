# Testing

A feature is **done** only when it builds in release, clippy is clean with `-D warnings`, all
tests pass, and its roadmap item is checked off with evidence (a test name or a manual test note
with machines and OS). This page explains the kinds of tests and how to run each.

## The quality gate: `scripts/check.ps1`

```powershell
pwsh scripts/check.ps1        # or: powershell -File scripts\check.ps1
```

It runs, stopping at the first failure:

1. `cargo fmt --all -- --check`
2. `cargo build --release --workspace`
3. `cargo clippy --workspace --all-targets -- -D warnings`
4. `cargo test --workspace`

Run it before every push. Additional repository checks:

```powershell
cargo deny check   # licences (permissive only), advisories, sources — see .cargo/deny.toml
typos              # spelling — see .typos.toml
```

## Kinds of tests

| Kind | Where | Needs | Runs in CI |
|---|---|---|---|
| Unit tests | `src/**` `#[cfg(test)]` in every crate | nothing | yes |
| Protocol round trips | `tlp-desk-proto/tests/roundtrip.rs` | nothing | yes |
| Protocol properties | `tlp-desk-proto/tests/properties.rs` (`proptest`) | nothing | yes (long runs ignored) |
| Network integration | `tlp-desk-net/tests/session.rs`, `tests/discovery.rs` | loopback UDP | yes (except mDNS) |
| Hostile peer | `tlp-desk-net/tests/hostile.rs` | loopback UDP | yes |
| End-to-end host ↔ viewer | `tlp-desk-host/tests/e2e.rs` | loopback; fake capturer | yes (except real capture) |
| Hostile viewer | `tlp-desk-host/tests/hostile_viewer.rs` | loopback; `tlp-desk-testkit` | yes |
| Soak / leak | `tlp-desk-host/tests/soak.rs` | loopback; minutes of CPU | **no** — run by hand |
| Real backend | `tlp-desk-app/tests/real_backend.rs` | loopback; real QUIC, TLS, SPAKE2, H.264 | yes |
| Headless UI | `tlp-desk-app/src/ui/session.rs`, `i18n/shaping.rs` | egui without a window | yes |
| Hardware tests | marked `#[ignore = "..."]` | a real, unlocked Windows desktop | **no** — run by hand |
| Benchmarks | `tlp-desk-codec/examples/bench.rs` | real desktop | by hand |

### End-to-end without a desktop

`tlp-desk-host` takes factories for its capturer, encoder, injector, local-activity monitor,
pointer marker and monitor lister. The e2e tests plug in fakes and then run a real `Host` and a
real `Viewer` over QUIC on `127.0.0.1`. That is how accept/reject, codes, rate limits, TOFU, the
allow-list, monitor switching, local priority and frame dropping are all tested in CI.

### The test kit (`tlp-desk-testkit`)

New end-to-end tests should not copy those fakes: the dev-only crate
[`tlp-desk-testkit`](crates/testkit.md) has them all (capturer, encoder, injector, local
activity, pointer marker, clipboard, audio source), a `TestPair` that starts a real host and
viewer in one call, a `ProbeFeature` that records what reaches session features, bounded waits,
and memory/task counters. Add it to your crate's `[dev-dependencies]`:

```toml
[dev-dependencies]
tlp-desk-testkit.workspace = true
```

```rust,ignore
let mut pair = TestPair::start_with(|host, viewer| { /* register features, tweak config */ }).await;
let conn = pair.connect(Permissions::ALL).await;   // prompt shown and accepted
wait::next_frame(&conn.session).await;
conn.session.disconnect();
pair.all_released().await;                          // sessions gone, every fake dropped
```

### Hostile input

Every decoder and every message handler must turn bad input into an error, never a panic
(CODE_STYLE section 2). Three layers check that:

- `tlp-desk-proto/tests/properties.rs` — random, truncated, mutated and oversized bytes into the
  frame decoder; every message round-trips. The `*_long` tests run 200 000 cases each and stand in
  for `cargo fuzz` (which needs nightly):
  `cargo test --release -p tlp-desk-proto --test properties -- --ignored`.
- `tlp-desk-net/tests/hostile.rs` — a peer with a valid key that sends garbage, the wrong order,
  huge frames, stalls (slowloris), floods streams, connects and vanishes in a storm, or brute-forces
  the password.
- `tlp-desk-host/tests/hostile_viewer.rs` — an authorized viewer that sends what it has no
  permission for, forges host-only messages, floods, and reconnects in a loop.

When your lane adds a message handler, add its malformed and unpermitted cases there (or next to
your feature's tests with the test kit): wrong ids, Arabic and over-long text, `..` paths,
absurd sizes.

### Soak test

`tlp-desk-host/tests/soak.rs` runs a host and a viewer (fake capture, real OpenH264) with
continuous input and periodic reconnects, sampling resident memory and tokio tasks every 5 s. It
fails if memory grows by more than 48 MiB between the first and the last third of the run, if
video stalls, or if tasks, connections or per-session threads are left behind.

```powershell
$env:TLP_SOAK_SECS = "600"    # default 10 minutes
cargo test --release -p tlp-desk-host --test soak -- --ignored --nocapture
```

## Ignored hardware tests

These need an interactive, unlocked Windows session (they capture the screen, move the real
mouse, or open windows). Run them on a real PC before checking off a capture or input roadmap
item:

| Crate | Test(s) | Why ignored | Command |
|---|---|---|---|
| capture | `lists_primary_monitor_first`, `wgc_captures_primary_monitor`, `dxgi_captures_primary_monitor`, `factory_captures_primary_monitor`, `wgc_scaled_half_matches_cpu_reference`, `dxgi_scaled_half_matches_cpu_reference`, `wgc_scaled_third_uses_mips_and_matches_cpu_reference`, `wgc_scaled_second_monitor_matches_cpu_reference`, `max_size_larger_than_monitor_is_native` | needs a real interactive desktop session | `cargo test -p tlp-desk-capture -- --ignored` |
| input | `real_mouse_move_lands_on_exact_pixel` | moves the real mouse cursor | `cargo test -p tlp-desk-input --test real_injection -- --ignored --nocapture` |
| input | `marker_window_starts_toggles_and_stops` | shows a window on the desktop | `cargo test -p tlp-desk-input -- --ignored marker_window` |
| net | `mdns_advertise_and_browse` | needs a multicast-capable adapter | `cargo test -p tlp-desk-net --test discovery -- --ignored` |
| host | `real_capture_end_to_end` | needs a real interactive desktop session | `cargo test -p tlp-desk-host --test e2e -- --ignored` |
| net | `bulk_side_stream_throughput` | benchmark: moves several GB over loopback (target ≥ 1 GB/s) | `cargo test --release -p tlp-desk-net --test session -- --ignored --nocapture bulk_side_stream_throughput` |
| proto | `every_message_roundtrips_long`, `random_bodies_never_panic_long`, `random_streams_never_panic_long`, `mutated_frames_never_panic_long` | 200 000 cases each (about a minute in release) | `cargo test --release -p tlp-desk-proto --test properties -- --ignored` |
| host | `soak_host_and_viewer_stay_bounded` | runs for `TLP_SOAK_SECS` (default 600 s) | `cargo test --release -p tlp-desk-host --test soak -- --ignored --nocapture` |

Tips:

- Do not touch the mouse while `real_injection` runs.
- With two monitors the capture tests also check the second monitor; with one they skip it.
- Run in `--release` for realistic timings.

## Manual tests

Some acceptance criteria are inherently manual (two real PCs on a switch or Wi-Fi; Arabic typing
on an Arabic keyboard layout). Record them in the roadmap as a short note: what you did, which
machines and OS builds, and the result — for example
`_(evidence: Win11 23H2 desktop ↔ Win10 laptop over Wi-Fi, 1080p 52 fps, latency est. 18 ms)_`.

Screenshots taken while testing go to `%TEMP%`, never into the repository (`.gitignore` blocks the
usual names).

## Writing tests

- Prefer pure logic plus a thin platform shim, as `input::grab_logic` and `host::priority` do —
  then most behaviour is testable without Windows hooks.
- Network tests bind to `127.0.0.1:0` (an ephemeral port) so they can run in parallel.
- Use `#[tokio::test(flavor = "multi_thread")]` for anything using `quinn`.
- A hardware test gets `#[ignore = "<reason>"]` with a reason that says what it needs, and the
  command to run it in the file's doc comment.
- Tests may `unwrap`, `expect` and index freely (`.clippy.toml` allows it in tests).
