# 0013: A self-hosted relay for devices on other networks

- **Status:** Accepted
- **Date:** 2026-10-21
- **Area:** `tlp-desk-net` (`relay`), `tlp-desk-relay`, `tlp-desk-viewer`, `tlp-desk-app`,
  `tlp-desk-cli`

## Context

TLP-Desk is LAN-only by design (CLAUDE.md hard rule 2): no cloud, no vendor relay, nothing
leaves the local network on its own. Users nevertheless want to reach a device at another
home. The options are a VPN (works today, but is a second product to install and explain),
opening a port on the remote router (fails behind CGNAT, which most home and mobile lines
use, and exposes the port to the internet), or a relay. AnyDesk-style products run the relay
for you, which is exactly the dependency TLP-Desk refuses.

## Decision

TLP-Desk gains an **optional, self-hosted relay** (`tlp-desk-relay`, a small Linux binary the
user runs on a server they control: a VPS or a PC at home with one UDP port range forwarded).
Nothing changes for users who do not configure one.

- The relay is a **UDP forwarder, not a party to the session**. Viewer and host still run the
  ordinary QUIC/TLS 1.3 session with mutual authentication, device-ID pinning, SPAKE2 and the
  permission model; the relay copies encrypted packets between two addresses it learned from
  authenticated punches. It cannot read, inject or impersonate (the pin fails).
- A tiny **control protocol** (`tlp_desk_net::relay`): the host `Register`s every 15 s, the
  viewer `Connect`s, the relay `Allocate`s one UDP session port per session and tells the
  host (`Incoming`); both sides `Punch` the port from their QUIC socket, the relay tells the
  viewer `Ready`, the viewer dials `relay:port` with the device-ID pin. Every control message
  is `MAGIC | version | postcard | HMAC-SHA256(token)` with a 120 s timestamp window; frames
  with the wrong token are dropped silently, so strangers cannot use (or enumerate) a relay.
- **Opt-in on both sides.** The viewer uses the relay only when an ID is not found on the LAN
  and a relay is configured. The host registers only while "Let devices reach this PC through
  the relay" is on. The relay address is the one address outside the allow-list a viewer may
  dial, and the one address a host accepts connections from next to its allow-list
  (`Endpoint::set_relay_peer`).
- The **wire protocol is unchanged** (no `PROTOCOL_VERSION` bump): the relay control messages
  are a separate datagram format on separate ports.
- CLAUDE.md rule 2 is amended: the relay is the user's own server, named explicitly in the
  settings; it is not a vendor service and nothing contacts it unless configured.

## Alternatives considered

- **Vendor-run relay / rendezvous service** — the dependency and the trust the project
  exists to avoid; also a running cost. Rejected.
- **VPN only (document Tailscale / WireGuard)** — stays the recommended answer for people who
  already run one, but it is a second product with its own accounts and clients, and it was
  the one thing users asked not to need.
- **Router port forwarding only** — documented, but it does not work behind CGNAT and exposes
  the host port to the whole internet.
- **Tunnelling QUIC over a QUIC stream to the relay** (a custom `AsyncUdpSocket`) — would keep
  one socket, but adds head-of-line blocking and a second congestion controller under the
  session's own; raw UDP forwarding keeps the session's behaviour identical to the LAN case.
- **Signed registrations (host key signs the register)** instead of a shared token — stronger
  against a replayed `Register` (which can only misdirect notices, never a session), but needs
  the relay to know every device's key; left for a later version.

## Consequences

- Easier: a device at another home is reachable by its ID with one address and one token
  typed on each side; no router change at the remote end; works behind CGNAT.
- Harder: one more binary to ship (`tlp-desk-relay`, Linux x86_64 static), one more port
  range to document, and relayed sessions show the relay's address as the peer address (the
  rate limiter and connection log see the relay's IP for every relayed viewer).
- Must stay true (tests): `relay::tests` (frames, token, timestamps), `tests/relay.rs`
  (host registers, viewer reaches it only through the relay, offline and wrong-token paths,
  allow-list bypass limited to the relay's address); the viewer never dials a non-LAN address
  other than the relay session port it was given.
- Revisit if: users want a hosted option (that is a product decision, not a technical one),
  or if replayed registrations are observed in the wild (then sign them).

## References

`crates/tlp-desk-net/src/relay/{mod,server}.rs`, `crates/tlp-desk-relay/src/main.rs`,
`crates/tlp-desk-viewer/src/lib.rs` (`via_relay`), `crates/tlp-desk-app/src/settings/relay.rs`,
`crates/tlp-desk-app/src/ui/settings/relay.rs`, `installer/linux/install-relay.sh`,
`docs/src/user-guide/en/relay.md`, `docs/protocol.md` ("Relay").
