# Contributing to TLP-Desk

Thanks for helping. TLP-Desk is a LAN-only remote desktop in pure Rust; Windows 10/11 x64 first.
The complete rules for people and coding agents are in [CLAUDE.md](CLAUDE.md) and
[docs/CODE_STYLE.md](docs/CODE_STYLE.md); this page is the short version.

## Workflow

1. Pick an item from [ROADMAP.md](ROADMAP.md) (or an issue) and say you are on it.
2. Branch from `TLP-Desk` as `TLP-<topic>` (e.g. `TLP-files`). Never commit to `TLP-Desk` directly.
3. Commit small, self-contained changes that each build and pass their tests:
   `TLP-<area>: what changed` (e.g. `TLP-net: tune QUIC windows for bulk transfers`). Stage exact
   paths; never `git add -A`; never `--no-verify`.
4. Open a pull request into `TLP-Desk` and fill in the template. CI (fmt, clippy, tests, release
   build, typos, cargo-deny) must be green and the code owner must approve.
5. Tick the ROADMAP items you finished, with evidence (test name or manual test note).

## Running the checks

```powershell
# Optional: keep build output outside the repo (e.g. on another drive).
$env:CARGO_TARGET_DIR = "C:\tlp-build\my-lane"
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check.ps1
```

`check.ps1` runs `cargo fmt --all -- --check`, a release build, `cargo clippy --workspace
--all-targets -- -D warnings` and `cargo test --workspace`. While iterating, `cargo test -p
tlp-desk-<crate>` is enough. Tests that need a real desktop are `#[ignore]` and never grab your
screen or mouse in a normal run.

## Rules that are not negotiable

- **Clean-room.** Never copy code, protocol details, assets, icons, names or UI text from AnyDesk,
  RustDesk, TeamViewer or any other remote-desktop product. Use public OS documentation and crate
  docs only.
- **LAN only.** No internet calls, update checks, telemetry or crash uploads.
- **Security first.** Everything encrypted and authenticated; no debug switch that weakens it.
- **Arabic is first-class.** Anything that shows, stores or sends text must work in Arabic (RTL
  layout, shaping, bidi, UTF-16 paths), with an Arabic test case. Every UI string goes through the
  i18n table in English and Arabic.
- **Wire changes** bump `PROTOCOL_VERSION`, append enum variants at the end only and update
  [docs/protocol.md](docs/protocol.md).
- **Files have one place** (CLAUDE.md, "Repository layout"). Never commit screenshots, build output
  or temp files.

## Documentation

The book in [`docs/`](docs/) (mdBook: `docs/src/SUMMARY.md`, build with `mdbook build docs`) has
the user guide in English and Arabic, the contributor guide and the ADRs. When your change is
visible to users or changes how a crate works, update the matching pages in the same pull request:

- the crate page in `docs/src/dev/crates/`;
- the user guide page in **both** `docs/src/user-guide/en/` and `docs/src/user-guide/ar/` — and
  when a feature ships, turn its *Coming soon* page into a real one;
- `docs/src/changelog.md` (Unreleased section);
- a new ADR in `docs/src/adr/` for a decision others should not have to rediscover.

Arabic pages are wrapped in `<div dir="rtl">` and use the same terms as the Arabic interface.

## Reporting problems

Bugs and ideas: GitHub issues. Security problems: privately, see [SECURITY.md](SECURITY.md).
By participating you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).
