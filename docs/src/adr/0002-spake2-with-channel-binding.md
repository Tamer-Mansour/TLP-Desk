# 0002: SPAKE2 with TLS channel binding for codes and passwords

- **Status:** Accepted (key confirmation added in protocol v2)
- **Date:** 2026-10-13
- **Area:** `tlp-desk-crypto` (`pake.rs`), `tlp-desk-net` (`endpoint.rs`)

## Context

A viewer may authorize itself with the host's 6-digit one-time code, or later with a permanent
unattended-access password. CLAUDE.md requires that **the password never crosses the wire**.
A 6-digit code has only a million values, so anything that lets an observer or an active attacker
test guesses offline would break it immediately. The connection is already TLS 1.3, but TLS alone
does not prove the *peer* knows the code, and a viewer dialing by IP address uses trust on first
use, so it cannot fully rely on the host certificate.

## Decision

Use **SPAKE2** (`spake2` crate, Ed25519 group; viewer = side A, host = side B), bound to the TLS
session:

- Secret: the code's 6 ASCII digits, or for passwords a 32-byte Argon2id verifier (m = 19 MiB,
  t = 2, p = 1, salt `"tlp-desk/pw/v2" || host_fp`).
- Identities include both TLS fingerprints: `tlp-desk/v2/viewer/<hex>` and
  `tlp-desk/v2/host/<hex>`.
- **Channel binding:** 32 bytes of TLS exporter output (label `EXPORTER-tlp-desk-pake-v2`) go into
  the transcript.
- **Mutual key confirmation:** viewer and host exchange `HMAC-SHA256(K, "tlp-desk/kc/<role>" || T)`
  over the full transcript. The host sends its MAC only after verifying the viewer's; the viewer
  aborts if the host's MAC is wrong.
- Online guessing is limited: 5 attempts per minute per IP then a 10-minute lockout, and the code
  rotates after 3 failures and after each success.

## Alternatives considered

- **Send the code inside TLS** — the host would learn it (fine), but a man-in-the-middle with a
  TOFU-accepted certificate would too, and the viewer would get no proof that the host knew it.
- **HMAC challenge-response keyed by the code** — a single observed exchange allows an offline
  brute force of all 10⁶ codes.
- **SRP / OPAQUE (augmented PAKEs)** — stronger for stored passwords (the server keeps a
  verifier that is not password-equivalent) but more complex, with fewer mature Rust options at
  the time. SPAKE2 is balanced, so the stored verifier *is* password-equivalent for that host; the
  key directory's ACL is what protects it. Revisit when unattended access lands (M3).

## Consequences

- A wrong code fails cleanly on both sides, and each failure costs the attacker one online attempt.
- A MITM terminating TLS separately with each side fails, because the fingerprints and exporter
  values in the two transcripts differ.
- The viewer learns that the host knew the code — a fake host cannot "accept any code".
- `PakeKeys::session_key()` is derived but unused; TLS already encrypts the session.

Guarded by `pake_same_code_confirms_both_ways`, `pake_wrong_code_fails_both_sides`,
`pake_channel_binding_mismatch_fails` (crypto), `wrong_code_fails_on_both_sides`,
`repeated_wrong_codes_trigger_lockout`, `password_auth_right_and_wrong` (net).

## References

[Connection and authentication](../dev/architecture/auth.md); [Security](../security.md);
[wire protocol](../protocol.md#authentication).
