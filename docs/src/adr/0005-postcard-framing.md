# 0005: `postcard` with a `u32` length prefix for messages

- **Status:** Accepted
- **Date:** 2026-10-13
- **Area:** `tlp-desk-proto`

## Context

Every control message and video frame needs an encoding. Requirements: compact (video frames are
large, input events are frequent and tiny), fast, defined by Rust types so both sides cannot
disagree about field meaning, robust against hostile input from a peer, and usable on a byte
stream where messages arrive in arbitrary chunks.

## Decision

- Messages are one Rust enum, `Message`, serialised with **`serde` + `postcard`**.
- On streams, each message is prefixed by its body length as a **`u32` little-endian**.
- Bodies larger than `MAX_FRAME_LEN` (16 MiB) are a protocol error and close the connection; the
  length is checked *before* allocating.
- `FrameDecoder` decodes incrementally from any chunking and returns errors, never panics, on
  garbage.
- **Append-only evolution:** postcard encodes an enum variant as its index, so variants and fields
  are never reordered or removed; new ones are appended and `PROTOCOL_VERSION` is bumped. The
  ALPN is version-less so a peer with another version still reaches the `Hello` check and gets a
  clean `VersionMismatch` (v1 variant indices are pinned so even a v1 peer can decode that reply).

## Alternatives considered

- **Protocol Buffers / Cap'n Proto** — self-describing or schema-evolving, but add a schema
  language and code generation; we control both ends and refuse mismatched versions anyway.
- **JSON** — large and slow for frames; binary data would need base64.
- **bincode** — similar trade-offs to postcard; postcard's varint encoding is more compact for the
  many small integers in input events.

## Consequences

- Very small messages: a key event is a handful of bytes.
- Any layout change is incompatible; peers must run the same protocol version. This is accepted
  for a LAN tool where both sides are usually updated together, and the error message says so.
- The proto crate pins variant indices in tests (`v1_variant_indices_are_stable`,
  `v3_variants_are_appended`, `v4_variants_are_appended_in_order`) and round-trips every message
  (`every_message_roundtrips`, `frames_decode_from_byte_by_byte_stream`,
  `oversized_length_prefix_is_rejected`, `garbage_body_is_an_error_not_a_panic`).

## References

[tlp-desk-proto](../dev/crates/proto.md); [wire protocol](../protocol.md#framing).
