# Security

TLP-Desk gives another person control of your computer, so its security model is strict and
written down. This page is for users who want to understand what is protected, and for reviewers.
The wire-level details are in the [wire protocol](protocol.md); the reasoning behind each choice is
in the [decision records](adr/index.md).

> To report a vulnerability, please do not open a public issue; report it privately through
> [GitHub Security Advisories](https://github.com/Tamer-Mansour/TLP-Desk/security/advisories/new).

## What TLP-Desk promises

1. **LAN only.** It never contacts the internet: no cloud, relay, account, update check, analytics
   or crash upload. It works with the internet unplugged.
2. **Everything is encrypted.** Every connection is QUIC with TLS 1.3. There is no unencrypted
   mode and no "debug mode" that skips authentication in release builds.
3. **Nobody connects without permission.** Each session is authorized by the person at the host
   (Accept) or by a secret they shared (the one-time code; later, an unattended-access password).
4. **You know who you reached.** A viewer verifies the host's identity key; a host sees the
   viewer's authenticated device ID.
5. **The host stays in charge.** A banner on the host always shows who is connected, with a button
   to end the session; the local user's mouse and keyboard always take priority.

## Threat model

| Attacker | Can they… | Why not / mitigations |
|---|---|---|
| Someone on the internet | Reach TLP-Desk at all? | No listener is meant for the internet: the host only accepts private, link-local and loopback addresses by default, and the installer opens the firewall for Private/Domain networks only. |
| Another device on your LAN, passively listening | Read the screen, keystrokes, codes? | All traffic is TLS 1.3. Codes and passwords never cross the wire (SPAKE2). |
| Another device on your LAN, actively interfering (man in the middle) | Pretend to be the host you dialed by ID? | The host's key must hash to the ID you typed. After the first connection, its full fingerprint is pinned. |
| | Pretend to be the host you dialed by IP the first time? | Only if it is already in the middle on that very first connection (trust on first use). Any later key change triggers a loud warning. |
| | Relay the one-time code exchange between you and the real host? | No: SPAKE2 is bound to each TLS connection (exporter-based channel binding), so a relay fails key confirmation. |
| | Guess the 6-digit code? | 5 attempts per minute per IP, then a 10-minute lockout; the code also changes after 3 wrong guesses and after every use. Each guess needs a live exchange with the host. |
| | Flood the host with Accept prompts? | 5 prompts per minute per IP, then a lockout; at most 8 prompts open at once; unanswered prompts are declined after 60 s. |
| A viewer you accepted | Do more than you allowed? | Permissions are enforced on the host: the GUI grants view + mouse + keyboard; clipboard, files, audio, restart and other permissions are separate bits. The host can revoke permissions live. |
| | Keep control while you use your PC? | No: local input has priority; remote input pauses while you use your mouse or keyboard. |
| Malware already running on either PC | Anything | Out of scope. A compromised endpoint can read the screen and inject input on its own. |

## Identity and pinning

- On first run each install creates an **ECDSA P-256** key, stored as `identity.key.pem` in
  `%ProgramData%\TLP-Desk` (or `%LOCALAPPDATA%\TLP-Desk` when ProgramData is not writable). The
  folder's permissions are reduced to SYSTEM, Administrators and the owner.
- The **device ID** is derived from the key's SHA-256 fingerprint, so a device cannot simply claim
  an ID.
- The viewer keeps a `known_hosts` file with the fingerprint of every host it connected to, and
  refuses a changed key until the user explicitly forgets the old one.

See [ADR 0003](adr/0003-ecdsa-identity-and-device-id.md) and
[ADR 0004](adr/0004-tofu-and-fingerprint-pinning.md).

## Authentication methods

| Method | Who decides | Secret on the wire? | Status |
|---|---|---|---|
| **Accept prompt** | The person at the host clicks Accept or Decline. The prompt shows the viewer's authenticated ID, its name and address. | None | Available |
| **One-time code** | Whoever knows the 6-digit code shown on the host. Changes after each use. | No — SPAKE2 | Available |
| **Unattended password** | Whoever knows the permanent password. Stored only as an Argon2id verifier. | No — SPAKE2 | Protocol and crypto ready; no UI yet (M3) |

## Network allow-list

By default a host accepts connections only from `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`,
`169.254.0.0/16`, `127.0.0.0/8`, `fc00::/7`, `fe80::/10` and `::1`. Other addresses are refused
before the TLS handshake. The viewer applies the same list before sending anything, so typing a
public address fails immediately. A settings screen for custom ranges is planned (M2).

## What is encrypted

Everything in the session: video, mouse and keyboard, control messages, and (when they arrive)
clipboard, files, chat and audio — all inside one TLS 1.3 connection.

Not encrypted, because it must be readable before a connection exists: **discovery
announcements** (mDNS and UDP broadcast). They contain the device ID, the device name, the port
and the protocol version — visible to anyone on the LAN. Do not put sensitive information in your
device name.

## Data stored on disk

| File | Location | Contents |
|---|---|---|
| `identity.key.pem` | `%ProgramData%\TLP-Desk\` | Private identity key — treat like a password |
| `known_hosts` | same folder | Fingerprints of hosts this PC connected to |
| `settings.json` | `%APPDATA%\TLP-Desk\` | Language, digit style, device name |

No log files are written at the moment. Planned logging is local only and must never contain
codes, passwords, keys, clipboard or file contents.

Uninstalling keeps the identity and settings folders so your device ID stays the same; delete
`%ProgramData%\TLP-Desk` after uninstalling to reset it.

## Known limitations

- **9-digit IDs are short.** An ID has about 30 bits. An attacker who wants to impersonate one
  particular device by ID must generate, on average, around 900 million key pairs until one maps
  to that ID — expensive, but within reach of a determined attacker with time. Mitigation: after
  the first successful connection the viewer pins the host's **full** fingerprint, which a
  colliding key cannot match, and warns loudly if it changes. For the strongest assurance on a
  first connection, compare the fingerprint shown by `TLP-Desk-cli id` on the host.
- **Trust on first use by IP.** The very first connection to an IP address accepts whatever key
  answers. Prefer connecting by ID.
- **Password verifier is password-equivalent.** SPAKE2 is a balanced PAKE: the stored Argon2id
  verifier is enough to authenticate to that host. It is protected by the folder ACL; an
  augmented PAKE may replace it when unattended access ships.
- **Device names are self-reported.** The ID shown in the Accept prompt is authenticated; the
  name next to it is whatever the other PC calls itself.
- **Discovery is unauthenticated.** A device can announce any name; the ID check at connect time
  is what protects you.
- **Elevated windows and the secure desktop** (UAC prompts, lock screen) cannot be controlled by
  the current, non-service app; the M3 Windows service will address this.
- **Unsigned installer.** Releases are not code-signed yet. Verify downloads against
  `SHA256SUMS.txt`.

## For contributors

Security rules every change must keep (from CLAUDE.md and the code style guide):

- no network access beyond the LAN, ever;
- no code path that disables authentication in release builds;
- every byte from a peer is hostile — bounded allocations, checked arithmetic, no panics;
- never log secrets;
- any protocol change bumps `PROTOCOL_VERSION` and updates `docs/protocol.md`.
