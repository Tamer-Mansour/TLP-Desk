# 0009: Rename the product to TLP-Desk, keep wire identifiers

- **Status:** Accepted
- **Date:** 2026-10-13
- **Area:** all user-visible surfaces

## Context

The project started as `tlp-desk`. The product name shown to users was standardised as
**TLP-Desk**. Renaming touches the window title, executables, installer, firewall rules and data
folders — and, if done carelessly, the network protocol and every existing device ID.

## Decision

Rename everything **users see**; keep everything **peers and existing installs depend on**.

Renamed:

- Window title and the i18n app name (English and Arabic both use the Latin "TLP-Desk").
- Executables `TLP-Desk.exe` and `TLP-Desk-cli.exe`, their version resources and CLI help.
- Installer script `TLP-Desk.iss`, output names `TLP-Desk-<version>-setup.exe` and
  `TLP-Desk-<version>-win-x64-portable.zip`.
- Firewall rule names `TLP-Desk (QUIC)`, `TLP-Desk (discovery)`, `TLP-Desk (mDNS)`; the installer
  deletes the old lowercase rules on upgrade.
- Data folders `%ProgramData%\TLP-Desk` and `%APPDATA%\TLP-Desk`.

Unchanged on purpose:

- Crate names (`tlp-desk-*`).
- The mDNS service type `_tlpdesk._udp.local.`, the broadcast magic `TLPD`, the ALPN `tlp-desk`,
  the TLS SNI and certificate names.
- Cryptographic labels (`tlp-desk/pake/v2`, `EXPORTER-tlp-desk-pake-v2`, `tlp-desk/pw/v2`, ...).

## Alternatives considered

- **Rename everything** — would change PAKE labels and the password salt (breaking every stored
  verifier), make old and new builds invisible to each other in discovery, and churn every crate
  path for no user benefit.
- **Keep the old name** — inconsistent branding.

## Consequences

- Old and new builds with the same protocol version still discover and talk to each other.
- Device IDs survive the rename: Windows paths are case-insensitive, so `%ProgramData%\tlp-desk`
  *is* `%ProgramData%\TLP-Desk`, and the existing identity key is reused.
- Contributors see two spellings: `tlp-desk` in code and on the wire, `TLP-Desk` in anything a user
  reads. New user-facing text uses TLP-Desk; new wire identifiers keep the `tlp-desk` prefix.

## References

Commit `85292e4` ("product name TLP-Desk everywhere users see it");
[ADR 0005](0005-postcard-framing.md) for why wire identifiers are frozen.
