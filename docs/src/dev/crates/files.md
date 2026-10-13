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

- **File transfer control** (v4): `TransferOffer { id, direction, source_dir, dest_dir, items,
  total_bytes, resume }`, `TransferAccept { id, resume }`, `TransferReject`, `TransferProgress`
  (at most ~4 per second), `TransferCancel`, `TransferDone { id, item, blake3 }` (the receiver
  verifies a BLAKE3 hash of each file) and `TransferError`. Item paths are relative,
  `/`-separated and must not contain `..`. File bytes never travel on the control stream.
- **Clipboard extensions** (v4): `ClipboardHtml { html, alt_text }` and `ClipboardFiles {
  transfer_id, items }` — files copied on one side are only transferred when the user pastes on
  the other.

The full flow is specified in the [wire protocol](../../protocol.md), section *Features (v4)*.
The BLAKE3 hash implies a new dependency for this crate (check it against `deny.toml`).

## Arabic checklist for this lane

From the Arabic section of the roadmap: clipboard sync must preserve Arabic text exactly
(including RTL marks, ZWJ and ZWNJ), and file transfer must handle Arabic file and folder names in
the UI and on disk.

When you implement it, update this page, the user guide pages for
[file transfer](../../user-guide/en/coming-soon/file-transfer.md) and
[clipboard](../../user-guide/en/coming-soon/clipboard.md), and the
[changelog](../../changelog.md).
