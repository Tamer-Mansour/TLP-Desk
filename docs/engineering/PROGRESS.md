# Progress

Branch: `claude/blissful-mendel-laexg6` (based on `TLP-Desk` @ `13ae43e`, v1.0.0-rc.4).
Read `architecture-audit.md` first, then this file, `DECISIONS.md` and `REMAINING-WORK.md`.

## How to resume

```sh
# Linux build deps (same as CI): libasound2-dev pkg-config cmake nasm xvfb xauth x11-utils
export CARGO_TARGET_DIR=/tmp/tlp-target            # never inside the repo
export CARGO_PROFILE_DEV_DEBUG=line-tables-only    # keeps the target dir under ~12 GB
export CARGO_PROFILE_TEST_DEBUG=line-tables-only
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The cloud session's disk allowance is about 30 GB: one target dir with full debug info plus a
second worktree's target dir filled it and the linker died with SIGBUS. Use one target dir.

## Milestones

| Milestone | State | Commits |
|---|---|---|
| Phase 0 audit + roadmap | Done | `TLP-docs: engineering audit…` |
| P0 BUG 1: false "my own device" | Done, tested (unit + e2e on loopback); needs a two-PC manual check | `TLP-net: rank and expire…`, `TLP-viewer: dial all…`, `TLP-app: self-connection message…` |
| P0 BUG 2: LAN connect fails until Wi-Fi restart | Done, tested (unit + e2e + discovery on loopback); needs a two-PC manual check | same |
| P0 storage test on Linux/macOS | Done | `TLP-crypto: fix macOS data-dir test…` |
| P1.1 viewport-matched streaming (4K host → small window) | Done, tested (unit + host e2e with fakes); needs a visual check on a 4K Windows host | `TLP-host: viewport hint…`, `TLP-app: send the picture area…` |
| P1.2 zoom & pan | Not started | |
| P1.3 maximum clarity mode | Not started (design notes in REMAINING-WORK.md) | |
| P2 title bar / UI redesign | Not started (needs Windows for verification) | |
| P3 updates | Mostly pre-existing (rc.4); small items left | |

## What changed, by crate

* `tlp-desk-net`: `discovery.rs` (per-source addresses, `rank_addrs`, netwatch, identity
  conflict), `endpoint.rs` (`dial_any`, `authorize`, bounded `dial`, self-key refusal on both
  sides), `error.rs` (`SelfConnection`).
* `tlp-desk-viewer`: `lib.rs` (connect via `dial_any`, `pick_dial_error`, `SelfConnection`,
  wrong-device vs key-changed).
* `tlp-desk-host`: `adapt/quality.rs` (`QualityTarget::viewport`, `cap_for`), `adapt/source.rs`,
  `pipeline.rs`; tests `tests/connect_paths.rs`, `tests/perf.rs`
  (`viewport_hint_scales_the_stream_to_the_viewer_window`).
* `tlp-desk-app`: self-connection message, own-ID filter incl. service ID, identity conflict
  warning (`ui/home.rs`), `ui/session/viewport_hint.rs`, 1:1 for barely smaller frames
  (`ui/view_scale.rs`).
* `tlp-desk-cli`: exit code for `SelfConnection` (`Usage`).
* Docs: changelog, troubleshooting (en + ar), `docs/protocol.md` (SetQuality preset fields),
  `docs/engineering/*`, CLAUDE.md layout table.

No `PROTOCOL_VERSION` change so far; every change is compatible with rc.4 peers.
