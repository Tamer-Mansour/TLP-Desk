# 0008: Inno Setup `setup.exe` instead of an MSI

- **Status:** Accepted (MSI via cargo-wix stays optional)
- **Date:** 2026-10-13
- **Area:** `installer/`

## Context

Users need a normal Windows installer: Arabic and English wizard, Start-menu entry, Windows
Firewall rules limited to private networks, and a clean uninstall. The original plan
(ROADMAP M5) named an MSI built with cargo-wix. Building must also work offline.

## Decision

Ship a single **Inno Setup 6** `TLP-Desk-<version>-setup.exe` (`installer/TLP-Desk.iss`, built by
`installer/build-setup.ps1`), plus a portable zip. An MSI remains an optional later item.

- Wizard languages English and Arabic (Inno Setup ships an Arabic translation), preselected from the
  Windows UI language, with a language dialog.
- Firewall rules via `netsh` for the private and domain profiles only; removed on uninstall.
- A fixed `AppId` so upgrades replace earlier installs.
- Data folders kept on uninstall so the device ID survives a reinstall.

## Alternatives considered

- **MSI with WiX / cargo-wix** — the enterprise standard and good for Group Policy deployment, but
  localising the WiX UI into Arabic and authoring firewall rules is considerably more work, and
  WiX v4+ tooling needs .NET.
- **MSIX** — requires signing and a package identity; firewall rules and a future Windows service
  are harder.
- **Portable zip only** — no firewall rules (Windows prompts on first run), no Start-menu entry.

## Consequences

- One `.exe` that most users already know how to run; the Arabic wizard works out of the box.
- The installer is **not code-signed**, so SmartScreen shows "Windows protected your PC" until the
  file gains reputation; the user guide explains "More info → Run anyway".
- **An MSI is not planned.** Inno Setup covers what TLP-Desk needs (Arabic wizard, firewall rules for
  private networks only, the service, silent install switches), and a second installer would
  double the packaging, signing and test work for every release without adding a capability. The
  audience is home and small-office networks, not managed fleets. Administrators who deploy with
  Group Policy or Intune can wrap the silent `setup.exe` install (`/VERYSILENT /SUPPRESSMSGBOXES`)
  as a Win32 app or a startup script, or use the portable zip. If real demand appears, this
  decision can be revisited with a new ADR; nothing in the installer's layout prevents it.
- Silent installs use standard Inno Setup switches (`/VERYSILENT`, `/SUPPRESSMSGBOXES`, `/DIR=`,
  `/LANG=`).
- Unattended access (M3) will add a Windows service; the installer must then register and remove
  it as well.

## References

[Release process](../dev/release.md); user guide: [Installing](../user-guide/en/installing.md).
