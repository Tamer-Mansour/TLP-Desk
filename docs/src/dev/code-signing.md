# Code signing, SmartScreen and winget

This page explains why Windows warns about the TLP-Desk downloads, what the repository already does
about it, and what a maintainer has to buy or set up to make the warning go away. Users who only
want to install TLP-Desk should read [Installing](../user-guide/en/installing.md) instead.

## Why Windows warns

Opening a file that came from the internet makes Windows check it with **Microsoft Defender
SmartScreen** (on Windows 11 PCs with Smart App Control turned on, an unsigned, unknown app can even
be blocked outright). SmartScreen does not scan
for viruses; Microsoft Defender Antivirus does that separately and finds nothing in TLP-Desk. SmartScreen
asks a different question: *does this file, or its publisher, have a good reputation?* A file with no
signature has no publisher, and a new file has no download history, so SmartScreen shows
**"Windows protected your PC"** with **Publisher: Unknown publisher**, and UAC shows the same.

Today none of the TLP-Desk files is signed: not `TLP-Desk.exe`, not `TLP-Desk-cli.exe`, not the
Inno Setup installer or its uninstaller. That is the whole cause of the warning.

What signing changes:

- An **Authenticode signature** names a verified publisher, proves the file was not modified, and
  lets reputation build up for that publisher instead of for every new file separately.
- Reputation is earned through downloads over time. A newly signed release can still warn for its
  first downloads; with the same certificate the warnings fade out and later releases start from
  the publisher's reputation. Microsoft does not publish thresholds.
- **EV certificates no longer skip this.** Since 2024 Microsoft treats EV and OV code-signing
  certificates the same for SmartScreen (the Microsoft Trusted Root Program removed the EV code
  signing OIDs). Older advice that "EV removes the warning instantly" no longer holds.
- Remote-control software is a favourite tool of scammers, so antivirus engines look at it more
  closely. A signature, a visible consent prompt and the on-screen banner (both of which TLP-Desk
  has) are what separate a legitimate tool from a remote-access trojan.

## What is already in the repository

Everything below works today without a certificate and switches itself on when one is configured.

| Piece | What it does |
|---|---|
| `installer/sign.ps1` | Signs files with SHA-256 and an RFC 3161 timestamp, then checks them with `signtool verify /pa /v`. Picks Azure Artifact Signing, a PFX file, or a certificate in the Windows store (also hardware tokens), in that order. Without any, it prints one English + Arabic line saying the build is unsigned. Never prints secrets. |
| `installer/build-setup.ps1`, `installer/portable.ps1` | Sign the staged `TLP-Desk.exe` and `TLP-Desk-cli.exe` before packaging, for the setup and the portable zip alike. |
| `installer/TLP-Desk.iss` | When signing is configured, `build-setup.ps1` passes `/DSignSetup` and the Inno Sign Tool `/Stlpsign=...`; the script then sets `SignTool=tlpsign $f` and `SignedUninstaller=yes`, so Inno signs **the setup and the uninstaller it embeds**. |
| `.github/workflows/release.yml` | Optional signing step from repository secrets (PFX) or Azure OIDC (Artifact Signing). Skipped when nothing is configured; when configured, a signing failure fails the release. |
| VERSIONINFO | `TLP-Desk.exe`, `TLP-Desk-cli.exe` and the setup carry CompanyName `TLP`, ProductName, FileDescription, LegalCopyright, OriginalFilename and versions; the setup's Apps & features entry has publisher, support and update links. |
| `installer/winget/` | A validated winget manifest set for `TLP.TLP-Desk`, kept up to date by `scripts/update-winget-manifest.ps1` (see [winget](#winget-install-without-the-warning)). |

## Choosing a certificate (the decision to make)

Prices are rough, in late 2026, and change often; check the vendor before buying. Since 1 March 2026
publicly trusted code-signing certificates are issued for at most 460 days, and since June 2023 the
private key of a new OV or EV certificate must live on a hardware token or in a cloud HSM, so a new
certificate normally does not come as a plain `.pfx` file any more.

| Option | Rough cost | Name users see | Works in CI | Notes |
|---|---|---|---|---|
| **Azure Artifact Signing** (formerly "Azure Trusted Signing") | Basic tier around US$10 per month (5,000 signatures) | Your validated legal name (person or company) | Yes, with OIDC; no secret to store | Needs a **paid** Azure subscription and Microsoft's identity validation. Individual developers are accepted only in some countries: check the country list in the Artifact Signing quickstart's prerequisites first. Microsoft-run, keys in an HSM, 3-day certificates renewed automatically (timestamping makes old signatures stay valid). |
| **Certum Open Source Code Signing** | Very roughly EUR 25-70 per year; the first year includes the smart card and reader | "Open Source Developer, *your name*" | No (physical card) | Cheapest certificate for a person. Sign on the PC that holds the card with `TLP_SIGN_THUMBPRINT`. Before buying, confirm the product is for Authenticode / Windows code signing and check the renewal terms. |
| **SignPath Foundation** | Free for qualifying open-source projects | "SignPath Foundation" | Only in CI | Requires automated builds in CI (GitHub Actions; currently blocked on this account by the billing lock), a manual approval per release, a code-signing policy page, MFA for maintainers. Remote-access tools get extra review. |
| **Standard (OV) certificate** from a commercial CA (Sectigo, DigiCert, GlobalSign, SSL.com, ...) | About US$200-500 per year | Your validated name | Only with the CA's cloud-HSM service (extra cost) | Token-based by default (`TLP_SIGN_THUMBPRINT`). |
| **EV certificate** | About US$300-700 per year | Your company name | Only with a cloud HSM | Companies only, stricter validation. No longer gives instant SmartScreen reputation (see above); worth it only when a customer demands EV. |

**Recommendation.** If you can pass Azure's identity validation, **Azure Artifact Signing** is the
cheapest certificate that also signs in CI with no secret to leak. If not, buy the **Certum Open
Source** certificate and sign releases locally on the PC with the card. SignPath Foundation is the
free route once GitHub Actions runs again. In every case, offer **winget** in the meantime.

## Signing a build locally

1. Install `signtool.exe`: the Windows SDK component *Windows SDK Signing Tools for Desktop Apps*
   (already present on PCs that build TLP-Desk). `sign.ps1` uses `TLP_SIGNTOOL`, then `PATH`, then the
   newest `Windows Kits\10\bin\<version>\x64\signtool.exe`.
2. Set the environment variables of **one** method in the PowerShell window that builds:

   ```powershell
   # a) Azure Artifact Signing (sign in first with `az login`, or set AZURE_* for a service principal)
   #    Client tools: winget install -e --id Microsoft.Azure.ArtifactSigningClientTools
   #    (or Install-Module ArtifactSigning -Scope CurrentUser; sign.ps1 falls back to it)
   $env:TLP_SIGN_ATS_ENDPOINT = 'https://weu.codesigning.azure.net'   # the region of your account
   $env:TLP_SIGN_ATS_ACCOUNT  = '<signing account name>'
   $env:TLP_SIGN_ATS_PROFILE  = '<certificate profile name>'
   # optional: $env:TLP_SIGN_ATS_DLIB = 'C:\path\to\bin\x64\Azure.CodeSigning.Dlib.dll'

   # b) A PFX file (avoid double quotes in the password)
   $env:TLP_SIGN_PFX = 'C:\secure\tlp-desk.pfx'
   $env:TLP_SIGN_PFX_PASSWORD = Read-Host 'PFX password'

   # c) A certificate in the Windows store, including a USB token or smart card (Certum, SafeNet, ...)
   $env:TLP_SIGN_THUMBPRINT = '<40 hex digits from certmgr.msc>'
   # $env:TLP_SIGN_CERT_STORE = 'machine'   # if it is in LocalMachine\My
   ```

   Optional for every method: `TLP_SIGN_TIMESTAMP_URL` (default DigiCert, then Sectigo; Microsoft's
   server for Artifact Signing) and `TLP_SIGN_REQUIRED=1` (fail instead of building unsigned).
3. Build as usual: `installer\build-setup.ps1` and `installer\portable.ps1`. The log shows
   `signing N file(s) with ...` and a `verified:` line per file; a token asks for its PIN.
4. Check a result: `signtool verify /pa /v dist\TLP-Desk-<version>-setup.exe`, or *Properties >
   Digital Signatures* in Explorer.

To sign any other file: `powershell -NoProfile -ExecutionPolicy Bypass -File installer\sign.ps1 <files>`;
`installer\sign.ps1 -Probe` prints which method is configured (`ats`, `pfx`, `thumbprint` or `none`).

## Signing in GitHub Actions

The release workflow signs when one of these is configured (*Settings > Secrets and variables >
Actions*); with both, Artifact Signing wins.

**Azure Artifact Signing (recommended, no stored secret):**

1. In Azure, create the Artifact Signing account, finish identity validation and create a
   *Public Trust* certificate profile.
2. Create an app registration in Microsoft Entra ID, add a **federated credential** for GitHub
   Actions (organization `Tamer-Mansour`, repository `TLP-Desk`, entity type *Tag* with the
   pattern of your release tags, or *Environment* if you use one).
3. Give that app the role **Artifact Signing Certificate Profile Signer** on the signing account.
4. Repository **secrets**: `AZURE_CLIENT_ID`, `AZURE_TENANT_ID` (and `AZURE_SUBSCRIPTION_ID` if you
   like). Repository **variables**: `ATS_ENDPOINT`, `ATS_ACCOUNT`, `ATS_PROFILE`.

The workflow signs in with `azure/login` (OIDC), downloads the signtool dlib
(`Microsoft.ArtifactSigning.Client` from nuget.org, version pinned in the workflow) and runs the same
`installer/sign.ps1` as a local build. It does not use `azure/artifact-signing-action` (the renamed
`azure/trusted-signing-action`) on purpose: that action signs finished files after the build, but
Inno Setup must be able to call a signing command *while* it compiles, or the uninstaller embedded
in `setup.exe` stays unsigned.

**PFX file** (only if your certificate exists as a `.pfx`):

```powershell
[Convert]::ToBase64String([IO.File]::ReadAllBytes('C:\secure\tlp-desk.pfx')) | Set-Clipboard
```

Paste it into the secret `SIGN_PFX_BASE64` and store the password in `SIGN_PFX_PASSWORD`. The
workflow writes the PFX to the runner's temp folder, signs, and deletes it.

A hardware token cannot be used on GitHub's runners; sign those releases locally and upload the
files by hand (see [Release process](release.md)).

## SignPath (documentation only)

With SignPath, signing happens in SignPath's service, not in this repository: the workflow uploads
the unsigned files as a build artifact, a signing request (for example with
`signpath/github-action-submit-signing-request`) is approved by a maintainer in SignPath, and the
signed files come back to the workflow, which attaches them to the release. Nothing in `sign.ps1`
needs to change; leave the `TLP_SIGN_*` variables unset in that workflow. Because SignPath signs
finished files, sign the exes in one request and the setup in a second one, and accept that the
embedded uninstaller stays unsigned (it is only started from the local disk, where SmartScreen does not
check it; UAC then shows "Unknown publisher"). To apply: <https://signpath.org/>.

## Testing the pipeline with a self-signed certificate

A self-signed certificate proves the plumbing, never trust. Windows does not trust it, so SmartScreen
still warns; never ship, commit or trust such a certificate.

```powershell
$tmp  = 'C:\tlp-build\signing-tmp'
$cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=TLP-Desk TEST signing (do not trust)' `
          -CertStoreLocation Cert:\CurrentUser\My -NotAfter (Get-Date).AddDays(2)
$pw   = Read-Host 'temporary password' -AsSecureString
Export-PfxCertificate -Cert $cert -FilePath "$tmp\test-sign.pfx" -Password $pw | Out-Null
Remove-Item "Cert:\CurrentUser\My\$($cert.Thumbprint)" -DeleteKey   # keep only the PFX

$env:TLP_SIGN_PFX = "$tmp\test-sign.pfx"
$env:TLP_SIGN_PFX_PASSWORD = [Net.NetworkCredential]::new('', $pw).Password
$env:TLP_SIGN_ALLOW_UNTRUSTED = '1'      # test certificates only; refused with TLP_SIGN_REQUIRED=1
$env:TLP_DESK_DIST_DIR = "$tmp\dist"     # keep the test build outside the repo
powershell -NoProfile -ExecutionPolicy Bypass -File installer\build-setup.ps1
signtool verify /pa /v "$tmp\dist\TLP-Desk-<version>-setup.exe"   # signature shown, root "not trusted"
Remove-Item "$tmp\test-sign.pfx"
```

`sign.ps1` still runs `signtool verify /pa` and prints its output; `TLP_SIGN_ALLOW_UNTRUSTED=1` only
turns the untrusted-root error into a warning after checking that a signature is present.

## winget: install without the warning

The Windows Package Manager downloads the installer itself, checks it against the SHA-256 in the
manifest (the catalog scanned that exact file when the manifest was accepted), and starts it without
the SmartScreen "Windows protected your PC" dialog.
The administrator prompt (UAC) still appears, and says "Unknown publisher" until the setup is signed.
This works only after TLP-Desk is accepted into the community catalog
([microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs)). Nothing has been submitted yet;
that is the maintainer's decision.

The manifest set lives in `installer/winget/` (schema 1.12.0): `TLP.TLP-Desk.yaml` (version),
`TLP.TLP-Desk.installer.yaml` (Inno, machine scope, x64, silent switches, the Inno uninstall key as
`ProductCode`), and the locales `en-US` (default) and `ar-SA`. Package identifier `TLP.TLP-Desk`,
publisher `TLP`, matching the installer's Apps & features entry.

**After every release** (once the GitHub release is published, not a draft):

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\update-winget-manifest.ps1 -Tag v1.0.0
```

The script downloads `TLP-Desk-<version>-setup.exe` from the release, checks its SHA-256 against the
release's `SHA256SUMS.txt` and GitHub's asset digest, updates version, URL, hash, date and release
notes link in the four files, and runs `winget validate --manifest installer\winget`. Commit the
result (`TLP-installer: winget manifest <version>`).

**Pre-releases.** winget's community catalog has no release channels: whatever version is listed
becomes "the latest TLP-Desk" for everyone, and winget's version ordering is not strictly semantic
versioning. The convention in winget-pkgs is to publish previews under a separate identifier (for
example `Microsoft.PowerShell.Preview`, `Microsoft.WindowsTerminal.Preview`). Recommended: make
**1.0.0 (stable)** the first `TLP.TLP-Desk` submission. If release candidates should be on winget
too, submit them as `TLP.TLP-Desk.Preview` (copy the files, change `PackageIdentifier` in all four
and the file names, keep everything else).

**Submitting (when you decide to):**

1. Run `scripts\update-winget-manifest.ps1 -Tag v<version>` and make sure `winget validate` passes.
2. Optional local test on a spare PC or Windows Sandbox (not your daily PC, it installs TLP-Desk):
   `winget settings --enable LocalManifestFiles` (admin, once), then
   `winget install --manifest installer\winget`.
3. Fork `microsoft/winget-pkgs`, create a branch, and copy the four files to
   `manifests/t/TLP/TLP-Desk/<version>/` (case-sensitive, one version per pull request).
4. Open the pull request against `microsoft/winget-pkgs` `master`, fill in its checklist and sign
   the Microsoft CLA when the bot asks.
5. Wait for the automated validation (download, hash, antivirus and PUA scan, silent install in a
   VM) and a moderator's review; answer any `Needs-Author-Feedback` comments.
6. After the merge, `winget search TLP-Desk` finds it within a few hours. Later versions can be
   submitted the same way, or with `wingetcreate update TLP.TLP-Desk --version <version> --urls
   <setup URL> --submit`.

## References

- Microsoft Learn: *SmartScreen reputation for Windows app developers*; *Artifact Signing*
  (quickstart, signing integrations, FAQ); *SignTool*.
- CA/Browser Forum Code Signing Baseline Requirements (ballot CSC-31, 460-day maximum validity).
- winget-pkgs: `doc/Authoring.md`, `doc/Validation.md`.
- [Release process](release.md), [ADR 0008](../adr/0008-inno-setup-instead-of-msi.md).
