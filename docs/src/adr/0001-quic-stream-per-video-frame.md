# 0001: One QUIC stream per video frame

- **Status:** Accepted (protocol v2)
- **Date:** 2026-10-13
- **Area:** `tlp-desk-net`, `tlp-desk-host`, `tlp-desk-viewer`

## Context

Video must reach the viewer with the lowest possible latency, and CLAUDE.md requires that when the
viewer or the link falls behind we **drop frames instead of queueing** — latency must never grow.
The first protocol version sent all frames on one long-lived unidirectional QUIC stream.

A single stream is a FIFO. Once a frame is written, every later frame waits behind it
(head-of-line blocking), and data already handed to the stream cannot be withdrawn. When the link
slowed down, latency grew without bound.

## Decision

The host opens **one unidirectional QUIC stream per encoded frame**, writes exactly one framed
`VideoFrame` (since v4 preceded by a `StreamHeader` of kind `Video`, id = `seq`) and finishes it.

- **Host:** a frame is *in flight* until the viewer acknowledged all of its bytes. With 3 or more
  frames in flight, non-key frames are dropped and the encoder is asked for a keyframe; until a
  keyframe is offered, all non-key frames are dropped. Keyframes are dropped only at 6 or more in
  flight.
- **Viewer:** streams are read concurrently and delivered in `seq` order through a 4-frame queue
  (overflow dropped); stale frames are discarded; after any gap, non-key frames are skipped until
  a keyframe arrives, and `RequestKeyframe` is sent (at most every 250 ms).

## Alternatives considered

- **One long-lived stream** (v1) — head-of-line blocking; impossible to drop queued frames.
- **QUIC datagrams** — no head-of-line blocking, but limited to about 1.2 KB each, so every frame
  would need our own fragmentation, reassembly and loss handling. An H.264 decoder needs complete
  frames anyway.
- **Resetting a long-lived stream when behind** — loses the frame being sent and still delays the
  next one behind the reset.

## Consequences

- Each frame is still delivered reliably and completely, which the decoder needs.
- Frames are independent: a late frame never blocks a newer one.
- Dropping is simple: just do not open a stream.
- Many short streams need generous stream limits (`max_concurrent_uni_streams = 32`).
- `seq` must increase by exactly one per encoded frame, and the first frame of a session must be a
  keyframe, so the viewer can detect gaps.

Guarded by `host_drops_video_instead_of_queueing`,
`viewer_skips_to_keyframe_and_requests_one_after_loss` (net) and
`slow_encoder_drops_frames_instead_of_queueing` (host e2e).

## References

`crates/tlp-desk-net/src/session.rs`; [Video pipeline](../dev/architecture/video.md);
[wire protocol](../protocol.md#why-one-stream-per-video-frame).
