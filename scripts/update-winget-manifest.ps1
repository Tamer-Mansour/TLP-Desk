# Regenerates the winget manifest set in installer\winget\ for a published GitHub release:
# downloads the release's TLP-Desk-<version>-setup.exe, computes its SHA-256 (cross-checked against
# the release's SHA256SUMS.txt and GitHub's asset digest), writes the version, installer URL, hash,
# release date and release-notes link into the four manifests, then runs `winget validate`.
#
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\update-winget-manifest.ps1 [-Tag v1.0.0]
#   -Tag           release tag (default: v<workspace version from Cargo.toml>)
#   -Repo          GitHub repository (default: Tamer-Mansour/TLP-Desk)
#   -ManifestDir   manifest folder (default: installer\winget)
#   -SkipValidate  do not run `winget validate`
#
# A maintainer script: it needs internet access to GitHub (GH_TOKEN/GITHUB_TOKEN is used when set,
# to avoid API rate limits). The app itself never goes online. Nothing is submitted anywhere; the
# submission steps are in docs\src\dev\code-signing.md ("winget").
[CmdletBinding()]
param(
    [string]$Tag,
    [string]$Repo = 'Tamer-Mansour/TLP-Desk',
    [string]$ManifestDir,
    [switch]$SkipValidate
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $ManifestDir) { $ManifestDir = Join-Path $root 'installer\winget' }
$id = 'TLP.TLP-Desk'
$utf8NoBom = New-Object System.Text.UTF8Encoding $false

if (-not $Tag) {
    $cargoToml = Get-Content (Join-Path $root 'Cargo.toml') -Raw
    if ($cargoToml -notmatch '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"') { throw 'workspace version not found' }
    $Tag = "v$($Matches[1])"
}
if ($Tag -notmatch '^v(\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?)$') { throw "unexpected tag '$Tag' (expected v<major>.<minor>.<patch>[-pre])" }
$version = $Matches[1]
$setupName = "TLP-Desk-$version-setup.exe"

# Replaces the value of each "key:" line (also "- key:" list items); every key must occur once.
function Update-Manifest([string]$File, [System.Collections.IDictionary]$Values) {
    if (-not (Test-Path -LiteralPath $File)) { throw "missing manifest $File" }
    $text = [IO.File]::ReadAllText($File, [Text.Encoding]::UTF8) -replace "`r`n", "`n"
    foreach ($key in $Values.Keys) {
        $pattern = "(?m)^(\s*(?:-\s+)?$([regex]::Escape($key)):)[ \t]*\S.*$"
        $found = [regex]::Matches($text, $pattern).Count
        if ($found -ne 1) { throw "$(Split-Path $File -Leaf): expected one '${key}:' line, found $found" }
        $text = [regex]::Replace($text, $pattern, ('${1} ' + ([string]$Values[$key]).Replace('$', '$$')))
    }
    [IO.File]::WriteAllText($File, $text, $utf8NoBom)
}

[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
$headers = @{ 'User-Agent' = 'TLP-Desk-update-winget-manifest'; 'Accept' = 'application/vnd.github+json' }
$token = if ($env:GH_TOKEN) { $env:GH_TOKEN } else { $env:GITHUB_TOKEN }
if ($token) { $headers['Authorization'] = "Bearer $token" }

Write-Host "==> GitHub release $Repo $Tag" -ForegroundColor Cyan
$release = Invoke-RestMethod -Headers $headers "https://api.github.com/repos/$Repo/releases/tags/$Tag"
if ($release.draft) { throw "$Tag is still a draft release: publish it first (winget needs a public, permanent URL)" }
$asset = @($release.assets | Where-Object { $_.name -eq $setupName }) | Select-Object -First 1
if (-not $asset) { throw "release $Tag has no asset $setupName" }
$url = "https://github.com/$Repo/releases/download/$Tag/$setupName"
$published = if ($release.published_at) { $release.published_at } else { $release.created_at }
$releaseDate = ([datetime]$published).ToUniversalTime().ToString('yyyy-MM-dd')

$work = Join-Path ([IO.Path]::GetTempPath()) ("tlp-winget-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $work | Out-Null
try {
    $file = Join-Path $work $setupName
    Write-Host "==> downloading $url" -ForegroundColor Cyan
    Invoke-WebRequest -UseBasicParsing -Headers @{ 'User-Agent' = $headers['User-Agent'] } $url -OutFile $file
    $size = (Get-Item -LiteralPath $file).Length
    if ($size -ne [long]$asset.size) { throw "download is $size bytes, GitHub says $($asset.size)" }
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $file).Hash.ToUpperInvariant()

    # Cross-check 1: the SHA256SUMS.txt published with the same release.
    $sumsAsset = @($release.assets | Where-Object { $_.name -eq 'SHA256SUMS.txt' }) | Select-Object -First 1
    if ($sumsAsset) {
        $sumsFile = Join-Path $work 'SHA256SUMS.txt'
        Invoke-WebRequest -UseBasicParsing -Headers @{ 'User-Agent' = $headers['User-Agent'] } $sumsAsset.browser_download_url -OutFile $sumsFile
        $line = @(Get-Content -LiteralPath $sumsFile | Where-Object { ($_ -split '\s+', 2)[-1].TrimStart('*') -eq $setupName }) | Select-Object -First 1
        if (-not $line) { throw "SHA256SUMS.txt of $Tag has no line for $setupName" }
        if (($line -split '\s+')[0].ToUpperInvariant() -ne $hash) { throw "SHA-256 of the download does not match SHA256SUMS.txt" }
        Write-Host '    matches SHA256SUMS.txt'
    } else {
        Write-Warning "release $Tag has no SHA256SUMS.txt to cross-check against"
    }
    # Cross-check 2: GitHub's own digest of the uploaded asset (newer API responses carry it).
    if ($asset.PSObject.Properties['digest'] -and $asset.digest) {
        if ($asset.digest -ne "sha256:$($hash.ToLowerInvariant())") { throw "SHA-256 of the download does not match GitHub's asset digest $($asset.digest)" }
        Write-Host '    matches the GitHub asset digest'
    }

    $sig = Get-AuthenticodeSignature -LiteralPath $file
    if ($sig.SignerCertificate) {
        Write-Host "    signed by $($sig.SignerCertificate.Subject) [$($sig.Status)]" -ForegroundColor Green
    } else {
        Write-Host '    the setup is not code-signed (winget installs it without the SmartScreen prompt)' -ForegroundColor Yellow
    }
} finally {
    Remove-Item -Recurse -Force -LiteralPath $work -ErrorAction SilentlyContinue
}

$notes = "https://github.com/$Repo/releases/tag/$Tag"
Update-Manifest (Join-Path $ManifestDir "$id.yaml") ([ordered]@{ PackageVersion = $version })
Update-Manifest (Join-Path $ManifestDir "$id.installer.yaml") ([ordered]@{
        PackageVersion  = $version
        ReleaseDate     = $releaseDate
        InstallerUrl    = $url
        InstallerSha256 = $hash
    })
foreach ($locale in @('en-US', 'ar-SA')) {
    Update-Manifest (Join-Path $ManifestDir "$id.locale.$locale.yaml") ([ordered]@{
            PackageVersion  = $version
            ReleaseNotesUrl = $notes
        })
}
Write-Host "Updated $ManifestDir for $version" -ForegroundColor Green
Write-Host "  InstallerUrl     $url"
Write-Host "  InstallerSha256  $hash"
Write-Host "  ReleaseDate      $releaseDate"

if ($SkipValidate) { return }
$winget = Get-Command winget.exe -ErrorAction SilentlyContinue
if (-not $winget) { Write-Warning 'winget not found: skipped `winget validate`'; return }
Write-Host '==> winget validate' -ForegroundColor Cyan
& $winget.Source validate --manifest $ManifestDir --disable-interactivity
if ($LASTEXITCODE -ne 0) { throw "winget validate failed ($LASTEXITCODE)" }
if ($release.prerelease) {
    Write-Warning ("$Tag is a pre-release. Read the pre-release note in docs/src/dev/code-signing.md " +
        '("winget") before submitting it to microsoft/winget-pkgs.')
}
Write-Host "Next: copy the four files to manifests/t/TLP/TLP-Desk/$version/ in your winget-pkgs fork (see the guide)." -ForegroundColor Green
