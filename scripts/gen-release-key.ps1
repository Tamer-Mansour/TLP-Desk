# Generates the TLP-Desk release signing keypair ONCE (ed25519, minisign-compatible signatures).
#
#   scripts\gen-release-key.ps1 -OutDir C:\secure\tlp-release-key
#
# Writes <OutDir>\release.key (SECRET) and <OutDir>\release.pub, and prints the public key line
# to embed in crates/tlp-desk-app/src/update/pubkey.rs. Never put OutDir inside the repository.
# Store the secret as the GitHub Actions secret RELEASE_SIGNING_KEY (the file contents or just
# its base64 line) and keep an offline backup. See docs/src/dev/release.md.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$OutDir
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$full = [IO.Path]::GetFullPath($OutDir)
if ($full.StartsWith([IO.Path]::GetFullPath($repo), [StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing to write a secret key inside the repository: $full"
}
Push-Location $repo
try {
    cargo run --quiet --release -p tlp-desk-app --example release_sign -- keygen $full
    if ($LASTEXITCODE -ne 0) { throw "release_sign keygen failed ($LASTEXITCODE)" }
} finally {
    Pop-Location
}
