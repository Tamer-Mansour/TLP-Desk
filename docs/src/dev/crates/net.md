# tlp-desk-net

**QUIC transport, TLS pinning, the authentication handshake, LAN discovery and Wake-on-LAN.**
No function in this crate contacts anything outside the local network.

| | |
|---|---|
| Path | `crates/tlp-desk-net` |
| Depends on | `proto`, `crypto`, `quinn` 0.11, `rustls` 0.23 (ring), `tokio`, `mdns-sd` 0.13, `socket2`, `if-addrs` |
| Used by | `host`, `viewer`, `app`, `cli` |
| Unsafe | `#![forbid(unsafe_code)]` |
| Threads | tokio tasks; `Endpoint`, `Session` and `Discovery` must be created inside a runtime |

## Constants

| Item | Value |
|---|---|
| `DEFAULT_PORT` | `47400` (QUIC, UDP) |
| `BROADCAST_PORT` | `47401` (discovery fallback, UDP) |
| `ALPN` | `b"tlp-desk"` — version-less so a mismatched peer still reaches the `Hello` check |
| `MDNS_SERVICE_TYPE` | `_tlpdesk._udp.local.` |
| `BROADCAST_MAGIC` | `b"TLPD"` |
| `DEFAULT_MAX_VIDEO_IN_FLIGHT` | `3` |
| `WOL_PORT` | `9` |

## TLS and pinning (`tls.rs`)

- TLS 1.3 only, `ring` provider, 0-RTT disabled on both sides.
- **Mutual TLS.** The host requires a client certificate (`AnyClientKeyVerifier` accepts any
  well-formed key — the handshake signature proves the viewer owns it, so the viewer's device ID
  shown on the host is authentic).
- The viewer uses `PinnedServerVerifier` with a `Pin`:
  - `Pin::DeviceId(id)` — the host's fingerprint must map to that ID;
  - `Pin::Fingerprint(fp)` — exact match (known-hosts entry);
  - `Pin::Tofu` — accept anything, but record what was seen.

  The observed fingerprint is recorded even when the certificate is rejected, so the app can show
  *which* key it got. `From<Option<DeviceId>>` maps `None` to `Tofu`.
- The SNI sent when dialing is always `tlp-desk`.
- Channel binding for SPAKE2: 32 bytes of `export_keying_material`, label
  `EXPORTER-tlp-desk-pake-v2`, empty context.

`transport_config()`:

| Setting | Value |
|---|---|
| keep-alive interval | 4 s |
| max idle timeout | 15 s |
| max concurrent uni streams | 32 |
| max concurrent bidi streams | 4 |
| stream receive window | 8 MiB |
| connection receive window | 32 MiB |
| send window | 8 MiB |

## Endpoint (`endpoint.rs`)

One `Endpoint` both listens (host) and dials (viewer); the GUI shares a single endpoint on UDP
47400 for both roles.

- `Endpoint::bind(identity, EndpointConfig)` — binds dual-stack `[::]:port` (`set_only_v6(false)`).
  `AddrInUse` is returned as an error; other bind errors fall back to `0.0.0.0:port`.
- `EndpointConfig` defaults: `bind = [::]:47400`, `device_name = default_device_name()`
  (`COMPUTERNAME`, else `HOSTNAME`, else `tlp-desk`), `allow_list = AllowList::default()`,
  `rate_limit = RateLimitConfig::default()`, `listen = true`, `handshake_timeout = 10 s`.
- `accept() -> Option<PendingSession>` — next authenticated-so-far incoming connection.
- `connect(addr, expected_id, auth)`, `connect_pinned(addr, pin, auth)` and
  `connect_pinned_with_progress(addr, pin, auth, on_awaiting_accept)` — the last one calls a
  hook once the host has shown its Accept prompt, so the UI can say "waiting for the other side".
- `set_allow_list`, `local_addr`, `device_id`, `fingerprint`, `close`, `wait_idle`.

### Host side of the handshake

1. The allow-list is checked on the *incoming* packet and non-matching peers are refused before
   any TLS work.
2. Each connection gets its own task bounded by `handshake_timeout`.
3. `Hello` is read: a different `protocol_version` → `Rejected(VersionMismatch)`; a
   `device_id` that does not match the client certificate, or the wrong role →
   `Rejected(NotAllowed)`.
4. For code or password requests, a peer IP that is locked out gets `Rejected(RateLimited)`.
5. The result is queued (capacity 16) as a `PendingSession`. The application decides:
   - `accept(permissions)` — only for `AuthKind::AcceptPrompt`;
   - `verify_code(&OneTimeCode, permissions)` / `verify_password(&PasswordVerifier, permissions)`
     — runs SPAKE2: count the attempt in the rate limiter, send `Spake2Reply`, wait for the
     viewer's `KeyConfirm`; on success rotate the code, clear the limiter, send the host
     `KeyConfirm` then `Accepted`; on a wrong secret record a failure and send
     `Rejected(WrongCredentials)`;
   - `reject(reason)`; dropping the `PendingSession` closes the connection;
   - `cancelled().await` resolves if the viewer gives up.

`reject_and_close` sends the `Rejected` message, waits up to 3 s for the viewer to close, then
closes with application code 1.

### Viewer side

- The password verifier (Argon2id) is derived with `spawn_blocking`.
- Every handshake step is bounded by `handshake_timeout`, **except** waiting for the host user to
  click Accept, which has no network-side timeout (the host enforces its own).

See the [authentication handshake diagram](../architecture/auth.md).

## Session (`session.rs`)

`Session` is the authenticated connection. All methods take `&self`; dropping it closes the
connection.

- `send(&Message)` / `recv()` on the control stream (priority 10).
- `send_video(VideoFrame) -> VideoSendStatus` (host): opens **one unidirectional stream per frame**
  (header `StreamKind::Video`, id = `seq`; priority 5) in a spawned task. A frame stays *in flight* until the viewer has acknowledged all
  its bytes. Non-key frames are `Dropped` when 3 or more are in flight or a keyframe is pending;
  keyframes are dropped only at 6 or more.
- `recv_video()` (viewer): a reader task accepts each uni stream in its own task, reads the frame
  and hands it over in `seq` order through a 4-frame queue. Stale frames are discarded; after a
  gap, non-key frames are skipped until a keyframe and `RequestKeyframe` is sent (at most every
  250 ms).
- `close(ByeReason)` sends `Bye`, waits up to 500 ms, closes with code 0.
- `peer()` → `PeerInfo { addr, device_id, fingerprint, name }` (`name` is self-reported, not
  authenticated). `rtt()`, `role()`, `initial_permissions()`, `video_in_flight()`, `closed()`.

### Side streams (v4)

Every non-control stream starts with a 9-byte `StreamHeader` (`kind`, `id`). A router task reads
the header (the peer has 10 s to send it) and hands the stream to the right place: video frames to
the decoder queue, everything else to a **per-kind queue** of up to 64 streams.

- `open_uni(kind, id)` / `open_bi(kind, id)` — open a side stream and write its header; then write
  the payload and `finish()` (or `reset()` to abort).
- `accept_side_stream(kind) -> SideStream { kind, id, send: Option<SendStream>, recv }` — wait for
  the next stream of that kind from the peer. Each kind has its own queue, so the file, tunnel,
  audio and clipboard features can wait concurrently; only one task should accept a given kind.
- **Permission checks are the caller's job**: a feature that receives, say, `FileData` without the
  `FILES` permission must refuse it with `recv.stop(STREAM_REFUSED.into())`.
- Unknown kinds, malformed headers, full queues and kinds nobody accepts are refused with
  `STREAM_REFUSED` (`STOP_SENDING` code 1).
- `stream_priority(kind)` — QUIC send priority, higher first: control 10, audio 6, video 5,
  clipboard 2, tunnel 1, file data −10. A multi-gigabyte copy therefore never delays input or
  video.

Close codes: `0` normal, `1` rejected, `2` protocol error. See
[ADR 0001](../../adr/0001-quic-stream-per-video-frame.md) for the per-frame stream design.

## Allow-list (`allow.rs`)

`AllowList::default()` (= `lan_default()`): `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`,
`169.254.0.0/16`, `127.0.0.0/8`, `fc00::/7`, `fe80::/10`, `::1/128`. IPv4-mapped IPv6
addresses are normalised to IPv4 first. `AllowList::parse(&["10.1.0.0/16", ...])` builds a
custom list; an empty list allows nobody. `Cidr` parses `addr/prefix` or a bare address.

## Discovery (`discovery.rs`)

`Discovery::start(DiscoveryConfig)` runs mDNS and the broadcast fallback together and merges
results by device ID. It fails only if *neither* could start.

| `DiscoveryConfig` field | Default |
|---|---|
| `advertise` | `None` (browse only) |
| `enable_mdns`, `enable_broadcast` | `true` |
| `broadcast_port` | 47401 |
| `announce_interval` | 5 s |
| `expiry` | 16 s (broadcast-only entries) |
| `allow_list` | default |

- **mDNS:** instance name = device ID, host name `tlpdesk-<id>.local.`, TXT `id`, `name` (≤ 64
  characters), `port`, `pv` (protocol version). Results without `id`, our own ID, and addresses
  outside the allow-list are ignored.
- **Broadcast:** `b"TLPD" || postcard(BroadcastPacket { kind, protocol_version, device_id, name,
  port })`, at most 512 bytes, sent to every IPv4 interface's subnet broadcast plus
  `255.255.255.255`. A `Query` is sent on start and on `refresh()`; advertising hosts `Announce`
  every 5 s and answer queries by unicast.
- `devices()` returns `DiscoveredDevice { device_id, name, addrs, protocol_version, via_mdns,
  via_broadcast, last_seen }` sorted by name then ID (IPv4 addresses first).
- `subscribe()` gives `DiscoveryEvent::Updated` / `Lost` (broadcast channel, capacity 64).

## Wake-on-LAN (`wol.rs`)

`parse_mac`, `magic_packet(mac)` (6 × `0xFF` + MAC × 16 = 102 bytes) and
`send_wake_on_lan(mac, target)` — to the given address or every subnet broadcast on UDP 9. The
user-facing feature (address book integration) is planned for M4.

## Errors

`NetError` wraps transport errors (`Io`, `Tls`, `Connect`, `Connection`, `Write`, `Read`,
`ReadToEnd`, `Proto`, `Crypto`) and adds the domain cases the UI maps to messages:
`PinMismatch { expected, actual_id, actual_fp }`, `NotAllowed(addr)`,
`VersionMismatch { ours, theirs }`, `Rejected(RejectReason)`, `AuthFailed` (host: viewer proved
the wrong secret), `HostAuthFailed` (viewer: host's MAC was wrong), `RateLimited`,
`WrongAuthKind`, `Timeout`, `Closed`, `Unexpected`, `Discovery`.

## Tests

- Unit: `allow_list_default_accepts_lan_only`, `allow_list_custom_cidrs`,
  `wol_magic_packet_layout`, `wol_sends_to_explicit_target`,
  `broadcast_packet_roundtrip_and_size`, `broadcast_targets_include_limited_broadcast`,
  `hello_version_mismatch_is_rejected`, `hello_with_spoofed_device_id_is_rejected`.
- `tests/session.rs` (two endpoints on loopback): `two_peers_connect_with_accept_prompt`,
  `awaiting_accept_hook_fires_before_the_host_decides`, `accept_prompt_reject_reaches_viewer`,
  `frames_flow_on_control_and_video_streams`, `host_drops_video_instead_of_queueing`,
  `viewer_skips_to_keyframe_and_requests_one_after_loss`,
  `correct_one_time_code_succeeds_and_rotates`, `wrong_code_fails_on_both_sides`,
  `password_auth_right_and_wrong`, `host_with_different_key_than_dialed_id_is_rejected`,
  `tofu_reports_host_key_for_known_hosts`, `peer_outside_allow_list_is_refused`,
  `repeated_wrong_codes_trigger_lockout`,
  `second_endpoint_on_a_taken_port_fails_with_addr_in_use`, `side_streams_of_every_kind_work_both_ways`,
  `side_stream_kinds_have_separate_queues`, `unknown_stream_kind_is_refused`, and the ignored
  benchmark `bulk_side_stream_throughput` (moves several GB over loopback; target ≥ 1 GB/s on one
  stream — run with
  `cargo test --release -p tlp-desk-net --test session -- --ignored --nocapture bulk_side_stream_throughput`).
- `tests/discovery.rs`: `broadcast_fallback_discovers_peer_and_expires_it`, and
  `mdns_advertise_and_browse` — **ignored** in CI because it needs a multicast-capable network
  adapter:

```powershell
cargo test -p tlp-desk-net
cargo test -p tlp-desk-net --test discovery -- --ignored   # needs real multicast
```
