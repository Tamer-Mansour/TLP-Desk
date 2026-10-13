# tlp-desk-proto

**Wire messages, framing and stream headers.** Every byte that crosses the network between two
TLP-Desk peers is defined here. The crate is pure, synchronous Rust with no I/O.

| | |
|---|---|
| Path | `crates/tlp-desk-proto` |
| Depends on | `serde`, `postcard`, `thiserror` |
| Used by | `net`, `host`, `viewer`, `input`, `app`, `cli` |
| Unsafe | `#![forbid(unsafe_code)]` |
| Threads | none |

## Public API

### Constants

| Item | Value | Meaning |
|---|---|---|
| `PROTOCOL_VERSION` | `4` | Sent in `Hello`; peers with a different value are rejected with `VersionMismatch`. |
| `MAX_FRAME_LEN` | 16 MiB | Largest message body; longer length prefixes are a protocol error. |
| `STREAM_HEADER_LEN` | 9 | Size of `StreamHeader` (v4). |
| `FILE_DATA_HEADER_LEN` | 12 | Size of `FileDataHeader` (v4). |

### Framing (`framing.rs`)

- `encode_message(&Message) -> Result<Vec<u8>, ProtoError>` — postcard body, no prefix.
- `decode_message(&[u8]) -> Result<Message, ProtoError>` — the inverse.
- `encode_frame(&Message)` — `u32` little-endian length + body; fails with `FrameTooLarge` above
  `MAX_FRAME_LEN`.
- `FrameDecoder` — incremental decoder for a byte stream: `push(bytes)` then call
  `next_message()` until it returns `Ok(None)` (needs more bytes). It never panics on garbage;
  bad input becomes a `ProtoError`.

### Messages (`messages.rs`)

`Message` is one enum; postcard encodes the **variant index** as the first byte, so the order of
variants is part of the protocol. The full table, field by field, is in the
[wire protocol](../../protocol.md). In short:

| Index | Variant | Since |
|---|---|---|
| 0–8 | `Hello`, `AuthRequest`, `AuthResult`, `VideoFrame`, `InputEvent`, `ClipboardData`, `FileChunk`, `Permissions`, `Bye` | v1 |
| 9 | `RequestKeyframe` | v2 |
| 10–12 | `HostInputPriority`, `MonitorList`, `SelectMonitor` | v3 |
| 13–14 | `FsRequest`, `FsResponse` — remote file system | v4 |
| 15–21 | `TransferOffer`, `TransferAccept`, `TransferReject`, `TransferProgress`, `TransferCancel`, `TransferDone`, `TransferError` — file transfer | v4 |
| 22–23 | `ClipboardFiles`, `ClipboardHtml` | v4 |
| 24 | `ChatMessage` | v4 |
| 25 | `AudioControl` | v4 |
| 26–31 | `SendCtrlAltDel`, `RestartRemote`, `ReconnectTicket`, `PrivacyMode`, `DesktopState`, `LockWorkstation` — session and system control | v4 |
| 32–36 | `SetQuality`, `ViewerFeedback`, `LatencyProbe`, `LatencyEcho`, `SelectAllMonitors` — quality and latency | v4 |
| 37–39 | `TunnelOpen`, `TunnelOpened`, `TunnelClose` — TCP port forwarding | v4 |
| 40–41 | `WhiteboardStroke`, `WhiteboardClear` | v4 |
| 42–43 | `DeviceInfoRequest`, `DeviceInfo` | v4 |

`AuthRequest` also gained `ReconnectTicket { token }` (index 4) in v4.

> **Defined is not implemented.** Protocol v4 reserves the wire format for most of the roadmap
> (M2–M4) up front so feature lanes can work in parallel without bumping the version again. As of
> this writing only the v1–v3 messages are acted on by the host and viewer; the endpoint answers
> `AuthRequest::ReconnectTicket` with `Rejected(NotAllowed)` (test
> `reconnect_ticket_is_refused_for_now`), and the other v4 messages have no handler yet.

`Message::required_permissions()` returns the permission the host requires before it acts on an
incoming message from the viewer — for example `VIEW` for `RequestKeyframe` / `SelectMonitor`,
`CONTROL` for `InputEvent` (the host additionally checks `KEYBOARD` per key event), `CLIPBOARD`
for `ClipboardData`, `FILES` for `FsRequest` and the transfer messages, `CLIPBOARD | FILES` for
`ClipboardFiles`, `CHAT`, `AUDIO`, `SYSTEM`, `RESTART`, `PRIVACY_MODE`, `TUNNEL` and
`WHITEBOARD` for their features. The authoritative mapping is the `match` in `messages.rs` and the
test `v4_permissions_gate_feature_messages`. The host does not call it yet — it currently gates
the v1–v3 messages it handles by hand — so a feature lane that adds a handler should route through
`required_permissions()` rather than add another hand-written check.

### Permissions

`Permissions(u16)` is a bit set with `contains`, `union`, `difference` and `|`:

| Bit | Name | Bit | Name |
|---|---|---|---|
| 0 | `VIEW` | 6 | `RESTART` |
| 1 | `CONTROL` | 7 | `PRIVACY_MODE` |
| 2 | `KEYBOARD` | 8 | `CHAT` |
| 3 | `CLIPBOARD` | 9 | `TUNNEL` |
| 4 | `FILES` | 10 | `WHITEBOARD` |
| 5 | `AUDIO` | 11 | `SYSTEM` (Ctrl+Alt+Del, lock) |

`Permissions::NONE` is `0`, `Permissions::ALL` is `0x0FFF`.

### Stream headers (`stream.rs`, v4)

From protocol v4 on, every QUIC stream other than the control stream is meant to start with a
9-byte `StreamHeader { kind: StreamKind, id: u64 }` (`kind` as `u8`, then `id` little-endian):

| `StreamKind` | Value | Direction | `id` |
|---|---|---|---|
| `Video` | 0 | host → viewer, uni | frame `seq` |
| `FileData` | 1 | uni | transfer id (followed by a 12-byte `FileDataHeader { item, offset }`) |
| `Tunnel` | 2 | bidi, opened by the viewer | tunnel id |
| `Audio` | 3 | host → viewer, uni | audio stream generation |
| `Clipboard` | 4 | uni | clipboard sequence number |

Unknown kinds decode to `ProtoError::UnknownStreamKind`.

`tlp-desk-net` writes the header on every video stream and on side streams opened with
`Session::open_uni` / `open_bi`, and routes incoming streams by kind — see
[tlp-desk-net](net.md#side-streams-v4).

### Errors

`ProtoError`: `Serde(postcard::Error)`, `FrameTooLarge(usize)`, `UnknownStreamKind(u8)`.

## Rules for changing this crate

1. **Append only.** Never reorder, remove or insert variants above existing ones — in `Message`
   or in any nested enum. New variants go at the very end (`// FEATURE HOOK` marks the spot).
2. **Bump `PROTOCOL_VERSION`** for any change to a variant or field layout. Postcard is not
   self-describing, so two builds with different layouts silently misread each other.
3. **Update `docs/protocol.md`** in the same change (it is included in this book as the
   [wire protocol](../../protocol.md) page).
4. Add the new variant to the round-trip samples and pin its index in a test like
   `v4_variants_are_appended_in_order`.

## Tests

All tests run in CI (none are ignored).

- `tests/roundtrip.rs`: `every_message_roundtrips`, `frames_decode_from_byte_by_byte_stream`,
  `oversized_length_prefix_is_rejected`, `garbage_body_is_an_error_not_a_panic`,
  `permissions_bit_ops`, `permission_bits_are_stable_and_distinct`,
  `v1_variant_indices_are_stable`, `v3_variants_are_appended`,
  `v4_variants_are_appended_in_order`, `v4_permissions_gate_feature_messages`,
  `protocol_version_is_4`.
- `src/stream.rs`: `stream_header_roundtrips_every_kind`, `unknown_stream_kind_is_rejected`,
  `kind_values_are_stable`, `file_data_header_roundtrips`.

```powershell
cargo test -p tlp-desk-proto
```
