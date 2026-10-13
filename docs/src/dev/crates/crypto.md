# tlp-desk-crypto

**Identity, device IDs, trust store and password-authenticated key exchange.** This crate holds
every security-sensitive primitive that is not TLS itself. It does no networking.

| | |
|---|---|
| Path | `crates/tlp-desk-crypto` |
| Depends on | `rcgen` (ring), `x509-parser`, `sha2`, `hmac`, `spake2`, `rand`, `argon2`, `tracing` |
| Used by | `net`, `host`, `viewer`, `app`, `cli` |
| Unsafe | `#![forbid(unsafe_code)]` — the Windows ACL step shells out to `icacls.exe` to stay safe |
| Threads | none; `OneTimeCode` is shared through `Arc<Mutex<_>>`, Argon2 is run off the async runtime by the caller |

## Identity (`identity.rs`)

- `Identity::load_or_generate(dir)` loads `identity.key.pem` (`KEY_FILE`, PKCS#8 PEM) or creates
  a new **ECDSA P-256** key and saves it atomically (`.tmp` + rename). P-256 rather than Ed25519
  because every TLS 1.3 stack supports it.
- Only the private key is stored. The self-signed X.509 certificate (SAN `tlp-desk`, CN
  `tlp-desk <device id>`) is regenerated from it on every start.
- `fingerprint()`, `device_id()`, `cert_der()`, `private_key_pkcs8_der()`.
- `Debug` never prints key material (test `debug_does_not_leak_key`).
- `fingerprint_of_cert(der)` — SHA-256 of a certificate's `SubjectPublicKeyInfo`.

## Device ID and fingerprint (`device_id.rs`)

- `Fingerprint([u8; 32])` = SHA-256 of the DER SPKI. `to_hex()` / `from_hex()`; `Display`
  shows colon-separated groups of four hex characters.
- `DeviceId(u32)`, always between `100_000_000` and `999_999_999`:

  ```text
  id = u64::from_be_bytes(fingerprint[0..8]) % 900_000_000 + 100_000_000
  ```

  `Display` is `123 456 789`; `FromStr` accepts `123456789`, `123 456 789` and `123-456-789`.

See [ADR 0003](../../adr/0003-ecdsa-identity-and-device-id.md) for why the ID is derived from the
key and what its 30-bit size means for security.

## Known hosts (`known_hosts.rs`)

The viewer's trust-on-first-use store, file `known_hosts` (`KNOWN_HOSTS_FILE`) next to the
identity key.

- Keys: `HostKey::Id(DeviceId)` (written `id:<number>`) or `HostKey::Addr(String)`
  (`addr:<address>`).
- One `<key> <64-hex fingerprint>` per line; `#` comments allowed; saved atomically.
- `check(key, fp)` returns `TrustCheck::Unknown`, `Trusted` or `Changed { previous }`.
- `remember`, `forget`, `fingerprint_for`, `iter`, `save`.

## One-time code (`code.rs`)

- `OneTimeCode::new()` draws 6 digits uniformly from `000000..=999999` using the OS CSPRNG.
- `current()` → `"042917"`, `display()` → `"042 917"`, `generation()` increments on each rotation.
- **Rotation:** after every successful use (`record_success`) and after
  `MAX_FAILURES_PER_CODE = 3` failures against the same code (`record_failure`). The second rule
  caps guesses per code even when an attacker spreads attempts over many IP addresses.
- Clones share state, so the UI and the network endpoint always agree on the current code.

## SPAKE2 (`pake.rs`, public module)

SPAKE2 over the Ed25519 group (`spake2` crate). Viewer is side A, host is side B.

- `PakeSession::start(role, secret, PakeContext) -> (PakeSession, outbound_msg)` and
  `finish(peer_msg) -> Result<PakeKeys, PakeError>`.
- `PakeContext { method, viewer_fp, host_fp, channel_binding }` — `method` is
  `PakeMethod::OneTimeCode` or `Password`; `channel_binding` is the 32-byte TLS exporter value.
- Identities: `tlp-desk/v2/viewer/<hex fp>` and `tlp-desk/v2/host/<hex fp>`.
- Transcript `T` = SHA-256 of length-prefixed fields: `tlp-desk/pake/v2`, method tag, both
  fingerprints, channel binding, both SPAKE2 messages.
- `PakeKeys::my_confirmation()` / `verify_peer(mac)` (constant time) implement key confirmation
  with `HMAC-SHA256(K, "tlp-desk/kc/viewer" || T)` and `... "tlp-desk/kc/host" ...`.
- `session_key()` derives `HMAC(K, "tlp-desk/session-key" || T)`; it is **not used** by the
  current transport (TLS already encrypts the session).
- Errors: `PakeError::BadMessage`, `PakeError::ConfirmationFailed`.

See [ADR 0002](../../adr/0002-spake2-with-channel-binding.md).

## Password verifier (`password.rs`)

`PasswordVerifier::derive(password, host_fp)` = Argon2id (v0x13, m = 19 MiB, t = 2, p = 1,
32-byte output) with salt `"tlp-desk/pw/v2" || host_fp`. The host stores only this verifier.
Because SPAKE2 is a *balanced* PAKE, the verifier is **password-equivalent for that host**:
anyone who steals it can authenticate to that host without knowing the password. The storage
ACL is therefore part of the security model (see [Security](../../security.md)). Unattended
access, which uses this, is planned for M3.

## Rate limiter (`ratelimit.rs`)

`RateLimiter<K = IpAddr>` with `RateLimitConfig { max_attempts: 5, window: 60 s, lockout: 600 s }`
by default. `attempt(&key, now)` uses a sliding window; the attempt that would exceed the limit
starts the lockout and returns `RateLimited { retry_after }`. `record_success` clears a peer's
history. The table is pruned above 10 000 entries. Time is passed in, so tests are
deterministic.

## Storage (`storage.rs`)

`default_dir()` returns the first writable candidate:

| OS | First choice | Fallback |
|---|---|---|
| Windows | `%ProgramData%\TLP-Desk` | `%LOCALAPPDATA%\TLP-Desk` |
| Other | `/var/lib/tlp-desk` | `$XDG_DATA_HOME/tlp-desk` or `~/.local/share/tlp-desk` |

Writability is checked with a probe file; an existing `identity.key.pem` must also be readable.
Windows paths are case-insensitive, so folders created by older builds named `tlp-desk` are
reused and the device ID survives the rename.

`harden_dir(dir)` runs, on Windows,
`icacls <dir> /inheritance:r /grant:r *S-1-5-18:(OI)(CI)F *S-1-5-32-544:(OI)(CI)F *S-1-3-4:(OI)(CI)F`
— full control for SYSTEM, Administrators and OWNER RIGHTS only — and `chmod 700` elsewhere.
Failure is logged as a warning, not fatal.

## Errors

`CryptoError`: `Io`, `Cert`, `BadCertificate`, `BadDeviceId`, `BadFingerprint`,
`KnownHostsFormat { line, reason }`, `PasswordHash`.

## Tests

All unit tests, none ignored:

- code: `code_is_six_digits`, `code_rotates_after_use`, `code_rotates_after_max_failures`
- device ID: `device_id_always_nine_digits`, `device_id_derivation_is_stable`,
  `device_id_display_and_parse`, `fingerprint_hex_roundtrip`
- identity: `cert_fingerprint_matches_identity`, `persisted_identity_is_stable`,
  `load_missing_is_none`, `debug_does_not_leak_key`
- known hosts: `known_hosts_tofu_and_change_detection`, `known_hosts_rejects_garbage`
- PAKE: `pake_same_code_confirms_both_ways`, `pake_wrong_code_fails_both_sides`,
  `pake_channel_binding_mismatch_fails`, `pake_garbage_message_is_error`
- password: `password_verifier_is_deterministic_and_host_bound`
- rate limit: `rate_limiter_locks_out_after_five_attempts_per_minute`,
  `rate_limiter_window_slides`, `rate_limiter_success_resets`
- storage: `default_dir_ends_with_app_name`, `writable_probe`,
  `hardened_dir_is_still_usable_by_creator`

```powershell
cargo test -p tlp-desk-crypto
```
