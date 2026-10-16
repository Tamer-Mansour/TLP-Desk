# tlp-desk-audio

**System audio streaming — placeholder.** The crate exists so the workspace layout is final, but
it contains only a module doc comment and `#![forbid(unsafe_code)]`. It has no dependencies, no
API and no tests yet.

| | |
|---|---|
| Path | `crates/tlp-desk-audio` |
| Roadmap | M2 Lane C — "System audio streaming (WASAPI loopback → Opus), mute toggle" |
| Planned stack | `cpal` (WASAPI loopback on Windows) + `opus` (CLAUDE.md tech choices) |
| Permission bit | `AUDIO` (bit 5), already defined in `tlp-desk-proto` |

## What is already prepared elsewhere

- **Stream kind.** `tlp-desk-proto` defines `StreamKind::Audio = 3`: a host → viewer
  unidirectional stream whose header `id` is the audio stream *generation*, and whose body is a
  sequence of `u16` little-endian length + Opus packet. The transport does not open such streams
  yet.
- **Permission.** `Permissions::AUDIO` exists; the host does not grant it by default
  (`DEFAULT_PERMISSIONS` is view + control + keyboard).

## Suggested shape (for whoever picks up the lane)

Follow the pattern of `capture` and `codec`: a platform-neutral trait (for example an
`AudioSource` that yields PCM frames and an encoder/decoder pair), a Windows implementation in a
single `#[allow(unsafe_code)]` module if raw APIs are needed, and loopback tests that do not need
real audio hardware. Latency matters more than fidelity: prefer small Opus frames and drop late
packets rather than buffering, in the same spirit as video.

When you implement it, update this page, the [user guide page](../../user-guide/en/coming-soon/audio.md)
and the [changelog](../../changelog.md).
