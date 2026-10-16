# Explorer copy and paste between the PCs

Goal: Ctrl+C on files in Explorer on one PC, Ctrl+V in Explorer (or on the desktop) on the other
PC, in both directions. Code: `crates/tlp-desk-files/src/shellclip/` (COM, Windows only for the
`unsafe` part) and `clipfiles/` (announcements). Requires `CLIPBOARD` and `FILES`.

## Flow

```text
PC A: Ctrl+C in Explorer           clipboard has CF_HDROP
  clipfiles watcher  ---- ClipboardFiles { id, items } ---->  PC B: clipfiles (peer_loop)
                                                               RemoteHandler -> shellclip::engine
                                                               ShellClipboard::offer(...)
                                                               OleSetClipboard(VirtualFiles)
PC B: Ctrl+V in Explorer
  GetData(FileGroupDescriptorW)   names, sizes, times (descriptor.rs)
  GetData(FileContents, i)        returns a VirtualStream; begin() starts the pull
  stream.Read(...)                waits until the whole file is staged, then reads it
```

Nothing moves until the first `FileContents` request (delayed rendering).

## Who pulls

* **Host copied, viewer pastes.** The viewer's `ViewerStager` starts a normal download
  (`start_download_items` under the announced id) into a staging folder. The host resolves the
  id with `set_announced_source` (the folder the items are relative to).
* **Viewer copied, host pastes.** Only the viewer opens transfers, so the host's `HostStager`
  *echoes* the announcement. The viewer's upload handler uploads the files it announced with an
  empty `dest_dir`; the host maps `(session, id)` to its staging folder (`set_announced_dest`).
  This reuses the v4/v6 messages (see `protocol.md`, `ClipboardFiles`); no version bump.

## Staging and streams

Files are staged under `%TEMP%\TLP-Desk\paste\<session>-<id>`. The engine writes
`name.tlpdesk-part` and renames after BLAKE3 verification, so "the final file exists with the
announced size" is the completion signal (`staged.rs`). `IStream::Read` waits for it while
pumping window messages, bounded by: cancel (offer replaced / cleared / session ended), a failed
or cancelled transfer, and a 60 s start timeout. Folders come from the descriptor only
(`FD_ATTRIBUTES` directory bit); Explorer does not request contents for them.

## COM design (`windows.rs`, the only `unsafe`)

* One thread `tlp-shellclip`: `OleInitialize` (STA), a `GetMessageW` loop. Commands (`Set`,
  `Clear`) are queued and signalled with a thread message. `OleSetClipboard` must run on a
  pumping STA, and Explorer's calls arrive on that thread.
* `VirtualFiles` implements `IDataObject` and `IDataObjectAsyncCapability` (`SetAsyncMode`,
  `StartOperation`, `EndOperation`), so Explorer copies on a worker thread with its own progress
  dialog. Formats: `FileGroupDescriptorW`, `FileContents` (`TYMED_ISTREAM`, one per file),
  `Preferred DropEffect` (copy); `SetData` accepts `Performed DropEffect` and `Paste Succeeded`.
* A blocked `Read` pumps messages every ~20 ms so other COM calls and `Release`s are served; our
  own command message is put back for the main loop.
* The object is *not* flushed (`OleFlushClipboard` would render everything). `Clear` empties the
  clipboard only if `OleIsCurrentClipboard` says it is still ours, so a later user copy is never
  wiped. When OLE drops the object (replaced by another copy) its `Drop` cancels the offer.

## Lifecycle and permissions

`ShellClipboard` keeps one offer per process, keyed by session. A new announcement replaces the
old one (cancel + delete its staging folder). The clipfiles loop clears the remote entry (and so
the clipboard) when the peer announces nothing, when `CLIPBOARD` or `FILES` is revoked, and when
the session ends. The transfer engine checks `FILES` on every offer anyway. Stale staging folders
older than a day are purged on start.

## Service deployment

The clipboard belongs to the interactive user's desktop. The in-process GUI path (host and
viewer inside `TLP-Desk.exe`) owns the object directly, and that is what is implemented. When the
host runs as a Windows service, session 0 has no user clipboard: the agent that runs in the
logged-in user's session must own `ShellClipboard` and run `shellclip::engine::install` next to
the clipboard sync feature it already hosts. That agent wiring is not done yet.

## Tests

`descriptor.rs`, `staged.rs`, `shellclip/mod.rs` (unit: layout with Arabic names, folders, sizes
over 4 GiB, times; reads with a fake engine; lifecycle with a fake port), `tests/clipfiles_e2e.rs`
(announcements and pull handlers over loopback) and `tests/shellclip_e2e.rs` (both directions
through the real transfer engine with a fake OS clipboard). The test that really replaces the
clipboard is `#[ignore]`.
