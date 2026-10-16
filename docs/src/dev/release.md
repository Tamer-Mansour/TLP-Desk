# Release process

TLP-Desk ships two artifacts for Windows x64, plus a checksum file:

| Artifact | Built by | Contents |
|---|---|---|
| `TLP-Desk-<version>-setup.exe` | `installer\build-setup.ps1` (Inno Setup) | Installs to `C:\Program Files\TLP-Desk`, Start-menu shortcut, firewall rules, uninstaller |
| `TLP-Desk-<version>-win-x64-portable.zip` | `installer\portable.ps1` | `TLP-Desk.exe`, `TLP-Desk-cli.exe`, `README.txt`, `licenses\` — runs without installing (attended mode) |
| `SHA256SUMS.txt` | both scripts | `<sha256>  <file name>` per artifact, sorted, LF line endings |
| `SHA256SUMS.txt.minisig` | `scripts\sign-release.ps1` | ed25519 signature of `SHA256SUMS.txt` (minisign format) for the in-app updater |

All three are written to `dist\` (git-ignored).

> **Status:** no version has been tagged yet. Pushing a tag `v*` runs
> `.github/workflows/release.yml`, which builds both artifacts with the same scripts on a Windows
> runner (NASM and Inno Setup installed) and attaches them with `SHA256SUMS.txt` to a **draft**
> GitHub Release. The scripts below also work locally.

## Version number

The single source is `[workspace.package] version` in the root `Cargo.toml` (currently `1.0.0-rc.1`).
Every crate inherits it; the installer scripts read it from there, `winres.rs` writes it into the
executables' VERSIONINFO, and the setup's file name uses it. The wire protocol has its own
`PROTOCOL_VERSION`, independent of the app version.

## Checklist

1. **Update the changelog.** Move the `Unreleased` entries in `docs/src/changelog.md` under a new
   `[x.y.z] - YYYY-MM-DD` heading; call out any `PROTOCOL_VERSION` change ("cannot connect to
   older versions").
2. **Bump the version** in the root `Cargo.toml` and run `cargo build` so `Cargo.lock` follows.
3. **Run the gate:** `pwsh scripts/check.ps1` (fmt, release build, clippy `-D warnings`, tests),
   `cargo deny check`, `typos`.
4. **Run the hardware tests** on a real desktop (see [Testing](testing.md#ignored-hardware-tests))
   and a two-PC manual session.
5. **Build the artifacts** (needs NASM for a fast encoder and the Windows SDK for icons):

   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File installer\build-setup.ps1
   powershell -NoProfile -ExecutionPolicy Bypass -File installer\portable.ps1
   ```

   `build-setup.ps1` sets `TLP_DESK_REQUIRE_EXE_ICON=1`, so a build without `rc.exe` fails
   instead of producing an exe without an icon. Both scripts build with `--offline`, honour
   `CARGO_TARGET_DIR`, and write to `TLP_DESK_DIST_DIR` instead of `dist\` when it is set.

   **Signing:** with a certificate configured (`TLP_SIGN_*` variables, see
   [Code signing](code-signing.md)), both scripts sign `TLP-Desk.exe` and `TLP-Desk-cli.exe`, and
   Inno signs the setup and its uninstaller; set `TLP_SIGN_REQUIRED=1` for a release so a signing
   problem stops the build. Without a certificate the scripts print that the build is unsigned.
6. **Smoke-test the installer**: install, launch, check the firewall rules exist
   (`netsh advfirewall firewall show rule name="TLP-Desk (QUIC)"`), uninstall, check they are gone.
   Silent mode: `TLP-Desk-<version>-setup.exe /VERYSILENT /SUPPRESSMSGBOXES` (standard Inno Setup
   switches; firewall rules are added in silent mode too, the app is not launched).
   **Sign the manifest** (after both scripts, whenever `SHA256SUMS.txt` changes):
   `$env:TLP_RELEASE_SIGNING_KEY = '<path to release.key>'; scripts\sign-release.ps1`. Without
   `SHA256SUMS.txt.minisig` the in-app updater refuses to install the release (it still offers
   the release page). See [below](#signing-the-release-manifest-in-app-updater).
7. **Commit** the version bump and changelog: `TLP-release: x.y.z`.
8. **Tag** the commit on `TLP-Desk`: `git tag -a vx.y.z -m "TLP-Desk x.y.z"` and push the tag.
9. **Publish.** The release workflow creates a draft GitHub Release for the tag with the setup,
   the portable zip and `SHA256SUMS.txt`. Check that the version in the file names matches the
   tag (the version comes from `Cargo.toml`, not from the tag), paste the changelog section as
   the description, include the SHA256 lines, and publish the draft. Without CI, build with the
   scripts and upload the three files from `dist\` by hand.
10. **winget manifest.** After publishing, run
    `scripts\update-winget-manifest.ps1 -Tag vx.y.z` (downloads the setup, checks its SHA-256,
    updates `installer\winget\`, runs `winget validate`) and commit the result. Submitting it to
    `microsoft/winget-pkgs` is a separate, manual step; see
    [Code signing, SmartScreen and winget](code-signing.md#winget-install-without-the-warning)
    (including the note on pre-release versions).

## What the installer does

From `installer\TLP-Desk.iss`:

- Per-machine install (requires admin), x64 only, Windows 10 or later, into
  `{autopf}\TLP-Desk`.
- Wizard in **English or Arabic**, preselected from the Windows UI language.
- A fixed `AppId` so upgrades replace the previous install — never change it.
- Closes a running TLP-Desk before replacing files.
- Start-menu shortcut; optional desktop shortcut (unchecked by default).
- Windows Firewall rules, **private and domain profiles only** (never Public):

  | Rule | Program | Port |
  |---|---|---|
  | `TLP-Desk (QUIC)` | `TLP-Desk.exe` and `TLP-Desk-cli.exe` | UDP 47400 |
  | `TLP-Desk (discovery)` | `TLP-Desk.exe` | UDP 47401 |
  | `TLP-Desk (mDNS)` | `TLP-Desk.exe` | UDP 5353 |

  Existing rules with these names (and the lowercase names of pre-rename builds) are deleted
  first, so reinstalling does not create duplicates.
- **Uninstall** removes the program and the firewall rules but deliberately **keeps**
  `%ProgramData%\TLP-Desk` (identity key, known hosts) and `%APPDATA%\TLP-Desk` (settings), so a
  reinstall keeps the same device ID.

The pipeline can sign everything (exes, setup, uninstaller), but no certificate is configured yet,
so releases are **not code-signed** and SmartScreen warns on first run; the user guide explains
"More info → Run anyway" and the winget route. What to buy and how to set it up:
[Code signing, SmartScreen and winget](code-signing.md). See also
[ADR 0008](../adr/0008-inno-setup-instead-of-msi.md).

## Signing the release manifest (in-app updater)

The in-app updater (`crates/tlp-desk-app/src/update/`) installs a release only when
`SHA256SUMS.txt.minisig` verifies against the public key embedded in
`crates/tlp-desk-app/src/update/pubkey.rs` **and** the setup's SHA-256 matches the signed
`SHA256SUMS.txt`. The signature is minisign-compatible (prehashed ed25519, `ED`), so
`minisign -Vm SHA256SUMS.txt -P <public key>` verifies it too.

- **Key generation (once):** `scripts\gen-release-key.ps1 -OutDir <folder outside the repo>`
  writes `release.key` (secret) and `release.pub`, and prints the public key line for
  `pubkey.rs`. The script refuses to write inside the repository.
- **Where the secret lives:** the GitHub Actions secret **`RELEASE_SIGNING_KEY`** (contents of
  `release.key`, or just its base64 line) plus an offline backup with the maintainer. It is never
  committed. `release.yml` runs `scripts/sign-release.ps1` with it after building the artifacts
  and attaches `SHA256SUMS.txt.minisig`; the step is skipped when the secret is absent.
- **Signing locally:** set `TLP_RELEASE_SIGNING_KEY` to the key file path (or base64) and run
  `scripts\sign-release.ps1 [-Sums dist\SHA256SUMS.txt]`. It signs with the trusted comment
  `TLP-Desk <version> SHA256SUMS.txt` and then verifies the result against the key embedded in
  the app, so a wrong key fails here instead of on users' PCs.
- **Rotating the key:** ship a release whose `pubkey.rs` holds the new key (signed with the old
  key), then sign later releases with the new key. Installs older than that release can only
  update by hand.

The tool behind both scripts is `crates/tlp-desk-app/examples/release_sign.rs`
(`keygen`, `sign`, `verify`).

## Verifying a download

Users can check a file against `SHA256SUMS.txt`:

```powershell
Get-FileHash .\TLP-Desk-1.0.0-rc.1-setup.exe -Algorithm SHA256
```

The hash must match the line for that file name (case-insensitive).
