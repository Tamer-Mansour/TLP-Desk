# TLP-Desk code style

How we write, test and ship code in this repository. `CLAUDE.md` holds the product rules (LAN-only,
security model, tech choices); this file holds the engineering rules. When the two disagree,
`CLAUDE.md` wins and this file gets fixed.

**Where these rules come from.** We took the engineering habits of the ArtCraft "Crafting Apps"
organisation on GitHub ([storytold](https://github.com/storytold)) as our reference for clean Rust:
`photocraft`, `pdfcraft`, `gridcraft`, `craft-libs` and `craft-repo-status-app`. Each rule says which
repository it came from, e.g. *(photocraft)*. We adopted *patterns*, not code: nothing here is
copied from those repositories, and everything is adapted to TLP-Desk (Windows-first, LAN-only,
Arabic first-class, a network peer on the other side of every byte).

Licences of the reference repositories, checked 2026-10-13: `photocraft`, `pdfcraft`, `gridcraft` and
`craft-libs` are dual **MIT OR Apache-2.0** (they ship `LICENSE-MIT` and `LICENSE-APACHE`; GitHub
lists them as Apache-2.0). `craft-repo-status-app` has **no licence file** (all rights reserved), so
we only looked at its structure and copied nothing from it.

---

## 1. Repository layout

```text
Cargo.toml          workspace: members, [workspace.package], [workspace.dependencies], [workspace.lints]
.clippy.toml        lint tuning (tests may unwrap); lint *levels* live in Cargo.toml
.rustfmt.toml       stable rustfmt options only
.cargo/deny.toml    cargo-deny: licences, sources, advisories
.typos.toml         spell checker allow-list
rust-toolchain.toml stable + clippy + rustfmt
crates/tlp-desk-*   one crate per concern (see CLAUDE.md "Workspace layout")
crates/*/tests/     integration tests (public API only)
crates/*/examples/  runnable tools and benchmarks (e.g. codec `bench`)
docs/               protocol.md, CODE_STYLE.md, user guides (en + ar)
installer/          Inno Setup + portable zip
scripts/            Windows build/check helpers (PowerShell)
```

Rules:

- **All shared metadata is inherited.** Every crate's `Cargo.toml` uses `version.workspace = true`,
  `edition.workspace = true`, `license.workspace = true`, `rust-version.workspace = true`, takes
  every dependency it shares with another crate from `[workspace.dependencies]` (`foo.workspace =
  true`), and has `[lints] workspace = true`. *(craft-libs, pdfcraft)*
- **Internal crates are workspace dependencies with a path and a version**
  (`tlp-desk-net = { path = "crates/tlp-desk-net", version = "0.1.0" }`). *(pdfcraft)*
- **Machine names are lowercase, product name is `TLP-Desk`.** Crates, binaries, files and ids are
  `tlp-desk-*` / `tlp_desk_*`; user-facing text (window titles, installer, About, docs prose) says
  `TLP-Desk`. *(photocraft: lowercase crate names vs. the PascalCase product name)*
- **A new crate** goes in `crates/tlp-desk-<name>`, gets a one-line `description` in its manifest, a
  crate-level `//!` doc, and is added to `[workspace.dependencies]` and to CLAUDE.md's layout. A
  crate with a non-trivial public API gets a `README.md` that says what it does, its boundaries
  (what it must *not* depend on or do) and how to use it. *(craft-libs `settings` README)*
- **Layering is a rule, not a hope.** Lower crates never depend on higher ones:
  `proto` and `crypto` (L0) <- `net`, `capture`, `codec`, `input`, `audio`, `files` (L1) <-
  `host`, `viewer` (L2) <- `app`, `cli` (L3). Nothing below L3 depends on `egui`/`eframe`/`winit`.
  *(photocraft and gridcraft enforce this with `cargo xtask layers`; we enforce it in review until we
  add an `xtask`, see "Targets")*
- **Keep `Cargo.toml` files valid at every moment.** The `crates/*` glob means one broken manifest
  breaks every agent's build. Edit manifests in one write, never leave a half-edited one. *(photocraft)*
- **Large binaries never go in git**: test captures, screenshots, recordings, corpora. Generate
  fixtures in code or fetch them pinned by hash. *(pdfcraft, photocraft corpus policy)*

## 2. Never crash, never hang the session

A malformed packet, a hostile peer, a corrupt settings file, an unplugged cable or a full disk must
end in an error the user can act on (or a clean session close), never a panic, and never take the
host's desktop down with it. **This outranks feature work.** *(the "never crash" rule shared by
photocraft, pdfcraft, gridcraft and craft-libs)*

- **No panicking shortcuts in non-test code**: no `unwrap()`, `expect()`, `panic!`, `todo!`,
  `unimplemented!`, `unreachable!` on anything input can reach. Propagate with `?`, use
  `ok_or(..)?`, `let .. else { return Err(..) }`, or `unwrap_or*` when the fallback is genuinely
  right. The one exception is a provably infallible call, written as `expect("why it cannot fail")`.
- **Everything from the wire is hostile.** Sizes, counts, offsets, indices, rectangles, monitor ids,
  string lengths and file paths that came from a peer are validated before use:
  `get()` instead of `[i]`, `checked_*`/`saturating_*` arithmetic, no `as` casts that can truncate or
  go negative, char-boundary-safe string slicing, allocation sizes capped *before* allocating.
- **Bound everything**: queue lengths, retry loops, recursion depth (directory walks in
  `tlp-desk-files`), frame sizes. A full queue drops the oldest frame (CLAUDE.md performance rule);
  it never grows.
- **Don't cascade.** Recover poisoned locks (`lock().unwrap_or_else(PoisonError::into_inner)`),
  treat `JoinHandle::join` and task results as `Result`, and keep a panic in one worker
  (capture, encode, audio) from killing the session. Keep `panic = "unwind"`.
- **Prove it.** Every crash fix lands with a small synthetic regression test that panicked before
  the fix. *(pdfcraft, photocraft)*

```rust
// Bad: a peer controls `len`.
let name = &buf[4..4 + len];

// Good: bounded, checked, and the error says what was wrong.
let end = 4usize.checked_add(len).ok_or(ProtoError::TooLong)?;
let name = buf.get(4..end).ok_or(ProtoError::Truncated { need: end, have: buf.len() })?;
```

## 3. Errors

- **Libraries: one `thiserror` enum per crate** (`ProtoError`, `NetError`, `CaptureError`, ...),
  in `error.rs` or at the top of `lib.rs`, re-exported from the crate root. **Binaries
  (`tlp-desk-app`, `tlp-desk-cli`): `anyhow`**, with `.context("...")` at each boundary.
  *(pdfcraft `CosError`, craft-libs `SettingsError`)*
- **Messages are lowercase, no trailing period, and say what was wrong and what was expected**
  (`"frame of {got} bytes exceeds the {max}-byte limit"`, not `"bad frame"`).
- **Variants carry data, not formatted strings,** so callers can match on them
  (`PinMismatch { expected, actual_id, actual_fp }`). Add `#[from]` only for a wrapped error that
  has exactly one meaning in this crate.
- **Security-relevant variants are documented** with what the caller must do (e.g. "possible MITM:
  do not retry silently").
- **Errors shown to the user go through i18n** (see section 9); the `Display` text of an error is
  for logs and developers.
- **Lenient where the spec allows, never silent.** If you repair or ignore bad input (an unknown
  message tag from a newer peer, a settings key we do not know), log it once at `debug!`/`warn!`.
  *(pdfcraft "never silently repair")*

## 4. `unsafe`

- The workspace sets `unsafe_code = "deny"`. **Pure crates** (`proto`, `crypto`, `net`, `host`,
  `viewer`, `files`, `audio`, `app`, `cli`) additionally put `#![forbid(unsafe_code)]` in `lib.rs` /
  `main.rs`, so nothing can opt back in.
- **Platform crates** (`capture`, `input`, `codec`) keep `#![deny(unsafe_code)]` at the crate root
  and `#[allow(unsafe_code)]` only on the module that wraps the OS API (`windows_capture`, `win`,
  `h264::raw`). Keep that module small and expose a safe API from it.
- **Every `unsafe` block has a `// SAFETY:` comment directly above it** that states the invariant
  that makes it sound (who owns the pointer, how long it lives, why the length is right). Clippy's
  `undocumented_unsafe_blocks` enforces the "directly above" part, so hoist the call into a `let`
  when it sits inside another expression:

  ```rust
  // SAFETY: `info` is a valid, writable MONITORINFO with `cbSize` set; `hmon` came from
  // EnumDisplayMonitors in this call.
  let ok = unsafe { GetMonitorInfoW(hmon, &mut info) };
  ```
- `unsafe impl Send/Sync` needs the same comment, explaining why the type is safe to move/share.
- *(photocraft: exactly one isolated crate/module may use `unsafe`, crate-wide `deny` plus a local
  allow, every block with a SAFETY note)*

## 5. Logging

- `tracing` only, never `println!`/`eprintln!` outside `tlp-desk-cli` output and `examples/`.
- Levels: `error!` = the session or a feature failed; `warn!` = something the user or an admin should
  know (auth failure, key change, permission revoked); `info!` = session lifecycle (connect, accept,
  disconnect); `debug!` = per-message detail; `trace!` = per-frame detail.
- Use structured fields: `info!(peer = %addr, device = %id, "session accepted")`.
- **Never log secrets**: passwords, one-time codes, SPAKE2 messages, private keys, session tokens,
  clipboard contents, file contents. Log the *fact* (`"password attempt failed"`), the peer and the
  count. *(pdfcraft: never log the control-channel token)*
- Logs are local files only (CLAUDE.md rule 2).

## 6. Documentation and comments

- **Every crate starts with a `//!` doc** that says what the crate is for, its main entry points as
  intra-doc links, and its hard boundaries (e.g. "No function in this crate contacts anything
  outside the local network"). *(pdfcraft `cos`, craft-libs `settings`; already our pattern in
  `tlp-desk-net`)*
- **Every public item gets a `///` doc**: one sentence for simple items; for functions that can
  fail, say when. Struct fields and enum variants get a short doc when the name alone does not say
  the unit or the meaning (`/// Milliseconds since the session started.`). *(craft-libs `error.rs`:
  every field documented)*
- **Comments explain *why*, not *what***. A comment that restates the code is noise; a comment that
  records a measured number, an OS quirk or a rejected alternative is gold. *(photocraft
  contributing guide)*
- **Config files explain themselves**: every non-obvious setting in `Cargo.toml`, `.cargo/deny.toml`,
  `.clippy.toml` has a one-line comment saying why it is there, and every advisory ignore has a
  reason and a "revisit when". *(photocraft profile comments, pdfcraft deny.toml)*
- Doc examples compile (`cargo test --doc`) or are marked `no_run` / `ignore` with a reason.

## 7. Naming, types and API shape

- Standard Rust naming (RFC 430). Modules and files `snake_case`, one concept per file.
- **Newtypes for anything with a unit or a meaning**: `DeviceId`, `Fingerprint`, `SessionId`,
  `MonitorId`, not bare `u32`/`String`. Validate in the constructor (`DeviceId::parse`) so an invalid
  value cannot exist.
- **Enums over booleans** for anything with more than one meaning
  (`AuthMethod::{Accept, OneTimeCode, Password}`), and over stringly-typed states.
- **Config structs with `Default`** and public fields (or a small builder) instead of functions with
  many parameters. Settings structs that are persisted use `#[serde(default)]` on every struct and
  never `deny_unknown_fields`, so older and newer files still load. *(craft-libs `settings`)*
- **Keep the public surface small**: `mod foo;` + `pub use foo::{Thing, OtherThing};` at the crate
  root; everything else `pub(crate)`. *(pdfcraft `cos` lib.rs)*
- **Trait at the seam, platform code behind it**: `Capturer`, `Encoder`, `InputInjector`, and a
  `fake` implementation for tests and for macOS/Linux stubs (already the pattern in `tlp-desk-app`'s
  `backend::{real, fake}`).
- **Wire types live in `tlp-desk-proto` only.** Any change there bumps `PROTOCOL_VERSION`, updates
  `docs/protocol.md` and adds a round-trip test (CLAUDE.md rule 6).

## 8. File and module size

- Aim for files under ~600 lines of non-test code. When a file passes ~800, split it by concern
  (`endpoint.rs` -> `endpoint/{accept,dial,auth}.rs`) in a commit of its own that changes no
  behaviour.
- **Put new features in a new module** rather than growing a shared file that several agents edit
  (`lib.rs`, `app.rs`, `messages.rs`). Shared files get small, surgical edits. *(photocraft
  "parallel agents": new commands go in a new module)*
- Functions: if it needs a comment to separate its "phases", it wants to be several functions.

## 9. Arabic and i18n (first-class)

- **No user-facing string literal outside the i18n table**, in either language. Keys are stable
  `snake_case` ids; English and Arabic ship together in the same commit.
- **Text is `String`/`&str` (UTF-8) internally**; convert to UTF-16 only at the Win32 boundary, and
  paths go through `Path`/`OsStr`, never through lossy `to_string_lossy()` when the result is used
  again.
- **Every feature that shows, stores or sends text has a test with Arabic input**: a file name, a
  device name, a clipboard string, a chat message (`"ملف تجريبي.txt"`). Mixed Arabic/Latin and
  digits too, because that is where bidi bugs live.
- Layout: no hard-coded left/right; use the layout direction from the theme so RTL mirrors.
- *(photocraft and gridcraft keep a per-language localisation parity checklist; ours is the "Arabic"
  section of ROADMAP.md)*

## 10. Tests

**Done = tested** (CLAUDE.md rule 5). Every change comes with tests at the right scale.
*(photocraft "tests are the gate", pdfcraft "tests mirror the zooms")*

| Scale | Where | Example |
|---|---|---|
| Unit | `#[cfg(test)] mod tests` at the bottom of the file | `DeviceId` parsing, CIDR matching, key mapping |
| Property | same, with `proptest` | any postcard message round-trips; decoder never panics on random bytes |
| Crate integration | `crates/<crate>/tests/*.rs`, public API only | `net/tests/session.rs` dials a real loopback endpoint |
| End-to-end | `crates/tlp-desk-host/tests/e2e.rs` | host + viewer on loopback, auth, frames, input |
| Real hardware / desktop | `tests/real_*.rs` with `#[ignore]` and a doc line saying how to run it | `input/tests/real_injection.rs` |

Rules:

- **A bug fix's test must fail before the fix.** Revert the fix, watch the test go red, restore it.
  A test nobody has seen fail proves nothing. *(pdfcraft)*
- **Malformed-input tests for every decoder**: truncated, oversized, wrong tag, wrong version,
  random bytes. The result is `Err`, never a panic.
- **Security tests are first-class**: wrong pin is rejected, wrong password is rejected and rate
  limited, a revoked permission stops the stream, a non-RFC1918 peer is refused.
- **Tests never touch the real profile or the network beyond loopback**: use temp dirs, ephemeral
  ports (`127.0.0.1:0`), and the `fake` backends. Tests that need a real desktop are `#[ignore]`.
  *(craft-libs `settings` tests write only to temp folders)*
- **Deterministic**: no sleeps as synchronisation, seeded randomness, timeouts on every await in
  tests so a hang fails instead of stalling CI.
- Shared test helpers live in `tests/common/mod.rs` of the crate that needs them. *(craft-libs)*
- Benchmarks and measuring tools live in `examples/` (`codec/examples/bench.rs`) and their numbers
  go into the PR description or ROADMAP evidence. *(photocraft records before/after timings)*
- While iterating, scope to your crate: `cargo test -p tlp-desk-<crate>`; the workspace run is the
  final gate. *(pdfcraft, photocraft)*

## 11. Dependencies

- **Add a dependency to `[workspace.dependencies]`** when a second crate needs it; pin to a major
  (`"1"`) or minor (`"0.36"`) version, never `"*"`.
- **Permissive licences only**: MIT, Apache-2.0, BSD-2/3, ISC, Zlib, Unicode-3.0, BSL-1.0. No GPL,
  LGPL, AGPL or MPL in the binary. `cargo deny check` enforces it (`.cargo/deny.toml`). *(pdfcraft
  deny.toml)*
- **crates.io only.** A git dependency needs an immutable `rev` and an `allow-git` entry with a
  comment. *(photocraft / pdfcraft pin `craft-libs` by commit)*
- **No dependency that phones home** (telemetry, update checks, crash upload) or that pulls in an
  HTTP client for something we do on the LAN.
- Turn off default features you do not use (`default-features = false`), and say in a comment why a
  non-obvious feature is on. *(photocraft's workspace dependency comments)*
- Every `[advisories] ignore` entry has a reason and a "revisit when".

## 12. Lints and formatting

Enabled now (root `Cargo.toml`, `[workspace.lints]`; all crates opt in):

| Lint | Level | Why |
|---|---|---|
| `unsafe_code` | deny | only platform modules opt back in (section 4) |
| `rust_2018_idioms` | warn | elided lifetimes in paths, `extern crate`, etc. |
| `clippy::undocumented_unsafe_blocks` | warn | every `unsafe` has a `// SAFETY:` note |
| `clippy::dbg_macro`, `clippy::todo`, `clippy::unimplemented` | warn | no debug leftovers or stubs in shipped code *(photocraft, pdfcraft, craft-libs)* |
| `clippy::mem_forget` | warn | leaks OS handles / encoder instances |

`-D warnings` in the gate turns every `warn` into an error. `.clippy.toml` lets tests `unwrap`,
`expect`, `panic` and index *(all four reference repos)*. `.rustfmt.toml` uses stable options only and
the default width.

**Targets** (not enabled yet because they need refactors; counts are from 2026-10-13). Enable one per
PR, crate by crate, by adding `#![deny(...)]` to that crate's root once it is clean, then promote to
the workspace when all crates are:

| Lint | Open hits | Notes |
|---|---|---|
| `clippy::unwrap_used`, `expect_used`, `panic` | ~33 | **denied (with `indexing_slicing`) at the roots of `proto`, `crypto` and `net`**; the never-crash set; new crates start with `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]` *(craft-libs, gridcraft)* |
| `clippy::indexing_slicing` | ~160 | most in pixel/YUV loops with checked dimensions; convert the wire-facing ones first |
| `clippy::cast_possible_truncation` | ~100 | `as` casts in capture/codec/input math |
| `missing_docs` | ~530 | mostly fields and variants; start with `proto` and `crypto` |
| `clippy::missing_errors_doc` | ~65 | `# Errors` sections on public fallible fns |
| `unreachable_pub` | ~35 | `pub` items not exported from the crate root |
| `unused_qualifications` | ~10 | trivial; do it in one sweep |
| `clippy::allow_attributes_without_reason` | ~6 | every `#[allow]` says why |

Also planned: `cargo xtask layers` (section 1), `cargo machete` for unused dependencies, and a wider
`max_width` only if the whole tree is reformatted in one dedicated commit.

## 13. Git, commits and PRs

- **Commit subject: `TLP-<area>: <what changed>`**, imperative or descriptive, no trailing period,
  under ~72 characters: `TLP-net: refuse peers outside the allow-list before the handshake`.
  `<area>` is the crate short name (`proto`, `net`, `capture`, ...), or `docs`, `style`, `repo`,
  `installer`, `ci`. Reference the roadmap item or issue in the subject or body when there is one.
  *(storytold uses one task id per commit, e.g. pdfcraft `M2: ...`, photocraft `fix(area): ...`)*
- **Body says why**, lists notable decisions, and names the evidence (test names, numbers).
- **Small, focused commits, each one green** (builds, clippy clean, tests pass). One concern per
  commit; a refactor and a behaviour change are two commits. *(photocraft, pdfcraft: "commit only
  green states")*
- **Branches: `TLP-<topic>`**; the integration branch is `TLP-Desk`. Rebase on it the same day it
  moves, and read every hunk you resolve: rebasing is a review step. *(pdfcraft)*
- **PRs are small**: the storytold apps merge PRs of typically 10-150 changed lines. Over ~400 lines
  needs a reason in the description (a generated table, a new crate skeleton).
- **Stage exact paths** (`git add <files>`), check `git diff --staged` before committing, never
  `git add -A`/`git add .`, never `--no-verify`, never force-push a shared branch.
- **PR description** answers: what changes and why; how you verified it (commands run, test names,
  measured numbers, screenshots for UI); and the checklist below. *(pdfcraft pull request template)*

PR checklist:

- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace` (or `-p` for the crates touched while iterating)
- [ ] New behaviour has a test at the right scale; a bug fix's test was seen failing first
- [ ] No new `unwrap`/`expect`/`panic!`/indexing on peer-controlled data; no new `unsafe` outside
      platform modules, every block has `// SAFETY:`
- [ ] No outbound network beyond the LAN; auth and encryption unchanged or strengthened
- [ ] Wire change: `PROTOCOL_VERSION` bumped, `docs/protocol.md` updated, round-trip test added
- [ ] User-facing text in English **and** Arabic through i18n; tested with Arabic input
- [ ] Clean-room: nothing read or copied from AnyDesk, RustDesk, TeamViewer or other remote
      desktop products; new dependency licences pass `cargo deny check`
- [ ] ROADMAP.md acceptance criteria ticked with evidence

## 14. Parallel agents

*(photocraft and pdfcraft run many agents on one repository; these are their working rules, adapted)*

- **Own target dir per agent**: `CARGO_TARGET_DIR=C:\tlp-build\<lane>`, and delete it when the lane
  is finished (each is several GB).
- **Edit only the crate you were assigned.** If someone else's half-finished edit breaks the build,
  wait and retry; do not fix their files.
- **Prefer a separate git worktree per agent** (`git worktree add`); several agents editing one
  working tree can overwrite each other's uncommitted changes.
- **Leave a trail**: update ROADMAP.md (what landed, evidence, what is still open) before you stop,
  so the next agent can pick up from a green tree.

## 15. Review at four zooms

Review each change as you make it, not only at the end *(pdfcraft)*:

1. **Line**: checked arithmetic, `get()` not `[i]`, the right error through `?`, `<` vs `<=`,
   byte vs char vs UTF-16 lengths.
2. **Function**: is it total? Name an input that panics or hangs: empty, zero, max, malformed,
   a peer that stops sending mid-message.
3. **Module**: one source of truth, no dead code, public surface still minimal, doc comment still
   true.
4. **Project**: layering, LAN-only, auth/pinning, permissions honoured live, Arabic, protocol
   version, and whether ROADMAP.md is still true.

## 16. CI (target)

The gate today is local (`scripts\check.ps1`, the commands in section 13). When we add GitHub
Actions, mirror the reference apps *(pdfcraft, photocraft, craft-libs CI)*:

- triggers: push to `TLP-Desk`, every pull request, manual; `concurrency` cancels superseded runs;
  `permissions: contents: read`.
- `windows-latest` job (tier 1): `cargo fmt --check`, `cargo clippy --workspace --all-targets
  --locked -- -D warnings`, `cargo test --workspace --locked`; Linux job for the platform-neutral
  crates (`proto`, `crypto`, `net`, `files`).
- separate jobs for `cargo deny check` and `typos`.
- `Swatinem/rust-cache`, `CARGO_TERM_COLOR=always`, `RUST_BACKTRACE=1`; third-party actions pinned
  by commit SHA with the version in a comment.
- release builds (installer + portable zip) only from tags, never on every push.
