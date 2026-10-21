# tlp-desk-relay

**The relay server** (`tlp-desk-relay`, Linux x86_64 static binary): lets a viewer reach a
host that is not on its network through a server the user runs. It is a UDP forwarder: the
session it carries is the ordinary end-to-end QUIC/TLS session, so pinning, SPAKE2 and
permissions are untouched. Decision and alternatives: [ADR 0013](../../adr/0013-self-hosted-relay.md).

| | |
|---|---|
| Path | `crates/tlp-desk-relay` (binary `tlp-desk-relay`); the protocol and the server logic live in `tlp-desk-net::relay` so tests run in-process |
| Depends on | `net`, `clap` 4, `tokio`, `tracing-subscriber`, `anyhow` |
| Unsafe | `#![forbid(unsafe_code)]` |
| Ports | control UDP 47402 (`--listen`), sessions UDP 47410–47509 (`--ports`), one per live session |
| Auth | shared token (`--token` / `TLP_RELAY_TOKEN`), stretched with SHA-256; every control frame carries an HMAC-SHA256 and a timestamp (±120 s) |

## Protocol (`tlp_desk_net::relay`)

```text
host   -> relay:47402   Register{device_id}           every 15 s (keeps the NAT mapping)
relay  -> host          Registered
viewer -> relay:47402   Connect{device_id}
relay  -> viewer        Allocated{session, port} | Offline
relay  -> host          Incoming{session, port}
host   -> relay:port    Punch{session, Host}          from the QUIC socket (Endpoint::send_raw)
viewer -> relay:port    Punch{session, Viewer}        from the QUIC socket
relay  -> viewer        Ready{session}
viewer -> relay:port    QUIC Initial ... (dial_any with the device-ID pin); relay copies both ways
```

Frame: `b"TLPR" | 0x01 | postcard(Frame { ts, payload }) | HMAC-SHA256(token, everything
before)`; at most 256 bytes. QUIC packets never start with the magic, so a session port can
tell a late punch from traffic. A session port is freed 30 s after allocation if both sides
have not punched, and 60 s after the last packet otherwise.

## Limits

Registrations expire after 45 s; at most 10 000 registered devices; 20 `Connect`s per source
address per minute; forwarded datagrams capped at 2048 bytes; as many sessions as ports.

## Tests

`relay::tests` (frame encoding, token, timestamp window, address parsing) and
`crates/tlp-desk-net/tests/relay.rs` (relay + host + viewer on loopback: registration, the
pinned handshake through the session port, offline and wrong-token paths, the allow-list
exception limited to the relay address).

## Running it

See the user guide: [Relay server](../../user-guide/en/relay.md). `installer/linux/install-relay.sh`
installs it as a hardened systemd service (`DynamicUser`, read-only system).
