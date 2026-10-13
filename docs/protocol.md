# tlp-desk wire protocol

Status: **draft**, `PROTOCOL_VERSION = 4`. Source of truth: `crates/tlp-desk-proto` (messages),
`crates/tlp-desk-net` (transport, discovery) and `crates/tlp-desk-crypto` (keys, PAKE).

### Changes in v4

- Every QUIC stream other than the control stream now starts with a 9-byte stream header
  (`kind: u8` + `id: u64` LE, see *Streams*); video frames use kind `Video` with `id = seq`.
  Feature data (file bytes, tunnels, audio, large clipboard payloads) travels on typed side streams.
- Appended `Message` variants 13-43 for remote files, file transfer, clipboard files/HTML, chat,
  audio, session/system control (M3), quality and latency, all-monitors view, TCP tunnels,
  whiteboard and device info (see the table and *Features (v4)*).
- Appended `AuthRequest::ReconnectTicket { token }` (index 4).
- New permission bits 8-11: `CHAT`, `TUNNEL`, `WHITEBOARD`, `SYSTEM`.
- The host enforces `Message::required_permissions()` for every message from the viewer.
- Appended only; all v1-v3 variants keep their indices.

### Changes in v3

- Added `Message::HostInputPriority { local_active }` (host user has taken over the input).
- Added `Message::MonitorList { monitors, active }` and `Message::SelectMonitor { index }`
  (multi-monitor hosts: list and switch the streamed monitor live).
- Appended only; all v1/v2 variants keep their indices.

### Changes in v2

- Added `AuthRequest::KeyConfirm { mac }` and `AuthResult::KeyConfirm { mac }` (SPAKE2 key
  confirmation, see *Authentication*).
- Added `Message::RequestKeyframe` (viewer asks for a keyframe after a lost/dropped frame).
- Video moved from one long-lived uni stream to **one uni stream per frame**.
- All v1 variants keep their indices (new ones are only appended), so a v1 peer can still decode
  the `Rejected { VersionMismatch }` it receives.

## Transport

- QUIC (`quinn`) over UDP port **47400** (default bind `[::]:47400`, dual-stack), TLS 1.3 only,
  ALPN `tlp-desk` (version-less on purpose so a version mismatch reaches the `Hello` check).
  No 0-RTT.
- **Identity**: each install has an ECDSA P-256 key (`identity.key.pem`, PKCS#8) in
  `%ProgramData%\tlp-desk\`, or the per-user fallback `%LOCALAPPDATA%\tlp-desk\` when ProgramData
  is not writable. The self-signed X.509 certificate is regenerated from the key on every start;
  only the key matters.
- **Fingerprint** = SHA-256 of the DER `SubjectPublicKeyInfo` of the certificate.
- **Device ID** = `u64::from_be_bytes(fingerprint[0..8]) % 900_000_000 + 100_000_000`
  (always 9 digits; displayed as `123 456 789`).
- **Mutual TLS.** The viewer verifies the host certificate by pinning:
  - dialed by device ID: the host fingerprint must map to that ID, otherwise the handshake is
    aborted (`PinMismatch`);
  - dialed with a known fingerprint (known-hosts store): exact fingerprint match;
  - dialed by IP/hostname only: trust-on-first-use; the observed fingerprint is returned and the
    app must warn loudly if it differs from the stored one.

  The host requires a client certificate too; the TLS handshake signature proves the viewer owns
  the key, so the viewer's device ID shown on the host is authentic.
- **Allow-list**: the host refuses peers outside the configured CIDR list before the handshake.
  Default: `10/8`, `172.16/12`, `192.168/16`, `169.254/16`, `127/8`, `fc00::/7`, `fe80::/10`,
  `::1`. IPv4-mapped IPv6 addresses are compared as IPv4.
- **Streams**:
  - **Control**: one bidirectional stream opened by the viewer. Carries every message except video.
  - **Video**: the host opens **one unidirectional stream per `VideoFrame`** after
    `AuthResult::Accepted`, writes exactly one framed `Message::VideoFrame` and finishes it.
- Keep-alive 4 s, idle timeout 15 s.
- Application close codes: `0` normal, `1` rejected, `2` protocol error.

### Why one stream per video frame

A single long-lived stream is a FIFO: once a frame is written, every later frame waits behind it
(head-of-line blocking) and nothing can be dropped, so latency grows when the viewer or the link
falls behind. QUIC datagrams avoid that but are limited to ~1.2 KB, so every frame would need
application-level fragmentation, reassembly and loss handling. A stream per frame keeps reliable
delivery of each frame (the H.264 decoder needs complete frames) while frames stay independent:

- **Host (drop instead of queue)**: a frame counts as *in flight* until the viewer has
  acknowledged all its bytes. With 3 or more frames in flight, non-key frames are dropped and the
  encoder is told to emit a keyframe next; until a keyframe is offered, all non-key frames are
  dropped. Keyframes are dropped only at 6 or more in flight.
- **Viewer**: frames are read concurrently and handed to the decoder in `seq` order through a
  4-frame queue (overflow is dropped); stale frames (`seq` <= last delivered) are discarded. After
  any gap, non-key frames are skipped until a keyframe arrives, and `RequestKeyframe` is sent on
  the control stream (at most every 250 ms).
- `seq` must increase by exactly 1 per encoded frame; the first frame of a session must be a
  keyframe.

## Framing

Each message is a `Message` enum value serialized with `postcard`, prefixed by its body length:

```
+----------------------+------------------------+
| len: u32 little-end. | body: postcard(Message)|
+----------------------+------------------------+
```

Bodies larger than `MAX_FRAME_LEN` (16 MiB) are a protocol error and the connection is closed.

## Messages

| Message | Direction | Purpose |
|---|---|---|
| `Hello { protocol_version, device_id, device_name, role }` | both, first on control | Identify peer, check version |
| `AuthRequest::AcceptPrompt` | viewer → host | Ask the host user to click Accept |
| `AuthRequest::OneTimeCode { spake2_msg }` | viewer → host | SPAKE2 keyed by the 6-digit one-time code |
| `AuthRequest::Password { spake2_msg }` | viewer → host | SPAKE2 keyed by the unattended password |
| `AuthResult::Spake2Reply { spake2_msg }` | host → viewer | Host's SPAKE2 half |
| `AuthRequest::KeyConfirm { mac }` | viewer → host | Viewer's SPAKE2 key confirmation (v2) |
| `AuthResult::KeyConfirm { mac }` | host → viewer | Host's SPAKE2 key confirmation (v2) |
| `AuthRequest::ReconnectTicket { token }` | viewer → host | Re-authorize with a single-use ticket (v4; refused until implemented) |
| `AuthResult::Accepted { permissions }` | host → viewer | Session authorized |
| `AuthResult::Rejected { reason }` | host → viewer | `UserDeclined`, `WrongCredentials`, `RateLimited`, `NotAllowed`, `VersionMismatch` |
| `VideoFrame { seq, timestamp_us, width, height, codec, keyframe, data }` | host → viewer (one uni stream per frame) | One encoded frame, H.264 Annex-B |
| `InputEvent` | viewer → host | Mouse/keyboard; see below |
| `ClipboardData::Text / Image` | both | Clipboard sync (needs `CLIPBOARD`) |
| `FileChunk { transfer_id, offset, data, last }` | both | File transfer (needs `FILES`) |
| `Permissions(u16)` | host → viewer | Live permission update/revocation |
| `Bye { reason }` | both | Graceful close |
| `RequestKeyframe` | viewer → host | Decoder lost sync; encode the next frame as a keyframe (v2) |
| `HostInputPriority { local_active }` | host → viewer | The host user is using their own mouse/keyboard; viewer input is dropped until `false` (v3) |
| `MonitorList { monitors: [MonitorDesc], active }` | host → viewer | Host monitors and the streamed one (v3) |
| `SelectMonitor { index }` | viewer → host | Switch the streamed/controlled monitor (v3) |
| `FsRequest { req_id: u64, op: FsOp }` | viewer → host (either) | Remote file-system operation (v4, `FILES`) |
| `FsResponse { req_id: u64, result: Result<FsReply, FsError> }` | host → viewer (either) | Answer to `FsRequest` (v4) |
| `TransferOffer(TransferOffer)` | viewer → host | Start an upload or download (v4, `FILES`) |
| `TransferAccept { id: u64, resume: Vec<(u32, u64)> }` | host → viewer | Offer accepted; upload resume points (v4) |
| `TransferReject { id: u64, error: FsError }` | host → viewer | Offer refused (v4) |
| `TransferProgress { id: u64, bytes_done: u64 }` | receiver → sender | Bytes written so far (v4) |
| `TransferCancel { id: u64 }` | both | Abort a transfer (v4) |
| `TransferDone { id: u64, item: u32, blake3: [u8; 32] }` | sender → receiver | Item fully sent; hash to verify (v4) |
| `TransferError { id: u64, item: Option<u32>, error: FsError }` | both | Transfer or item failed (v4) |
| `ClipboardFiles { transfer_id: u64, items: Vec<TransferItem> }` | both | Files copied; pasted later via a transfer (v4, `CLIPBOARD`+`FILES`) |
| `ClipboardHtml { html: String, alt_text: String }` | both | HTML clipboard + plain text (v4, `CLIPBOARD`) |
| `ChatMessage { id: u64, text: String, sent_unix_ms: u64 }` | both | Chat (v4, `CHAT`) |
| `AudioControl { enabled: bool, sample_rate: u32, channels: u8 }` | both | Start/stop system audio; format (v4, `AUDIO`) |
| `SendCtrlAltDel` | viewer → host | Secure attention sequence (v4, `SYSTEM`) |
| `RestartRemote { reconnect: bool }` | viewer → host | Restart the host PC (v4, `RESTART`) |
| `ReconnectTicket { token: Vec<u8>, expires_unix_ms: u64 }` | host → viewer | Single-use re-auth ticket (v4) |
| `PrivacyMode { enabled: bool }` | both | Blank host screen + block local input / actual state (v4, `PRIVACY_MODE`) |
| `DesktopState { secure_desktop: bool, locked: bool }` | host → viewer | Secure desktop / lock state (v4) |
| `LockWorkstation` | viewer → host | Lock the host (v4, `SYSTEM`) |
| `SetQuality(QualitySettings)` | viewer → host | Preset, size cap, fps, bitrate (v4, `VIEW`) |
| `ViewerFeedback { decode_us: u32, frames_dropped: u32, rtt_us: u32 }` | viewer → host | Adaptive-bitrate input, ~1/s (v4) |
| `LatencyProbe { t_viewer_us: u64 }` | viewer → host | Latency overlay probe (v4) |
| `LatencyEcho { t_viewer_us: u64, t_host_capture_us: u64 }` | host → viewer | Probe echo (v4) |
| `SelectAllMonitors` | viewer → host | Stream the whole virtual desktop (v4) |
| `TunnelOpen { id: u64, target_port: u16, target_host: Option<String> }` | viewer → host | TCP port forwarding (v4, `TUNNEL`) |
| `TunnelOpened { id: u64 }` | host → viewer | Tunnel allowed (v4) |
| `TunnelClose { id: u64, error: Option<String> }` | both | Tunnel closed / refused (v4, `TUNNEL`) |
| `WhiteboardStroke { id: u64, points: Vec<(u16, u16)>, color_rgba: u32, width: u16 }` | both | Draw on the streamed monitor (v4, `WHITEBOARD`) |
| `WhiteboardClear` | both | Erase all strokes (v4, `WHITEBOARD`) |
| `DeviceInfoRequest` | viewer → host | Ask for system info (v4) |
| `DeviceInfo(DeviceInfo)` | host → viewer | OS, CPU, RAM, disks, uptime (v4) |
`MonitorDesc { index: u32, name: String, width: u32, height: u32, primary: bool, x: i32, y: i32 }`
(physical pixels on the host virtual desktop).

### Local input priority (v3)

The person at the host always wins. The host watches its own physical mouse and keyboard
(low-level hooks; events injected by software, including the host's own `SendInput`, are
ignored). While a physical local event happened within the grace window (default 1000 ms),
the host drops every `MouseMove`, `MouseButton`, `MouseWheel` and `Key` from the viewer; on
the transition into local priority it releases every key and button the viewer still holds.
Viewer input is accepted again once the grace window passes without local input. The host
sends `HostInputPriority` on each change so the viewer can show a notice.

While the viewer is moving the host pointer (and the host user is not), the host shows a green
marker at the pointer on its own screen; the marker is excluded from capture
(`WDA_EXCLUDEFROMCAPTURE`) so it never appears in the video.

### Monitors (v3)

After `Accepted` the host sends `MonitorList`, and again whenever the monitor set (polled about
every 2 s) or the active monitor changes. On `SelectMonitor` with a known index the host
reopens capture on that monitor (the next frame is a keyframe, possibly with a new size) and
maps pointer input to that monitor's rectangle; unknown indices are ignored. `MouseMove`
coordinates are always normalized over the currently streamed monitor.

### Input

- `MouseMove { x, y }`: coordinates normalized to `0..=65535` over the captured monitor, so the
  viewer does not need to know the host resolution or DPI.
- `MouseButton { button, down }`, `MouseWheel { dx, dy }` (units of 1/120 notch).
- `Key { scancode, extended, down }`: hardware scancodes, layout-independent (Arabic/English work
  without translation).
- `ReleaseAll`: sent when the viewer window loses focus so no key stays stuck.

### Permissions bits

| Bit | Name |
|---|---|
| 0 | `VIEW` |
| 1 | `CONTROL` |
| 2 | `KEYBOARD` |
| 3 | `CLIPBOARD` |
| 4 | `FILES` |
| 5 | `AUDIO` |
| 6 | `RESTART` |
| 7 | `PRIVACY_MODE` |
| 8 | `CHAT` (v4) |
| 9 | `TUNNEL` (v4) |
| 10 | `WHITEBOARD` (v4) |
| 11 | `SYSTEM`: Ctrl+Alt+Del, lock workstation (v4) |

The host checks `Message::required_permissions()` before acting on any message from the viewer
(e.g. `FsRequest` needs `FILES`, `ClipboardFiles` needs `CLIPBOARD` and `FILES`); messages that lack
a permission are dropped (requests get a `PermissionDenied` answer where the message has one).
Side streams are checked by the feature that accepts them.

## Session flow

```
viewer                                   host
  |---- QUIC connect (mutual TLS, pinned) ->|   allow-list checked before the handshake
  |---- Hello ----------------------------->|
  |<--- Hello ------------------------------|   version mismatch -> Rejected(VersionMismatch)
  |                                         |   Hello.device_id != cert ID -> Rejected(NotAllowed)
  |---- AuthRequest ----------------------->|
  |        AcceptPrompt: host user decides  |
  |        OneTimeCode / Password:          |
  |<--- AuthResult::Spake2Reply ------------|
  |---- AuthRequest::KeyConfirm ----------->|   wrong MAC -> Rejected(WrongCredentials)
  |<--- AuthResult::KeyConfirm -------------|   viewer verifies, aborts on mismatch
  |<--- AuthResult::Accepted{permissions} --|   (or Rejected{reason} at any step)
  |<=== uni stream per VideoFrame ==========|
  |---- InputEvent / Clipboard / Files ---->|
  |---- RequestKeyframe ------------------->|
  |<--- MonitorList ------------------------|   after Accepted, and on changes (v3)
  |---- SelectMonitor --------------------->|   (v3)
  |<--- HostInputPriority ------------------|   on change (v3)
  |<--- Permissions (live changes) ---------|
  |---- Bye ------------------------------->|
```

## Authentication

- **Accept prompt**: the host shows the viewer's authenticated device ID and self-reported name;
  the user clicks Accept or Reject (`Rejected(UserDeclined)`).
- **One-time code / password: SPAKE2** (`spake2` crate, Ed25519 group; viewer = side A, host =
  side B). The secret never crosses the wire.
  - Secret: the 6 ASCII digits of the one-time code; for the password, the 32-byte
    `PasswordVerifier = Argon2id(m=19 MiB, t=2, p=1, password, salt = "tlp-desk/pw/v2" || host_fp)`.
    The host stores only the verifier.
  - SPAKE2 identities: `"tlp-desk/v2/viewer/" || hex(viewer_fp)` and
    `"tlp-desk/v2/host/" || hex(host_fp)`, using the fingerprints each side saw in TLS.
  - Channel binding: 32 bytes of TLS exporter output, label `EXPORTER-tlp-desk-pake-v2`, empty
    context.
  - Transcript `T` = SHA-256 over length-prefixed (`u64` BE) fields: `"tlp-desk/pake/v2"`, method
    (`"code"` / `"password"`), viewer_fp, host_fp, channel binding, viewer msg, host msg.
  - Key confirmation: viewer MAC = `HMAC-SHA256(K, "tlp-desk/kc/viewer" || T)`, host MAC =
    `HMAC-SHA256(K, "tlp-desk/kc/host" || T)`, compared in constant time. The host sends its MAC
    only after the viewer's verified. A wrong secret therefore fails cleanly on both sides, and a
    MITM terminating TLS separately on each side fails because fingerprints and exporters differ.
- **One-time code**: 6 digits from the OS CSPRNG; rotates after each successful use and after 3
  failed attempts.
- **Rate limit** (code and password): max 5 attempts per minute per peer IP; the 6th starts a
  10-minute lockout (`Rejected(RateLimited)`); success clears the counter. Every attempt is
  logged.

## Discovery

- **mDNS** (`mdns-sd`): service type `_tlpdesk._udp.local.`, instance name = device ID, host name
  `tlpdesk-<id>.local.`, SRV port = QUIC port. TXT records: `id` (9 digits), `name` (at most 64
  chars), `port`, `pv` (protocol version).
- **Broadcast fallback** on UDP **47401**: packet = `b"TLPD" || postcard(BroadcastPacket)`, at most
  512 bytes:

  ```
  BroadcastPacket { kind: Announce | Query, protocol_version: u32, device_id: u32,
                    name: String (at most 64 chars), port: u16 }
  ```

  Hosts send `Announce` every 5 s to each IPv4 interface's subnet broadcast and
  `255.255.255.255`. Browsers send `Query` on start/refresh (`device_id = 0` when not
  advertising); hosts reply with a unicast `Announce`. Broadcast-only entries expire after 16 s of
  silence.
- Both sources are merged by device ID; peers outside the allow-list are ignored. Manual IP
  connect is always possible.
- **Wake-on-LAN**: standard magic packet (6 x `0xFF` + MAC x 16) to UDP port 9 on the subnet
  broadcasts.

## Versioning

Any change to a `Message` variant or field layout is incompatible (postcard is not
self-describing): bump `PROTOCOL_VERSION` and update this document in the same change.
