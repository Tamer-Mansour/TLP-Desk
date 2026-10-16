# Performance notes

Targets from CLAUDE.md, on a 1 Gbps LAN or good Wi-Fi:

- 1080p at 60 fps, glass-to-glass latency under 60 ms;
- drop frames instead of queueing when the viewer falls behind — latency must never grow;
- idle host CPU under 1 %, active session under 15 % on a mid-range CPU with software encoding.

This page records how the current code tries to meet them and what has been measured.

## Measurements so far

| What | Result | Source |
|---|---|---|
| Capture → encode → decode, 1080p, serial, no NASM | 51.7 fps; encode 5.95 ms avg, decode 2.88 ms avg | `bench --crop 1920x1080` (ROADMAP M1 Lane B) |
| DXGI backend in the bench | 55 fps | ROADMAP M1 Lane B |
| Real loopback session, monitor 0 (3840×2400 → 1920×1200) | 57.9 fps, latency estimate 6–15 ms | ROADMAP M2 Lane B |
| Real loopback session, monitor 1 (1080p) | 55.9 fps | ROADMAP M2 Lane B |
| GUI viewer ↔ CLI host on one PC, early M1 | 30 fps at 1920×1200, ~27 ms estimate | ROADMAP M1 Lane D |

Still open (ROADMAP M2 Lane B): adaptive bitrate, a measured glass-to-glass figure on wired LAN, a
configurable capture cap, static-screen detection with near-zero bandwidth, hardware encoders.

## The 60 fps pipeline

Capture and encode run on **separate threads** joined by a one-slot mailbox, so capturing frame
*n + 1* overlaps encoding frame *n*. Decoding on the viewer runs on its own thread too. The
benchmark's `--pipelined` mode reproduces this arrangement; serial mode shows the per-stage cost.

The encode loop is paced to `1/fps` but only encodes when something changed. A still desktop
costs almost nothing after the first frame.

## GPU downscale before readback

Reading a 3840×2400 BGRA frame back from the GPU moves 36 MB per frame — over 2 GB/s at 60 fps —
before the encoder even starts. The host caps capture at **1920×1200** and scales on the GPU
(mip generation for reductions above 2×, then one bilinear pass), so only 9 MB per frame crosses
to the CPU and the encoder works on a quarter of the pixels. Input mapping is unaffected: clicks
are mapped over the real monitor, not the video. See
[ADR 0012](../adr/0012-capture-size-cap.md).

## Hybrid GPU laptops

On laptops with an integrated and a discrete GPU, the monitor may be driven by one adapter while
D3D defaults to the other. Copying every frame across adapters measured **3.7 ms vs 1.3 ms** per
1080p frame. The capturer therefore creates its D3D11 device on the adapter that owns the target
monitor's output.

## Drop, don't queue

Every hand-off holds at most a few frames and discards the oldest when full (see
[Video pipeline](architecture/video.md#where-latency-can-hide--and-why-it-doesnt)). Combined with
one QUIC stream per frame, a slow link or a slow viewer lowers the frame rate instead of building
a backlog. The host e2e test `slow_encoder_drops_frames_instead_of_queueing` and the net test
`host_drops_video_instead_of_queueing` keep this property from regressing.

## Quality policy: smaller, never blurrier

The stream must never turn into a smeared or blocky picture. When bits are short the picture gets
**smaller** (lower resolution, then lower frame rate); the quality of what is sent stays high.
Three pieces enforce this.

**1. A quantiser floor in the encoders.** `QpBounds::FLOOR` limits H.264 QP to 12..=32
(`QUALITY_MIN_QP`, `QUALITY_MAX_QP`). OpenH264 gets `iMaxQp`/`iMinQp`; Media Foundation encoders get
`CODECAPI_AVEncVideoMaxQP`/`MinQP` (a vendor that refuses them keeps its own limits; the refusal is
logged at debug level). Frame skipping stays off, so a frame that cannot be coded at QP 32 within
the bitrate overshoots it and the controller reacts (below). The minimum stops the rate control
from chasing invisible detail: NVIDIA's MFT otherwise drops to QP 2 on a static screen and spends
3.7 Mbps where 0.16 Mbps gives the same picture.

**2. A resolution ladder in the adaptive controller** (`crates/tlp-desk-host/src/adapt/controller.rs`).
What a bitrate buys is measured in bits per pixel (bpp = kbps / (pixels x fps)). Text stays crisp
above about **0.07 bpp**. When congestion has pushed the target under that for 2.5 s, the controller
moves to the first rung of the ladder that restores it (one jump, not a staircase):

| Rung | Resolution (of a 1920x1200 cap) | Frame rate (of 60) |
|---|---|---|
| 0 | 1920x1200 | 60 |
| 1 | 1600x1000 | 60 |
| 2 | 1600x1000 | 45 |
| 3 | 1280x800 | 45 |
| 4 | 1280x800 | 30 |
| 5 | 1024x640 | 30 |
| 6 | 1024x640 | 15 |

A rung change goes through the capture cap (`capture_cap`, sizes rounded to multiples of 4), an
encoder rebuild and a keyframe. Monitors smaller than the cap are scaled themselves. Rules that
prevent flapping: changes are at least 4 s apart; a step *up* needs 10 s without congestion **and**
at least 0.10 bpp at the bigger size (the gap to the 0.07 floor is the hysteresis); a step down
within 30 s of a step up doubles the wait for the next step up (to at most 60 s). Thresholds are
scaled down for capped ceilings (the Speed preset) so they can always climb back.

**3. Defaults that leave headroom on a LAN.** `EncoderConfig::for_screen` budgets **0.12 bpp**
(14.9 Mbps for 1080p60), up from 0.1. Presets: *Speed* caps at 1280x720 with a 60 % ceiling,
*Balanced* is the floor plus the ladder from 1920x1200, *Quality* streams the monitor at native size
(up to 3840x2160, the encoders' limit; larger sources are halved) with a 160 % ceiling, also under
the ladder.

The viewer's stats overlay shows the size and the average QP read from the bitstream (`QpProbe`):
`1280x800 . Q~24`. Intel Quick Sync always writes QP 26 in its slice headers and varies it per
macroblock, so the number is hidden for that encoder.

### Measured: `bench --quality`

`cargo run --release -p tlp-desk-codec --example bench -- --quality --encoder sw|hw --adapter
intel|nvidia --bpp 0.01,0.04 --qp 12:32` renders four animated scenes off screen with GDI
(ClearType): `ui` (settings page with English and Arabic text, a counter and a progress bar),
`code` (syntax-coloured editor scrolling 3 px/frame), `scroll` (prose scrolling 6 px/frame) and
`photo` (smooth colour fields with grain panning), at 1920x1080, 60 fps, 70 frames per point. It
reports luma PSNR, **edge PSNR** (pixels on or next to strokes: where text lives) and SSIM of the
decoded pictures. `--qp 0:51` is the old unbounded behaviour.

Text scenes with OpenH264, edge PSNR in dB (higher is better; below ~38 dB text visibly softens):

| bpp | `ui` | `code` | `scroll` | note |
|---|---|---|---|---|
| 0.005 unbounded (QP 42) | - | 21.2 | 22.1 | illegible |
| 0.005 floor (QP 32) | - | 28.4 | 28.6 | still soft: the ladder steps in long before |
| 0.02 | 40.1 | 33.1 | 31.2 | |
| 0.04 | 40.7 | 37.4 | 38.5 | |
| 0.06 | 40.7 | 39.3 | 40.4 | |
| **0.07 (the floor)** | 40.7 | **~39.8** | **~40.5** | text readable: >= 38 dB |
| 0.12 | 40.6 | 42.3 | 40.6 | the `for_screen` default |

Photographic content needs more (luma PSNR 37.3 dB at 0.06 bpp, 44.4 at 0.08), which is why the
floor is tuned for desktops and not for video.

What the quantiser floor changes at starvation (1080p60, 0.01 bpp, edge PSNR / luma PSNR in dB):

| Encoder | Scene | Unbounded | Floor (QP 12..32) |
|---|---|---|---|
| OpenH264 | code | 21.3 / 32.0 (QP 42) | 28.4 / 39.0 (QP 32) |
| OpenH264 | scroll | 21.7 / 30.3 (QP 42) | 28.6 / 37.2 (QP 32) |
| NVIDIA RTX A3000 | scroll | 20.2 / 28.9 (QP 42) | 30.1 / 38.8 (QP 30) |
| NVIDIA RTX A3000 | code | 23.5 / 34.0 (QP 39) | 31.0 / 41.7 (QP 28) |
| Intel UHD Quick Sync | scroll | 22.8 / 31.1 | 31.4 / 39.8 |
| Intel UHD Quick Sync | code | 25.6 / 35.1 | 30.0 / 39.8 |

At the same time the floor barely costs anything where bits are plentiful (OpenH264 `code` at
0.06 bpp: 39.3 vs 39.7 dB, within noise; NVIDIA `ui` at 0.04 bpp: 40.1 vs 40.2 dB using 156 kbps instead of
3 672 kbps).

The controller itself is tested against simulated links (`adapt::simulation`): a collapse from
100 to 2.5 Mbps lowers the picture to 1280x800 or smaller within 15 s with the modelled QP staying
at or below 24 and at most four structural changes; recovery climbs one rung at a time, at least
10 s apart, back to full quality; a borderline 6 Mbps Wi-Fi link settles on a rung instead of
flapping.

## Encoder settings that matter

- **No internal frame skipping** in OpenH264 — the session decides what to drop, with knowledge
  of the network.
- **IDR only on demand** — no periodic keyframe spikes.
- **Threads = half the logical CPUs, 1–4**, with the frame split into the same number of slices.
- **Bitrate ≈ 0.12 bit/pixel** (`EncoderConfig::for_screen`): about 15 Mbps for 1080p60, well
  within a LAN, and changeable at run time without a keyframe. See
  [Quality policy](#quality-policy-smaller-never-blurrier).
- **QP 12..32** on every encoder, so the picture never goes below a quality floor.
- **NASM** gives OpenH264 its assembly paths. Builds without it work but encode slower; release
  builds should have it.

## Colour conversion

BGRA → I420 is integer BT.709 code written for auto-vectorisation and split into bands of at least
128 rows across up to 8 threads. `bgra_to_i420_half` downsizes 2× and converts in one pass when
the encoder has to halve an oversized source.

## Hardware encoding

`new_encoder` tries a Media Foundation hardware H.264 encoder first (Intel Quick Sync, NVIDIA,
AMD; module `crates/tlp-desk-codec/src/mf/`) and falls back to OpenH264 if creation fails, or if
an encode or a bitrate change fails mid-stream (the replacement starts with a keyframe). The
stream is constrained baseline, no B-frames, infinite GOP with forced IDR, so the OpenH264
decoder in the viewer reads it unchanged. Input is NV12 packed on the CPU.

Override with `TLP_DESK_ENCODER=software|hardware|auto` (default `auto`) and
`TLP_DESK_HW_ADAPTER=auto|intel|nvidia|amd` (see `EncoderOptions`). On NVIDIA Optimus laptops
the NVIDIA MFT only opens in a process that runs on the dGPU, so it needs the driver's
high-performance setting for the exe; the bench example opts in by itself.

Synthetic 1080p60 desktop-like frames (`bench --synthetic 1920x1080 --encoder ...`, no screen
capture), 8 s each, 12 Mbps target, measured on a laptop with Intel UHD and an NVIDIA RTX A3000:

| Encoder | Encode ms (avg / p95) | CPU of the encode calls | Bitrate | PSNR avg / min |
|---|---|---|---|---|
| OpenH264 (software) | 3.6 / 4.1 | 68% of a core (2.8% of the machine) | 6.7 Mbps | 46.3 / 39.7 dB |
| Intel Quick Sync | 8.8 / 11.0 | 31% of a core (1.3%) | 4.9 Mbps | 46.4 / 42.7 dB |
| NVIDIA RTX A3000 | 5.6 / 6.9 | 32% of a core (1.3%) | 10.8 Mbps | 47.0 / 39.9 dB |

OpenH264 is faster per frame on this machine because the test content is easy and it runs on many
cores; the hardware encoders use about half the CPU (most of what remains is the BGRA to NV12
conversion and the copy), which matters on weaker hosts. Per-frame latency of the Intel path is
higher, so `auto` is a policy choice, not a speed claim: set `TLP_DESK_ENCODER=software` if
hardware latency is worse for your content. A GPU video processor for the colour conversion is
not implemented yet.

## Cursor

The pointer is not part of the captured image; the host composites it into the frame and saves
only the small patch underneath, so a cursor move costs one encode but no full-frame copy.

## How to measure

```powershell
# Pipeline cost per stage on this machine
cargo run --release -p tlp-desk-codec --example bench -- --seconds 10 --pipelined --scale 1920x1200

# Real session on one PC: CLI host on another port, GUI viewer, then read the stats overlay
cargo run --release -p tlp-desk-cli -- serve --code --port 47410
cargo run --release -p tlp-desk-app -- --connect 127.0.0.1:47410 --code <code>
```

Remember that the overlay's latency is an estimate that excludes display time; a camera filming
both screens is the honest glass-to-glass measurement. The built-in latency probe
(`LatencyProbe` / `LatencyEcho`) is reserved in protocol v4 for a better in-app measurement.
