# 0003: ECDSA P-256 identity key and a 9-digit device ID derived from it

- **Status:** Accepted
- **Date:** 2026-10-13
- **Area:** `tlp-desk-crypto` (`identity.rs`, `device_id.rs`)

## Context

Users need a short number they can read out loud or type ("connect to 123 456 789"), like other
remote-desktop tools. There is no server to assign or look up such numbers. And on a LAN anyone
can answer an mDNS query or a broadcast claiming to be any device, so a number that is merely
*announced* would let a neighbour impersonate a PC.

## Decision

- Each install generates a long-term **ECDSA P-256** key on first run (`identity.key.pem`,
  PKCS#8) in `%ProgramData%\TLP-Desk` (ACL: SYSTEM, Administrators, owner), with a per-user
  fallback when that is not writable. The self-signed TLS certificate is regenerated from the key
  on every start.
- **Fingerprint** = SHA-256 of the certificate's DER `SubjectPublicKeyInfo`.
- **Device ID** = `u64::from_be_bytes(fingerprint[0..8]) % 900_000_000 + 100_000_000` — always 9
  digits, shown as `123 456 789`.
- A viewer that dials by ID rejects any host whose key does not hash to that ID (see
  [ADR 0004](0004-tofu-and-fingerprint-pinning.md)).

P-256 rather than Ed25519 because every TLS 1.3 implementation, including `rustls` with the `ring`
provider and `rcgen`, supports ECDSA P-256 certificates.

## Alternatives considered

- **Random ID stored next to the key** — not tied to the key, so anyone could claim it.
- **Longer ID (12+ digits) or the full fingerprint** — much harder to read out and type; the full
  fingerprint is still used for TOFU and shown in the key-change warning.
- **Ed25519 keys** — smaller and faster, but not universally supported for TLS certificates.

## Consequences

- Connecting by ID is MITM-proof against anyone who has not found a key whose fingerprint maps to
  the same ID.
- The ID space is 9 × 10⁸ ≈ 2^29.7. An attacker who wants to impersonate *one specific* ID must
  generate on average about 900 million key pairs — feasible offline with enough CPU time, not
  instant. This is a known limitation, mitigated by fingerprint pinning after the first
  connection (ADR 0004) and documented in [Security](../security.md#known-limitations).
- Reinstalling Windows or deleting the key directory gives the PC a **new ID**; uninstalling
  TLP-Desk keeps the key, so a reinstall keeps the ID.
- Copying the key file to another PC clones its identity — which is why the directory is
  locked down.

Guarded by `device_id_always_nine_digits`, `device_id_derivation_is_stable`,
`persisted_identity_is_stable`, `cert_fingerprint_matches_identity`,
`host_with_different_key_than_dialed_id_is_rejected`.

## References

[tlp-desk-crypto](../dev/crates/crypto.md); [wire protocol](../protocol.md#transport).
