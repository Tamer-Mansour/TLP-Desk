# 0012: Cap capture at 1920×1200, scaled on the GPU

- **Status:** Accepted (a user-configurable cap is planned)
- **Date:** 2026-10-13
- **Area:** `tlp-desk-host`, `tlp-desk-capture`

## Context

High-resolution laptop and desktop monitors (2560×1440, 3840×2160, 3840×2400) are common. Streaming
them at native size multiplies every cost: a 3840×2400 BGRA frame is 36 MB to copy from GPU to
CPU, four times the pixels to convert and encode, and above OpenH264's 3840×2160 limit. The
viewer's window is usually smaller than the host's screen anyway.

## Decision

The host opens capture with `CaptureOptions::max_size(1920, 1200)` (`MAX_CAPTURE_SIZE`).

- Larger monitors are scaled **on the GPU, before readback**: a copy into a shader-readable
  texture, mip generation when the reduction exceeds 2×, then one bilinear draw to the target
  size.
- `fit_within` keeps the aspect ratio, never upscales and rounds down to even dimensions:
  3840×2400 → 1920×1200, 2560×1440 → 1920×1080, 5120×1440 → 1920×540.
- The encoder is sized from `frame_size()`, not the monitor; if the GPU scaler cannot be created,
  capture falls back to native size and the encoder follows.
- Input is mapped over the **real** monitor rectangle, so clicks still land on exact physical
  pixels.

## Alternatives considered

- **Native resolution always** — fine for 1080p, too costly for 4K at 60 fps with software
  encoding.
- **Downscale on the CPU** — the 36 MB readback has already happened; the expensive part is not
  avoided.
- **Encoder-side halving only** (`Downscale::Half`) — still reads back full size; kept as a
  fallback for sources above OpenH264's limit.

## Consequences

- Measured: 57.9 fps for a 3840×2400 monitor streamed at 1920×1200 on loopback.
- On a 4K monitor fine text is softer than native; a viewer on a large screen sees an upscaled
  image. A quality preset / configurable cap (`SetQuality` in protocol v4, ROADMAP M2 Lane B) will
  let users trade bandwidth and CPU for sharpness.
- Tests compare GPU scaling with a CPU area average (`wgc_scaled_half_matches_cpu_reference` and
  friends, ignored in CI) and check click mapping on scaled video
  (`scaled_video_maps_clicks_to_physical_monitor_pixels`).

## References

[tlp-desk-capture](../dev/crates/capture.md#gpu-downscale);
[Performance notes](../dev/performance.md#gpu-downscale-before-readback).
