# Building on Windows

Windows 10/11 x64 is the tier-1 platform. Everything below uses PowerShell.

## Prerequisites

| Tool | Why | How |
|---|---|---|
| **Rust stable**, at least 1.95 | `rust-toolchain.toml` pins the `stable` channel with `clippy` and `rustfmt`; `egui`/`eframe` 0.36 need 1.95 (the highest `rust-version` in the dependency graph) | [rustup](https://rustup.rs), default host `x86_64-pc-windows-msvc` |
| **Visual Studio Build Tools** with "Desktop development with C++" | MSVC linker; the C++ compiler builds the bundled OpenH264 sources | Visual Studio Installer |
| **Windows 10/11 SDK** | `rc.exe` embeds the icon and version info into the executables | Included in the C++ workload |
| **NASM** (optional, recommended) | OpenH264's assembly fast paths. Without it the build prints "Failed to compile NASM files, not using any assembly." and still works, just slower | Put `nasm.exe` on `PATH`; the installer scripts also look in `%LOCALAPPDATA%\bin\NASM` |
| **Inno Setup 6.6+** (only for `setup.exe`) | Builds the installer | `winget install --id JRSoftware.InnoSetup -e` |

Set `OPENH264_NO_ASM=1` to force a build without assembly (useful to compare performance).

## Build

```powershell
git clone https://github.com/Tamer-Mansour/TLP-Desk.git
cd TLP-Desk
cargo build --release
```

The binaries are `target\release\TLP-Desk.exe` (GUI) and `target\release\TLP-Desk-cli.exe`.

The release profile uses thin LTO and one codegen unit, so a clean release build takes a while;
use `cargo build` (debug) for quick iterations, but measure performance only in release — the
H.264 encoder and colour conversion are many times slower in debug.

### Keep build output on another drive

The `target` directory grows to many gigabytes. If your source drive is small or slow, point Cargo
somewhere else:

```powershell
$env:CARGO_TARGET_DIR = 'C:\tlp-build\target'          # this shell only
[Environment]::SetEnvironmentVariable('CARGO_TARGET_DIR', 'C:\tlp-build\target', 'User')  # permanently
```

The installer scripts honour `CARGO_TARGET_DIR` when they collect the binaries.

### Offline builds

TLP-Desk itself never needs the internet, and neither does a build once dependencies are in the
Cargo cache. The packaging scripts call `cargo build --release --offline`; run `cargo fetch` once
while online. The icon and version resources are embedded without extra crates (`winres.rs`), so
nothing is downloaded at build time.

## Icons and version info

`crates/tlp-desk-app/winres.rs` (included by the app's and the CLI's `build.rs`) finds `rc.exe` from
`$RC`, then `PATH`, then the newest `Windows Kits\10\bin\<version>\<arch>\`. If it is missing:

- normally the build continues with a `cargo:warning` and the `.exe` has no icon;
- with `TLP_DESK_REQUIRE_EXE_ICON=1` (set by `installer\build-setup.ps1`) the build fails.

The icon files are committed. To regenerate them from `scripts/make_icon.py` you need Python with
Pillow:

```powershell
python -m venv .venv
.venv\Scripts\pip install pillow
.venv\Scripts\python scripts\make_icon.py
```

## Running

```powershell
cargo run --release -p tlp-desk-app                 # GUI
cargo run --release -p tlp-desk-app -- --fake       # GUI with fake data, no network
cargo run --release -p tlp-desk-app -- --lang ar    # Arabic UI
cargo run --release -p tlp-desk-cli -- serve --code # headless host
```

Two instances on one PC cannot both listen on UDP 47400; the second becomes dial-only and shows a
"port in use" banner. Use `--port` on one of them to test both roles locally.

The first time the GUI listens, Windows Firewall asks whether to allow it. Allow **Private
networks** only. Installed builds get firewall rules from `setup.exe` instead.

## Packaging

See [Release process](release.md) for `installer\build-setup.ps1` (setup.exe),
`installer\portable.ps1` (portable zip) and `SHA256SUMS.txt`.

## Troubleshooting the build

| Symptom | Fix |
|---|---|
| `link.exe` not found, or `cc` cannot find a C++ compiler | Install the Visual Studio C++ build tools; open a new shell |
| Warning: exe will have no icon/version info | Install the Windows SDK, or set `RC` to the full path of `rc.exe` |
| "Failed to compile NASM files" | Optional: install NASM and put it on `PATH` |
| `rustc` too old | `rustup update stable` (1.95 or later) |
| Disk full under `target\` | Set `CARGO_TARGET_DIR` to another drive; `cargo clean` |
