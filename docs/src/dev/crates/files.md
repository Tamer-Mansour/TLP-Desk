# tlp-desk-files

**File transfer and clipboard sync — placeholder.** The crate contains only a module doc comment
and `#![forbid(unsafe_code)]`; no dependencies, no API, no tests yet.

| | |
|---|---|
| Path | `crates/tlp-desk-files` |
| Roadmap | M2 Lane A — clipboard sync both ways; file transfer window (browse remote, upload/download, resume, progress, drag-and-drop); copy in Explorer on one side, paste on the other |
| Planned stack | `arboard` for the clipboard (CLAUDE.md tech choices) |
| Permission bits | `CLIPBOARD` (bit 3), `FILES` (bit 4) |

## Wire protocol pieces that already exist

The protocol was extended ahead of the implementation, so this lane can start without a protocol
bump for the basics:

- **`ClipboardData`** (`Text(String)` or `Image { width, height, rgba }`) — host ↔ viewer,
  requires `CLIPBOARD`.
- **`FileChunk { transfer_id, offset, data, last }`** — requires `FILES`.
- **Protocol v4 remote file system:** `FsRequest { req_id, op }` answered by exactly one
  `FsResponse { req_id, result }`. Operations (`FsOp`): `ListDir`, `Stat`, `Roots`, `MakeDir`
  (creates missing parents), `Rename` (same volume), `Delete { recursive }`. Replies: `Entries`,
  `Entry`, `Roots` (drives and well-known folders — `Home`, `Desktop`, `Documents`, `Downloads`,
  `Pictures`, `Music`, `Videos` — with optional total/free bytes), `Done`. Errors (`FsError`):
  `NotFound`, `PermissionDenied`, `AlreadyExists`, `NotEmpty`, `InvalidPath`, `NoSpace`, `Busy`,
  `Declined`, `Cancelled`, `HashMismatch`, `Other`.
- **Stream kinds** `FileData = 1` (header + 12-byte `FileDataHeader { item, offset }`, so one file
  can be sent as several parallel ranges) and `Clipboard = 4`.

Paths travel as UTF-8 strings in the receiver's native form. On Windows that means converting to
and from UTF-16 at the OS boundary — Arabic file and folder names must survive the round trip
(there are already Arabic-path samples in the proto round-trip tests).

## Things not defined yet

Doc comments in `tlp-desk-proto/src/stream.rs` refer to `TransferOffer` and `ClipboardHtml`, and
`FsError::HashMismatch` mentions `blake3`; none of these exist yet. Adding them is a protocol
change (bump `PROTOCOL_VERSION`, update `docs/protocol.md`).

## Arabic checklist for this lane

From the Arabic section of the roadmap: clipboard sync must preserve Arabic text exactly
(including RTL marks, ZWJ and ZWNJ), and file transfer must handle Arabic file and folder names in
the UI and on disk.

When you implement it, update this page, the user guide pages for
[file transfer](../../user-guide/en/coming-soon/file-transfer.md) and
[clipboard](../../user-guide/en/coming-soon/clipboard.md), and the
[changelog](../../changelog.md).
