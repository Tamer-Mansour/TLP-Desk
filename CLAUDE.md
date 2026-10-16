# TLP-Desk — Project Rules for Claude Code

TLP-Desk is a free, open-source, **LAN-only** remote desktop app (AnyDesk-style) written in pure Rust.
It works 100% offline inside a local network: no cloud, no relay server, no account, no telemetry.
One single binary acts as both **host** (being controlled) and **viewer** (controlling), like AnyDesk.

Read `ROADMAP.md` before starting any task. Work only on the lane/milestone you were assigned.

## Naming

- The product name is **TLP-Desk** everywhere a user can see it: window title, i18n app name (also in
  the Arabic UI, written in Latin letters), CLI help, installer (`installer/TLP-Desk.iss`,
  `TLP-Desk-<ver>-setup.exe`, `TLP-Desk-<ver>-win-x64-portable.zip`), firewall rule names
  (`TLP-Desk (QUIC)` ...), exe resources. Binaries: `TLP-Desk.exe` (GUI) and `TLP-Desk-cli.exe`.
- Data folders are `%ProgramData%\TLP-Desk\` and `%APPDATA%\TLP-Desk\`. Windows paths are
  case-insensitive, so these are the same folders older `tlp-desk` builds used: identities
  (device IDs) and settings survive the rename.
- **Deliberately unchanged** (renaming would break compatibility for no gain): crate names
  `tlp-desk-*`, the mDNS service type `_tlpdesk._udp.local.`, the ALPN `tlp-desk`, the TLS
  server name, SPAKE2/HMAC/exporter labels (`tlp-desk/...`), the installer AppId GUID, and every
  other wire identifier. Never rename these.

## Hard rules

1. **Clean-room.** Never copy code, protocol, assets, icons, names or UI text from AnyDesk, RustDesk,
   TeamViewer or any other remote desktop product. RustDesk is AGPL — do not read its source while
   implementing. Use only public OS documentation (Microsoft Learn, Apple docs, freedesktop) and crate docs.
2. **No network beyond the LAN.** No outbound internet calls, no update checks, no analytics, no crash
   uploads. The app must work with the Ethernet cable unplugged from the router's WAN port.
3. **Security is not optional.** Every connection is encrypted (QUIC/TLS 1.3) and authenticated
   (see Security model). No "debug mode" that disables auth may ship in release builds.
4. **Windows first.** Windows 10/11 x64 is the tier-1 target. Keep platform code behind traits so
   macOS/Linux can be added later, but do not block Windows work on them.
5. **Done = tested.** A feature is done only when: it builds with `cargo build --release`,
   `cargo clippy --workspace -- -D warnings` is clean, `cargo test --workspace` passes, and its
   acceptance criteria in ROADMAP.md are checked off with evidence (test name or manual test note).
6. **Small PRs per lane.** Touch only your crate unless the task says otherwise. Changes to
   `tlp-desk-proto` (wire protocol) require bumping `PROTOCOL_VERSION` and updating `docs/protocol.md`.

## Workspace layout

```
crates/
  tlp-desk-proto     # wire messages (serde + postcard), PROTOCOL_VERSION, framing
  tlp-desk-crypto    # identity keys, device ID, cert pinning, SPAKE2 password auth
  tlp-desk-net       # QUIC (quinn) transport, mDNS discovery, broadcast fallback, Wake-on-LAN
  tlp-desk-capture   # screen capture trait + Windows impl (Windows.Graphics.Capture / DXGI)
  tlp-desk-codec     # video encode/decode trait + H.264 impl (openh264), later HW encoders
  tlp-desk-input     # input injection trait + Windows impl (SendInput), key mapping
  tlp-desk-audio     # system audio loopback (cpal/WASAPI) + Opus
  tlp-desk-files     # file transfer + clipboard sync (arboard)
  tlp-desk-host      # host session logic, permissions, Windows service, desktop switching
  tlp-desk-viewer    # viewer session logic, frame pipeline, input capture
  tlp-desk-app       # the single GUI binary (egui/eframe + wgpu), tray icon, settings
  tlp-desk-cli       # headless CLI (list devices, connect, send file) for scripts/agents
  tlp-desk-android   # Android viewer (cdylib, NativeActivity + egui touch UI); viewer only, no host
  tlp-desk-testkit   # dev-only (publish = false): fakes + host/viewer pair harness for tests
installer/           # Inno Setup setup.exe + portable zip build scripts
docs/                # protocol.md, security.md, user-guide (Arabic + English)
```

## Repository layout (strict)

Every file has exactly one place. **Never add a new top-level file or folder without updating this
section in the same commit.**

| What | Where |
|---|---|
| Crate code | `crates/<crate>/src/`. One feature = one module folder `src/<feature>/mod.rs` (+ submodules); platform code in `src/<os>/` or `src/<feature>/windows.rs` |
| Integration tests | `crates/<crate>/tests/*.rs` (shared helpers in `tests/common/mod.rs`; cross-crate fakes and the host+viewer pair in the dev-only `tlp-desk-testkit`); hardware tests `#[ignore]` |
| Unit tests | `#[cfg(test)] mod tests` at the bottom of the file they test |
| Dev examples, benchmarks | `crates/<crate>/examples/` |
| App assets | `crates/tlp-desk-app/assets/{fonts,icon,icons}/` (licences next to the asset in `licenses/`) |
| Build scripts | `crates/<crate>/build.rs` (shared helper: `crates/tlp-desk-app/winres.rs`) |
| Installer | `installer/`: `TLP-Desk.iss`, `build-setup.ps1`, `portable.ps1`, `common.ps1`, `README-*.txt`, `assets/` (wizard images); `installer/macos/` (`build-app.sh`, `Info.plist.in`, `README-macos.txt`); `installer/linux/` (`build-tarball.sh`, `install.sh`, `TLP-Desk.desktop`, `README-linux.txt`) |
| Dev / CI scripts | `scripts/`, PowerShell named `verb-noun.ps1` (e.g. `check.ps1`, `vendor-icons.ps1`); `make_icon.py` is the one legacy exception |
| Documentation | `docs/`: `protocol.md`, `CODE_STYLE.md`, `book.toml`, `theme/`, `site/` (static landing page: `index.html`, `ar.html`, `style.css`, published at the Pages root with the book under `/book/`); mdBook sources in `docs/src/`: user guide in `docs/src/user-guide/en/` and `docs/src/user-guide/ar/` (same file names in both), contributor docs in `docs/src/dev/`, ADRs in `docs/src/adr/NNNN-title.md`, `security.md`, `glossary.md`, `changelog.md`; README media only in `docs/media/` |
| GitHub | `.github/`: `workflows/`, `ISSUE_TEMPLATE/`, `pull_request_template.md`, `CODEOWNERS`, `dependabot.yml` |
| Tool configs | dotfiles at the root: `.rustfmt.toml`, `.clippy.toml`, `.typos.toml`, `.gitignore`; cargo-deny in `.cargo/deny.toml` |
| Root files (only these) | `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `CLAUDE.md`, `ROADMAP.md`, `README.md`, `CONTRIBUTING.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md`, `LICENSE-MIT`, `LICENSE-APACHE`, `THIRD_PARTY_NOTICES.md` (the changelog lives in `docs/src/changelog.md`) |

Naming: Rust files and modules `snake_case.rs`; scripts and docs `kebab-case` (scripts `verb-noun.ps1`);
crates `tlp-desk-<concern>`; branches `TLP-<topic>`; the product is always written `TLP-Desk`.

Never write screenshots, build output, logs or temp files inside the repo: build output goes to
`CARGO_TARGET_DIR` (e.g. `C:\tlp-build\<lane>`), everything else to the OS temp dir. `target/`,
`dist/` and screenshot patterns are git-ignored as a safety net, not as a place to put things.

## README media

- Product screenshots and the demo video live **only** in `docs/media/`, and only as generated by
  `scripts/update-readme-media.ps1` (headless render of the real UI:
  `crates/tlp-desk-app/examples/readme_media`). Never commit any other screenshot.
- **Any PR that changes visible UI runs the script and commits the `docs/media/` changes in the
  same PR.** Output is deterministic, so an unchanged UI leaves git clean.
- A new screen worth showing (file transfer, chat, address book, ...) = one entry in
  `examples/readme_media/scenes.rs`; reference it from `README.md` or the book with a relative path.
- Budget: each PNG <= 400 KB, `docs/media/` <= 15 MB (the script fails otherwise).

## Tech choices (do not change without updating this file)

| Concern | Choice |
|---|---|
| Async runtime | `tokio` |
| Transport | `quinn` (QUIC), UDP port **47400**; control on a bidi stream, one uni stream per video frame, typed side streams (`StreamKind`: file data, tunnel, audio, clipboard) for feature data |
| Discovery | `mdns-sd`, service type `_tlpdesk._udp.local.`; UDP broadcast fallback on **47401**; manual IP always allowed |
| Serialization | `serde` + `postcard`, length-prefixed frames |
| Identity / TLS | `rcgen` self-signed cert per install, `rustls` custom verifier |
| Password auth | `spake2` (PAKE) — the password never crosses the wire |
| Capture (Win) | `windows` crate: Windows.Graphics.Capture, DXGI Desktop Duplication fallback |
| Video codec | `openh264` (software) first; Media Foundation / NVENC / QSV / AMF later behind a trait |
| Input (Win) | `windows` crate `SendInput`; Ctrl+Alt+Del via `SendSAS` from the service |
| Clipboard | `arboard` |
| Audio | `cpal` (WASAPI loopback) + `opus` |
| GUI | `egui` / `eframe` with `wgpu` backend; video frames uploaded as textures |
| Service | `windows-service` crate |
| Logging | `tracing`, local rotating log files only |
| Installer | Inno Setup 6 single setup.exe (Arabic+English), cargo-wix MSI later if needed + portable zip |

## Security model (must hold at all times)

- Each install generates a long-term key pair on first run, stored in `%ProgramData%\TLP-Desk\` (ACL: SYSTEM + Administrators).
- **Device ID** = 9 digits derived from SHA-256 of the public key. Connecting by ID therefore pins the
  key: a viewer must reject any host whose cert does not hash to the ID it dialed (MITM-proof on LAN).
- When connecting by IP/hostname only, use trust-on-first-use and warn loudly if the key changes.
- Host authorizes a session by one of: (a) user clicks **Accept** on the host, (b) one-time 6-digit code
  that rotates after each use, (c) permanent unattended-access password (Argon2 hashed at rest).
- Password attempts: max 5 per minute per peer IP, then 10-minute lockout. Log every attempt.
- Every session has a permission set: view, control, keyboard, clipboard, files, audio, restart,
  privacy mode. The host can revoke any permission live. A visible on-screen banner on the host shows
  who is connected, always.
- Optional LAN allow-list (CIDR ranges). Default: only RFC1918 + link-local peers may connect.

## Performance targets (LAN, 1 Gbps or good Wi-Fi)

- 1080p @ 60 fps, glass-to-glass latency < 60 ms (measure with the built-in latency overlay).
- Adaptive: drop frames instead of queuing when the viewer falls behind; never let latency grow.
- Idle host CPU < 1%; active session < 15% on a mid-range CPU with software encoding.

## Conventions

- Rust stable, edition 2021, `#![forbid(unsafe_code)]` everywhere except platform modules that wrap
  OS APIs — those must document every `unsafe` block with a `// SAFETY:` comment.
- Errors: `thiserror` in libraries, `anyhow` only in binaries.
- All user-facing strings go through the i18n table; ship Arabic (RTL) and English.
  Arabic is first-class: any feature that displays, stores or transfers text must work in Arabic
  (shaping, bidi, RTL layout, UTF-8/UTF-16 paths) before it is done — see "Arabic" in ROADMAP.md.
- Commit messages: `TLP-<area>: what changed` (e.g. `TLP-proto: add v4 messages`), small and often.
- Branches: every branch is named `TLP-<topic>` (e.g. `TLP-files`); the main branch is `TLP-Desk`.
- Build output may live outside the repo: set `CARGO_TARGET_DIR` (e.g. `C:\tlp-build\<lane>`);
  `scripts\check.ps1` and the installer scripts honour it. Never commit `target/` or `dist/`.

## Code style

Full guide: `docs/CODE_STYLE.md` (read it before your first change). The ten rules that matter most:

1. **Never crash.** No `unwrap`/`expect`/`panic!`/`todo!`/`unreachable!` in non-test code on anything
   input can reach; return the crate's error with `?`. Every crash fix ships a regression test.
2. **Every byte from a peer is hostile.** `get()` not `[i]`, checked/saturating arithmetic, no
   truncating `as` casts, cap allocations and queues *before* allocating, bound loops and recursion.
3. **Errors:** one `thiserror` enum per library crate (lowercase messages that say what was wrong),
   `anyhow` + `.context()` only in `tlp-desk-app` / `tlp-desk-cli`.
4. **`unsafe` only in platform modules** (`#[allow(unsafe_code)]` on that module), each block with a
   `// SAFETY:` comment directly above it; pure crates keep `#![forbid(unsafe_code)]`.
5. **Workspace inheritance:** every crate uses `*.workspace = true` for package fields and shared
   deps, and `[lints] workspace = true`. New deps: permissive licence, crates.io, `cargo deny check`.
6. **Docs:** `//!` crate doc with entry points and boundaries; `///` on every public item; comments
   say *why*. Never log secrets (passwords, codes, keys, clipboard or file contents).
7. **Small modules, small surface:** new feature = new module; files past ~800 lines get split;
   `pub use` the API from the crate root, everything else `pub(crate)`. Newtypes for ids and units.
8. **Tests at the right scale** (unit, proptest, `tests/` integration, host e2e); a bug-fix test must
   be seen failing before the fix; Arabic input in every text-handling test; loopback and temp dirs only.
9. **Gate before every commit:** `cargo fmt --all -- --check`,
   `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
   (`-p <crate>` while iterating). Never `--no-verify`.
10. **Commits:** `TLP-<area>: what changed`, one green concern per commit, stage exact paths (never
    `git add -A`), check `git diff --staged` first, rebase on `TLP-Desk` the day it moves.
