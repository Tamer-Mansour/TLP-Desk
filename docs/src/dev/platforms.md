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

See [the Android section below](#android-viewer-details) once the crate lands; summary: viewer
only, a `tlp-desk-android` cdylib that drives the shared viewer session with a touch-first UI.
