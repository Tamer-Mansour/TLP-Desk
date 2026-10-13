# Security policy

## Reporting a vulnerability

Please **do not** open a public issue. Report privately through GitHub Security Advisories:
<https://github.com/Tamer-Mansour/TLP-Desk/security/advisories/new>.

Include what is affected (version or commit), how to reproduce it, and what an attacker gains. We
aim to acknowledge reports within 7 days and to publish a fix and an advisory once it is ready,
crediting you unless you prefer otherwise.

## Supported versions

Until 1.0, only the latest release and the `TLP-Desk` branch receive security fixes.

## Security model in short

TLP-Desk is built for trusted local networks and never contacts the internet.

- Every connection is QUIC with TLS 1.3 and mutual authentication. Each install has its own key
  pair; the 9-digit device ID is derived from the public key, so dialing by ID pins the key.
  Dialing by IP uses trust-on-first-use and warns loudly if the key changes.
- A session is authorized by the host user clicking **Accept**, by a rotating one-time code, or by
  an unattended-access password (Argon2 at rest). Codes and passwords use SPAKE2 and never cross
  the wire. Attempts are rate limited (5 per minute per IP, then a 10-minute lockout).
- Every session has a permission set the host can change live, and the host always shows who is
  connected.
- By default only private (RFC 1918), link-local and loopback peers may connect.

Details: [docs/protocol.md](docs/protocol.md) and the "Security model" section of
[CLAUDE.md](CLAUDE.md).

## Scope

In scope: authentication or pinning bypasses, permission bypasses (e.g. input, files or clipboard
without the permission), remote crashes or memory corruption from network input, key or secret
leakage, anything that makes TLP-Desk talk beyond the local network.

Out of scope: attacks that need administrator rights on the host already, physical access to an
unlocked PC, or a deliberately misconfigured allow-list that admits untrusted networks.
