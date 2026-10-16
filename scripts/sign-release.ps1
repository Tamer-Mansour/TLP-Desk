# Signs dist\SHA256SUMS.txt -> dist\SHA256SUMS.txt.minisig with the release key, so the in-app
# updater accepts the release (it refuses any installer whose manifest signature or hash fails).
#
#   $env:TLP_RELEASE_SIGNING_KEY = 'C:\secure\tlp-release-key\release.key'   # path, or the base64 line
#   scripts\sign-release.ps1 [-Sums dist\SHA256SUMS.txt]
#
# Run it after installer\build-setup.ps1 and installer\portable.ps1 (they write SHA256SUMS.txt),
# and sign again whenever SHA256SUMS.txt changes. The signature is verified against the public
# key embedded in the app before the script reports success.
[CmdletBinding()]
param(
    [string]$Sums
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
if (-not $Sums) { $Sums = Join-Path $repo 'dist\SHA256SUMS.txt' }
if (-not (Test-Path $Sums)) { throw "Not found: $Sums" }
if (-not $env:TLP_RELEASE_SIGNING_KEY) { throw 'Set TLP_RELEASE_SIGNING_KEY (key file path or base64).' }
$Sums = (Resolve-Path $Sums).Path

$version = (Select-String -Path (Join-Path $repo 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
$pubLine = (Select-String -Path (Join-Path $repo 'crates\tlp-desk-app\src\update\pubkey.rs') `
    -Pattern 'RELEASE_PUBLIC_KEY: &str = "([^"]+)"').Matches[0].Groups[1].Value

Push-Location $repo
try {
    cargo run --quiet --release -p tlp-desk-app --example release_sign -- sign $Sums "TLP-Desk $version SHA256SUMS.txt"
    if ($LASTEXITCODE -ne 0) { throw "release_sign sign failed ($LASTEXITCODE)" }
    cargo run --quiet --release -p tlp-desk-app --example release_sign -- verify $Sums $pubLine
    if ($LASTEXITCODE -ne 0) {
        throw 'The signature does not verify with the public key embedded in the app (wrong key?).'
    }
} finally {
    Pop-Location
}
Write-Host "Signed: $Sums.minisig" -ForegroundColor Green
