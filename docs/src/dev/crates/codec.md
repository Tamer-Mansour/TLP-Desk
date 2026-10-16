# tlp-desk-codec

**Video encode/decode behind traits, with software H.264 through OpenH264.** OpenH264 is compiled
from bundled C++ sources at build time — nothing is downloaded at run time. Colour is BT.709
limited range, signalled in the SPS VUI.

| | |
|---|---|
| Path | `crates/tlp-desk-codec` |
| Depends on | `capture` (for `BgraFrame`), `openh264` 0.9 + `openh264-sys2` 0.9 (feature `source`), `thiserror` |
| Used by | `host`, `viewer` |
| Unsafe | `#![deny(unsafe_code)]`; only `h264::ffi` (direct encoder calls) is allowed, with `// SAFETY:` comments |
| Threads | OpenH264's own encoder threads; colour conversion splits large frames into bands on `std::thread::scope` |

`openh264-sys2` is used directly because the safe wrapper cannot set a fixed slice count or
silence the library's logging.

## Public API

```rust
pub trait VideoEncoder: Send {
    /// Ok(None) = frame dropped or buffered.
    fn encode(&mut self, frame: &BgraFrame) -> Result<Option<EncodedFrame>, CodecError>;
    fn request_keyframe(&mut self);
    fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError>;
    fn config(&self) -> EncoderConfig;
}

pub trait VideoDecoder: Send {
    /// Ok(None) = needs more data.
    fn decode(&mut self, data: &[u8]) -> Result<Option<BgraFrame>, CodecError>;
    fn recycle(&mut self, frame: BgraFrame) { /* default: drop */ }
}
```

- `new_encoder(EncoderConfig)` and `new_decoder()` return boxed trait objects.
- `EncoderConfig { width, height, fps, bitrate_kbps }`. `EncoderConfig::for_screen(w, h, fps)`
  picks about 0.1 bit per pixel: `pixels_per_second / 10_000` kbps, clamped to 1–40 Mbps
  (≈ 12.4 Mbps for 1080p60, 13.8 Mbps for 1920×1200@60, 6.2 Mbps for 1080p30).
- `EncodedFrame { keyframe, timestamp_us, data }` — `data` is an Annex-B bitstream.
- `CodecError`: `SizeMismatch { got_w, got_h, want_w, want_h }`, `BadBitstream`,
  `Backend(String)`.
- Re-exported: `H264Encoder`, `H264Decoder`, `H264Tuning`, `H264Complexity`, `Downscale`;
  `pub mod convert`.

## H.264 encoder

`H264Encoder::new(config)` / `with_tuning(config, H264Tuning)`.

| `H264Tuning` field | Default | Notes |
|---|---|---|
| `threads` | `0` | 0 = half the logical CPUs, clamped to 1–4; explicit values capped at 16 |
| `complexity` | `Low` | `Low`, `Medium`, `High` |
| `downscale` | `Auto` | `Auto` halves sources above OpenH264's 3840×2160 limit; `Never` rejects them; `Half` always halves (2×2 box) |
| `screen_content` | `false` | OpenH264 screen-content mode: smaller output for text but 2–3× the CPU; forces 1 thread |

Encoder behaviour:

- Constrained baseline profile, CAVLC, one reference frame, no B-frames — lowest latency.
- **IDR only on request.** `uiIntraPeriod = 0`; the first frame is always an IDR and
  `request_keyframe()` forces the next one.
- Bitrate rate control with peak = 1.5 × target. **Frame skipping is disabled**
  (`bEnableFrameSkip = false`): the session layer, not the encoder, decides what to drop.
- `set_bitrate` changes bitrate mid-stream **without** a keyframe (it orders the max/target
  updates so OpenH264 always sees max ≥ target).
- With more than one thread the frame is cut into that many fixed slices so threads can work in
  parallel.
- Denoise, background detection, adaptive quantisation and long-term references are off; scene
  change detection only in screen-content mode.
- Encoding a frame of a different size than configured returns `SizeMismatch`; the host then
  recreates the encoder.

## Decoder

`H264Decoder::new()` outputs tight-stride BGRA with alpha 255 and reuses buffers passed to
`recycle`. Any decode error becomes `BadBitstream`; the viewer then asks for a keyframe.

## Colour conversion (`convert`)

BT.709 limited range in integer arithmetic written so the compiler auto-vectorises it.

- `bgra_to_i420(src, &mut I420Buffer)` — odd last row/column padded by edge duplication.
- `bgra_to_i420_half(src, out)` — 2×2 box downscale and conversion in one pass.
- `i420_to_bgra(I420Planes, out)`.
- `I420Buffer::resize` rounds up to even and reuses allocations; `even_up`, `half_size`.
- Frames are split into horizontal bands of at least 128 rows, on up to `min(cpus, 8)` threads.

## Benchmark

`examples/bench.rs` measures capture → encode → decode on the real desktop:

```powershell
cargo run --release -p tlp-desk-codec --example bench -- --seconds 10 --fps 60
```

| Flag | Default | Meaning |
|---|---|---|
| `--seconds N` | 10 | run time |
| `--fps N` | 60 | target rate |
| `--monitor N` | 0 | monitor index |
| `--backend auto\|wgc\|dxgi` | auto | capture backend |
| `--static` | off | disable the moving test box; encode only real desktop changes |
| `--threads N` | 0 | encoder threads |
| `--complexity low\|medium\|high` | low | |
| `--kbps N` | from `for_screen` | bitrate override |
| `--crop WxH` | — | keep the top-left region |
| `--scale WxH` | — | GPU downscale in the capturer (`CaptureOptions::max_size`) |
| `--downscale auto\|never\|half` | auto | encoder-side downscale |
| `--screen` | off | screen-content mode |
| `--pipelined` | off | capture, encode and decode on separate threads with a newest-wins mailbox |
| `--animate` | off | open a small animated window so there is always motion |
| `--conv` | off | time the BGRA → I420 conversion separately |
| `--quality` | off | PSNR / edge PSNR / SSIM / QP on text-heavy off-screen scenes (also `--bpp a,b,c`, `--frames N`, `--scene ui\|code\|scroll\|photo`, `--qp MIN:MAX`, `--synthetic WxH`); see [Performance notes](../performance.md#measured-bench---quality) |

It prints average / p95 / max per stage, Mbps, KB per frame, keyframe count and process CPU
time. A recorded run without NASM: 51.7 fps serial at 1080p, encode 5.95 ms, decode 2.88 ms.

## Tests

All run in CI (none ignored):

- `tests.rs`: `encode_decode_1080p_matches_dimensions_and_psnr` (PSNR > 32),
  `keyframe_request_yields_idr`, `decoder_can_join_at_requested_keyframe`,
  `odd_dimensions_are_handled`, `size_mismatch_is_rejected`,
  `bitrate_change_takes_effect_without_keyframe`, `garbage_bitstream_is_an_error_or_no_picture`,
  `screen_config_defaults`, `prescaled_frames_are_encoded_at_their_own_size`,
  `oversized_source_is_downscaled_automatically`, `forced_half_downscale_keeps_quality`.
- `convert.rs`: `known_colors_convert_to_bt709_limited`, `flat_color_round_trip_within_tolerance`,
  `gradient_round_trip_within_tolerance`, `odd_dimensions_are_padded_by_edge_duplication`,
  `half_conversion_equals_conversion_of_downsampled_image`,
  `half_size_handles_odd_and_large_sources`, `odd_width_bgra_output`.

```powershell
cargo test -p tlp-desk-codec --release   # release is much faster for the 1080p tests
```

See [ADR 0006](../../adr/0006-openh264-first.md) for why software OpenH264 comes first.
