# tlp-desk-audio

**System audio, host to viewer.** Loopback capture of the host's default render device, converted
to 48 kHz stereo, coded with Opus in 10 ms frames, sent on a `StreamKind::Audio` side stream,
and played by the viewer through a small jitter buffer.

| | |
|---|---|
| Path | `crates/tlp-desk-audio` |
| Depends on | `tlp-desk-proto`, `tlp-desk-net`, `opus` (libopus, BSD-3-Clause, bundled and built with CMake), `cpal` 0.18 (WASAPI), `tokio`, `thiserror`, `tracing` |
| Used by | `tlp-desk-app` (`backend/audio.rs`) |
| Unsafe | none: `#![forbid(unsafe_code)]` (cpal and opus wrap the OS and libopus) |
| Permission | `AUDIO`, not granted by default, live-revocable |

## Codec choice

`opus` 0.4 (safe bindings over libopus 1.6) builds on MSVC with the CMake that ships with Visual
Studio's C++ tools, so no pure-Rust Opus was needed. Wire format: 48 kHz, stereo, 10 ms frames.

## Public API

- `AudioHost::new(Arc<dyn AudioCapture>)` and `AudioViewer::new(Arc<dyn AudioPlayback>, enabled_by_default)`
  are `Feature`s registered in `HostConfig::features` / `ViewerConfig::features`.
- `AudioCapture` / `AudioPlayback` are the device traits; `SystemCapture` / `SystemPlayback` use
  cpal, `device::fake` holds the tone/silence source and the recording speaker used by tests.
- `OpusEncoder` / `OpusDecoder`, `Resampler`, `JitterBuffer` + `JitterConfig`, `AudioEvent`.
- Control: the viewer sends `AudioControl { enabled, 48000, 2 }`; the host starts capture only
  then, answers with the format it uses, and stops at `enabled: false`, on `AUDIO` revocation or
  when the session ends.

## Stream format

One `Audio` uni stream per *generation* (the header `id`). A generation ends when the host goes
silent (a silence gate sends nothing) and a new one starts when sound resumes. The body is a
sequence of `u16` LE length + Opus packet; a length above `MAX_PACKET` ends the stream.

## Jitter buffer

Target 30 ms, ceiling 60 ms. Late and duplicate packets are dropped, reordered ones are put back
in order within the window, lost ones are concealed by Opus PLC, and if the queue stays above
target for a while one frame is skipped, so latency is bounded and never grows (drift handling).

## Tests

Unit tests in the modules; `tests/opus_roundtrip.rs` (tone SNR), `tests/e2e_loopback.rs`
(fake capture and speaker over a real loopback session, permission revoke); the app has
`remote_sound_follows_the_permission_and_the_toolbar_toggle`. `tests/real_devices.rs` uses the real
sound devices and is `#[ignore]`.

## Not done

Recording the sound into the MP4 recording is deferred (the recording lane stores H.264 only; an
Opus/AAC track would need a muxer change there).
