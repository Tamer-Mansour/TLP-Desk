# tlp-desk-capture

**Screen capture behind a trait.** On Windows it uses Windows.Graphics.Capture (WGC) first and
DXGI Desktop Duplication as a fallback, and can downscale large screens on the GPU before the
pixels are copied to the CPU. On other platforms every call returns `CaptureError::Unsupported`.

| | |
|---|---|
| Path | `crates/tlp-desk-capture` |
| Depends on | `thiserror`; on Windows `windows` 0.62 + `windows-core` |
| Used by | `codec` (for `BgraFrame`), `host`, `viewer` |
| Unsafe | `#![deny(unsafe_code)]`; only `mod windows_capture` is `#[allow(unsafe_code)]`, every block has a `// SAFETY:` comment |
| Threads | none of its own; WGC's free-threaded frame pool signals a `Condvar` from an OS thread |

## Public API

```rust
pub trait ScreenCapturer: Send {
    /// Ok(None) = the screen did not change within `timeout`.
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<BgraFrame>, CaptureError>;
    fn cursor(&mut self) -> Result<CursorState, CaptureError>;
    /// Always the real monitor geometry (input mapping relies on it).
    fn monitor(&self) -> &MonitorInfo;
    fn recycle(&mut self, frame: BgraFrame) { /* default: drop */ }
    fn backend(&self) -> Option<CaptureBackend> { None }
    /// Smaller than the monitor when GPU scaling is active.
    fn frame_size(&self) -> (u32, u32) { /* default: monitor size */ }
}
```

- `BgraFrame { width, height, stride, data, timestamp_us }` — `stride` may exceed `width * 4`
  (GPU row padding). Helpers: `new_tight`, `row`, `row_mut`, `is_well_formed`.
- `CursorState { x, y, visible, shape: Option<CursorShape> }` — hotspot position relative to the
  monitor in physical pixels; `shape` is only set when it changed. `CursorShape` is tightly
  packed RGBA8 with a hotspot.
- `MonitorInfo { index, name, width, height, primary, x, y }` — physical pixels on the virtual
  desktop; index 0 is always the primary monitor.
- `list_monitors()` — primary first, then left-to-right, top-to-bottom.
- `open_capturer(index)` (automatic backend), `open_capturer_with(index, backend)` (one backend,
  no fallback, no scaling), `open_capturer_opts(index, CaptureOptions)`.
- `CaptureOptions { backend: Option<CaptureBackend>, max_size: Option<(u32, u32)> }`;
  `CaptureOptions::max_size(w, h)`.
- `fit_within(src, max)` and `mip_level_for(src, out)` (pure helpers in `scale.rs`).
- `CaptureError`: `Unsupported`, `NoSuchMonitor(u32)`, `SurfaceLost` (desktop switch — lock
  screen, UAC — or a display-mode change: recreate the capturer), `Platform(String)`.

## Backends

| | Windows.Graphics.Capture | DXGI Desktop Duplication |
|---|---|---|
| Minimum OS | Windows 10 1903 | Windows 8 |
| Used | first | if WGC fails to start |
| Cursor | excluded from the image (`IsCursorCaptureEnabled = false`, best effort) | not drawn |
| Yellow border | disabled where supported (`IsBorderRequired = false`) | none |
| Lost surface | item `Closed` or size change → `SurfaceLost` | `DXGI_ERROR_ACCESS_LOST` → `SurfaceLost` |

The automatic path tries WGC, then DXGI, and reports both errors if both fail.

**Hybrid-GPU laptops.** The D3D11 device is created on the adapter that actually scans out the
target monitor (found by matching `DXGI_OUTPUT_DESC::Monitor`), falling back to the default
adapter. Copying frames across adapters was measured at 3.7 ms per 1080p frame versus 1.3 ms on
the right adapter.

**WGC details.** A free-threaded frame pool with 2 buffers; `take_latest()` drains and closes
older frames so a stale frame is never returned; each frame is closed right after readback.
Drop order matters: remove the `FrameArrived` handler, close the session, then the pool.

**DXGI details.** `DuplicateOutput` runs under a per-monitor-v2 DPI scope (high-DPI duplication
needs a DPI-aware caller). A frame with `LastPresentTime == 0` means only the pointer moved; it is
skipped.

## GPU downscale

When `max_size` is set and the monitor is larger, `StagingReader` runs a `GpuScaler` before the
CPU copy (the host caps capture at 1920×1200, so a 3840×2400 monitor copies 9 MB per frame
instead of 36 MB):

1. Copy the captured texture into a shader-readable texture.
2. If the reduction is more than 2×, `GenerateMips` (box filter) and sample from mip level
   `mip_level_for(src, out)`.
3. Draw one full-screen triangle into the smaller render target with a bilinear sampler. An
   exact 2× reduction is therefore a 2×2 box filter.

HLSL is compiled at run time with `D3DCompile` (`vs_4_0` / `ps_4_0`). If the scaler cannot be
created, capture silently falls back to native size — **callers must size the encoder from
`frame_size()`, not from the monitor**.

`fit_within` keeps the aspect ratio, never upscales, and rounds a scaled result down to even
dimensions (H.264 needs even sizes): 3840×2400 → 1920×1200, 2560×1440 → 1920×1080,
5120×1440 → 1920×540.

See [ADR 0012](../../adr/0012-capture-size-cap.md).

## Cursor

`CursorTracker` polls `GetCursorInfo` and re-reads the shape only when the `HCURSOR` changes.
Colour cursors are converted BGRA → RGBA (using the AND mask when there is no alpha);
monochrome cursors are decoded from the double-height mask, with "invert screen" pixels drawn
black. The host composites the cursor into the frame (see [tlp-desk-host](host.md)).

## Tests

Run in CI: `frame_rows_respect_stride`, `malformed_frame_detected`,
`fit_keeps_sources_that_already_fit`, `fit_scales_exact_halves`,
`fit_preserves_aspect_and_limits_both_sides`, `fit_rounds_down_to_even_and_never_exceeds_max`,
`mip_level_choice` (and `unsupported_off_windows` on other OSes).

**Ignored — need a real interactive desktop session:** `lists_primary_monitor_first`,
`wgc_captures_primary_monitor`, `dxgi_captures_primary_monitor`,
`factory_captures_primary_monitor`, `wgc_scaled_half_matches_cpu_reference`,
`dxgi_scaled_half_matches_cpu_reference`, `wgc_scaled_third_uses_mips_and_matches_cpu_reference`,
`wgc_scaled_second_monitor_matches_cpu_reference` (skips without a second monitor),
`max_size_larger_than_monitor_is_native`. The scaled tests compare against a CPU area average
(mean absolute difference below 2.0).

```powershell
cargo test -p tlp-desk-capture
cargo test -p tlp-desk-capture -- --ignored   # on an unlocked Windows desktop
```
