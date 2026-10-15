# 0004: Pin by device ID; trust on first use for addresses

- **Status:** Accepted
- **Date:** 2026-10-13
- **Area:** `tlp-desk-net` (`tls.rs`), `tlp-desk-viewer`, `tlp-desk-crypto` (`known_hosts.rs`)

## Context

There is no certificate authority on a LAN and no server to vouch for keys. The viewer still has
to know it reached the PC the user meant. Users connect three ways: by device ID, by device name
(resolved through discovery) and by IP address.

## Decision

The viewer verifies the host certificate itself (`PinnedServerVerifier`), with a pin chosen from
how the user dialed:

| Dialed | Pin | Result on mismatch |
|---|---|---|
| Device ID, or `ID@IP` | `Pin::DeviceId` — the key must hash to the ID | handshake aborted (`PinMismatch`) → "Identity check failed" |
| Address (or name) with a stored fingerprint | `Pin::Fingerprint` | `HostKeyChanged` → a loud warning dialog |
| Address seen for the first time | `Pin::Tofu` — accept, remember | — |

After every successful connection the fingerprint is stored in `known_hosts` under both the
address key (`addr:<ip>` or `addr:<ip:port>`) and the ID key (`id:<n>`). A changed key blocks the
connection until the user explicitly chooses "Forget old key and connect" (CLI:
`--forget-key`); nothing reaches the host user before that choice.

The host, in turn, requires a client certificate and checks that `Hello.device_id` matches it, so
the ID it shows in the Accept prompt is authentic.

## Alternatives considered

- **Always trust** — any device on the LAN could impersonate any other.
- **Always require the full fingerprint** — secure, but unusable for typing.
- **Silently update a changed key** — defeats the purpose; a key change is exactly what an
  attacker would cause.

## Consequences

- Dialing by ID is safe from the first connection (within the limits of the 9-digit ID, see
  [ADR 0003](0003-ecdsa-identity-and-device-id.md)); after the first connection the full
  fingerprint is pinned, closing the ID-collision gap for that host.
- Dialing by IP the first time is only as safe as the network at that moment.
- A legitimately reinstalled PC triggers the warning once; the user must decide. The dialog shows
  the remembered and presented key prefixes and tells the user to continue only if they know the
  PC was reinstalled.

Guarded by `host_with_different_key_than_dialed_id_is_rejected`,
`tofu_reports_host_key_for_known_hosts` (net), `tofu_remembers_key_and_detects_change`,
`wrong_device_id_is_rejected` (host e2e), `changed_host_key_is_loud_and_can_be_forgotten` (app),
`known_hosts_tofu_and_change_detection` (crypto).

## References

[Connection and authentication](../dev/architecture/auth.md); user guide:
[the host key changed warning](../user-guide/en/host-key-changed.md).
