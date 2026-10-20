# Engineering decisions

Short records of choices made during the transformation, newest last. Bigger architectural
decisions also get an ADR in `docs/src/adr/`.

## D1: Self-connection is decided by key, never by address (P0)

Two PCs can share addresses (virtual adapters, reused DHCP leases), public IPs and host name
patterns; only the TLS key is unique per install. So:

* an address equal to one of this machine's own non-loopback interface addresses is never
  dialed (it can only reach us);
* if any address answers with our own key, that address is skipped
  (`NetError::SelfConnection`) and the host side closes it without a prompt;
* dialing our own device ID (or the service's ID shown on the home screen) fails at once with
  "That is this device".

Loopback addresses are kept: two installs on one machine (tests, a developer box) are
legitimately different devices, and the key check still catches a real self-dial.

## D2: Dial every address of a device, staggered, authorize only the winner (P0)

Happy-eyeballs style: start the best address, then one more every 250 ms (immediately when an
attempt fails), each bounded by the endpoint's handshake timeout (10 s default). The first QUIC
handshake whose key matches the pin wins; the others are dropped before any `Hello`, so the
host user never sees two prompts and rate limits are not charged twice. Authorization
(`Endpoint::authorize`) runs on the winner only.

Alternative rejected: keep the sequential loop with a short per-address timeout. With 3–4
announced addresses (Wi-Fi, Ethernet, virtual switches) that is still seconds per attempt, and
picking the timeout is a guess on slow Wi-Fi.

When no address works the reported error is, in order: a key mismatch (possible attack, or
another device at a stale address), else the last transport error, else "That is this device"
when only ourselves answered.

## D3: Addresses are per-source state, not an accumulating list (P0)

The latest mDNS resolution replaces the mDNS address set; broadcast sources are kept with a
last-heard time and expire individually after `expiry` (16 s). Ranking happens whenever either
source or the local interface list changes. A device with no dialable address is hidden rather
than shown as connectable.

## D4: Network changes are detected by polling the interface list every 2 s (P0)

`if_addrs` on a blocking thread, compared with the previous snapshot. Portable (Windows,
Linux, macOS, Android), no `unsafe`, cheap (one syscall). `NotifyIpInterfaceChange` would be
instant on Windows but needs a platform module and buys < 2 s. The QUIC endpoint is not rebound:
it binds the wildcard address, which keeps working across interface changes.

## D5: "Wrong device" beats "key changed" when the IDs differ (P0)

With a remembered fingerprint, a different key used to always mean "host key changed". When the
attempt was by device ID and the presented key hashes to a *different* ID, it is another
device at that address, not a changed key for this one: report `IdentityMismatch`. The
connection still fails closed; only the message (and the absence of a "forget key" button)
changes. Connections by address keep the loud TOFU warning.

## D6: Firewall profile stays Private/Domain (P0, needs maintainer decision to change)

A Wi-Fi classified as Public blocks inbound UDP 47400 with the installer's rules. Opening the
Public profile would make the PC reachable on hotel/airport networks; that is a security
trade-off for the maintainer, so it is documented in Troubleshooting instead.

## D7: Engineering notes live in `docs/engineering/` (process)

Requested by the maintainer for resumable hand-offs between sessions. They are not part of the
mdBook (contributor docs stay in `docs/src/dev/`). CLAUDE.md's layout table lists the folder.
