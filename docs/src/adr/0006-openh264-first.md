# 0006: Software H.264 with OpenH264 first

- **Status:** Accepted
- **Date:** 2026-10-13
- **Area:** `tlp-desk-codec`

## Context

Screen video needs a codec that every Windows PC can encode and decode in real time at 1080p60,
with low latency and without per-machine setup. Hardware encoders (NVENC, Quick Sync, AMF, Media
Foundation) are faster but differ per vendor and driver, and some PCs have none.

## Decision

Start with **software H.264 through OpenH264** (`openh264` / `openh264-sys2` 0.9, feature
`source`), compiled from bundled sources at build time, behind the `VideoEncoder` /
`VideoDecoder` traits. Tune it for latency: constrained baseline, no B-frames, one reference frame,
IDR only on request, bitrate rate control with frame skipping disabled, fixed slices per thread.
Hardware encoders come later behind the same trait (roadmap M2 Lane B).

## Alternatives considered

- **Hardware encoding first** — best performance where it works, but four code paths, driver
  quirks and a mandatory software fallback anyway.
- **VP8/VP9/AV1 in software** — VP8 is similar in cost with worse tooling for this use; VP9/AV1
  real-time software encoders are far too slow for 1080p60 on mid-range CPUs.
- **Downloading Cisco's prebuilt OpenH264 binary at run time** — violates the no-internet rule.

## Consequences

- Works on every x64 Windows PC; no runtime download (CLAUDE.md rule 2).
- Measured 1080p: encode ≈ 6 ms, decode ≈ 3 ms per frame without NASM — enough for 60 fps with
  threading.
- Builds need a C++ compiler; NASM is optional but gives the assembly fast paths.
- CPU use during a session is higher than with a hardware encoder; the "< 15 % CPU" target may need
  hardware encoding on weaker machines.
- OpenH264 is limited to 3840×2160; larger sources are halved (`Downscale::Auto`), and the host's
  1920×1200 capture cap makes this rare.
- The licence of the bundled sources (BSD-2-Clause) is compatible with the project's
  permissive-only policy.

## References

[tlp-desk-codec](../dev/crates/codec.md); [Performance notes](../dev/performance.md).
