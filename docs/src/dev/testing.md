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
cargo deny check   # licences (permissive only), advisories, sources — see deny.toml
typos              # spelling — see typos.toml
```

## Kinds of tests

| Kind | Where | Needs | Runs in CI |
|---|---|---|---|
| Unit tests | `src/**` `#[cfg(test)]` in every crate | nothing | yes |
| Protocol round trips | `tlp-desk-proto/tests/roundtrip.rs` | nothing | yes |
| Network integration | `tlp-desk-net/tests/session.rs`, `tests/discovery.rs` | loopback UDP | yes (except mDNS) |
| End-to-end host ↔ viewer | `tlp-desk-host/tests/e2e.rs` | loopback; fake capturer | yes (except real capture) |
| Real backend | `tlp-desk-app/tests/real_backend.rs` | loopback; real QUIC, TLS, SPAKE2, H.264 | yes |
| Headless UI | `tlp-desk-app/src/ui/session.rs`, `i18n/shaping.rs` | egui without a window | yes |
| Hardware tests | marked `#[ignore = "..."]` | a real, unlocked Windows desktop | **no** — run by hand |
| Benchmarks | `tlp-desk-codec/examples/bench.rs` | real desktop | by hand |

### End-to-end without a desktop

`tlp-desk-host` takes factories for its capturer, encoder, injector, local-activity monitor,
pointer marker and monitor lister. The e2e tests plug in fakes and then run a real `Host` and a
real `Viewer` over QUIC on `127.0.0.1`. That is how accept/reject, codes, rate limits, TOFU, the
allow-list, monitor switching, local priority and frame dropping are all tested in CI.

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
- Tests may `unwrap`, `expect` and index freely (`clippy.toml` allows it in tests).
