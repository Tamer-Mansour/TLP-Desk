# Platform notes: macOS and Android

Platform code sits behind the same traits as on Windows (`ScreenCapturer`, `InputInjector`), in
`macos` / `macos_capture` modules, so the Windows build never compiles it.

## macOS (host and viewer)

| Concern | Implementation |
|---|---|
| Capture | `tlp-desk-capture/src/macos_capture`: `CGDisplayCreateImage` polled at up to 60 fps, BGRA, Retina frames at pixel size. `MonitorInfo` is in **points**, `frame_size()` in pixels. Cursor: position from `CGEventGetLocation`, one built-in arrow shape. |
| Input | `tlp-desk-input/src/macos`: `MacInjector` over `CGEventCreateMouseEvent` / `KeyboardEvent` / `ScrollWheelEvent2` posted at the HID tap, reusing `TrackingInjector`. Keys: Windows set-1 scancode to `kVK_*` (`keymap.rs`); modifier flags are stamped on every event; clicks are counted for double click (`pointer.rs`). |
| Permissions | `tlp-desk-input::permissions`: `CGPreflightScreenCaptureAccess` / `CGRequestScreenCaptureAccess`, `AXIsProcessTrusted(WithOptions)`, `x-apple.systempreferences:` deep links. The home screen shows the guidance card (`ui/permissions.rs`). |
| Data | `~/Library/Application Support/TLP-Desk` |
| Windows-only | service, privacy shield, pointer marker, whiteboard overlay, keyboard grab, local-activity monitor: stubs return `Unsupported`; the host runs without them. |
| Packaging | `installer/macos/build-app.sh` builds an ad-hoc-signed `.app` and a zip; the Gatekeeper steps are in `installer/macos/README-macos.txt`. |
| CI | `macos` job in `.github/workflows/ci.yml` (informational until its first green run). |

Why `CGDisplayCreateImage` and not ScreenCaptureKit: Apple labels it superseded, but it still works
and needs no Objective-C block/delegate plumbing, so it was the smallest correct first step.
Moving to `SCStream` only replaces `macos_capture/platform.rs`. Known limits: slower than
ScreenCaptureKit at high resolutions, and it cannot exclude windows from the picture.

### Cross-checking without a Mac

```powershell
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo clippy --target aarch64-apple-darwin -p tlp-desk-proto -p tlp-desk-input -p tlp-desk-capture -- -D warnings
```

These three crates are pure Rust on macOS and check without the Apple SDK. The others pull
`ring`, `blake3`, `opus` or `openh264`, whose build scripts need a C compiler for the target, so
they are only checked by the macOS CI job.

## Android (viewer only)

`crates/tlp-desk-android` is a `cdylib` (+ `rlib`) loaded by `NativeActivity`
(`android-activity` + eframe's `android-native-activity`). It reuses `tlp-desk-viewer` unchanged and
has its own slim egui UI. Android cannot host: there is no unattended capture or injection API for
a normal app.

| Module | What it does | Tested on Windows |
|---|---|---|
| `gesture` | touch recognizer (tap, long press, drag, two-finger scroll, pinch) and the mapper to `InputEvent` | yes, unit tests |
| `view` | zoom/pan of the picture | yes |
| `keyboard` | soft-keyboard text to US-layout scancodes; Arabic and other characters via clipboard + Ctrl+V | yes |
| `i18n` | Arabic/English; includes the desktop app's `i18n/shaping.rs` by path | yes |
| `session` | tokio runtime + `Viewer::connect` + state for the UI | partly |
| `ui` | connect form, session view | compiles; not run |

### Building the APK on Windows

One-time setup (nothing needs admin rights; everything lives under one folder, here
`C:\tlp-build\android`):

1. JDK 17 (zip), Android command-line tools (zip), then
   `sdkmanager --sdk_root=<sdk> "platforms;android-34" "build-tools;34.0.0"`, and the NDK r27c zip.
2. `rustup target add aarch64-linux-android` and `cargo install cargo-apk --locked`.

Build (PowerShell):

```powershell
$env:ANDROID_HOME = "C:\tlp-build\android\sdk"
$env:ANDROID_NDK_ROOT = "C:\tlp-build\android\ndk-x\android-ndk-r27c"
$env:JAVA_HOME = "C:\tlp-build\android\jdk\jdk-17.0.13+11"; $env:Path = "$env:JAVA_HOME\bin;$env:Path"
cargo apk build -p tlp-desk-android --lib           # debug-signed APK under target/debug/apk
cargo apk build -p tlp-desk-android --lib --release # small (about 8 MB) but unsigned, see below
```

The debug APK is about 340 MB (debug symbols). `cargo-apk` needs a `[package.metadata.android.signing.release]`
keystore to sign a release build; to test without one, sign the release APK with the debug keystore
that `cargo apk build` created in `%USERPROFILE%\.android`:

```powershell
$bt = "$env:ANDROID_HOME\build-tools\34.0.0"
& $bt\zipalign.exe -f -p 4 target\release\apk\TLP-Desk-unaligned.apk aligned.apk
& $bt\apksigner.bat sign --ks $env:USERPROFILE\.android\debug.keystore --ks-pass pass:android --out TLP-Desk.apk aligned.apk
```

This identity is public knowledge; it is for testing only. Publish with your own keystore.

Manifest permissions come from `[package.metadata.android]` in the crate's `Cargo.toml`:
`INTERNET`, `ACCESS_NETWORK_STATE`, `ACCESS_WIFI_STATE`, `CHANGE_WIFI_MULTICAST_STATE`.

### Known gaps

* **Multicast lock.** mDNS discovery needs `WifiManager.createMulticastLock(..).acquire()` (a JNI
  call). Not implemented; the permission is requested and the call is documented in the crate
  docs. Connect by IP (or `id@ip`).
* No file transfer, chat, audio, monitor switching or recording UI.
* The touch UI has never run on a device or emulator.
