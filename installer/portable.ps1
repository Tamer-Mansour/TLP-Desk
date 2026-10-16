# Builds the portable package:
#   dist\TLP-Desk-<version>-win-x64-portable.zip   (TLP-Desk.exe, TLP-Desk-cli.exe, README.txt, licenses\)
#   dist\SHA256SUMS.txt                            (line for the zip added/updated)
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File installer\portable.ps1
# Works offline (uses only the local cargo registry cache). The exe icon is embedded by build.rs.
# Both exes are code-signed when installer\sign.ps1 finds a certificate (see its header).
. (Join-Path $PSScriptRoot 'common.ps1')

# Asked first so a half-configured certificate fails before the long cargo build.
$signMode = Get-SigningMode
Invoke-ReleaseBuild
$version = Get-WorkspaceVersion

$name = "TLP-Desk-$version-win-x64-portable"
$stage = Join-Path $Dist $name
New-PayloadStage -Stage $stage -ReadmeSource (Join-Path $PSScriptRoot 'README-portable.txt')

$zip = Join-Path $Dist "$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip -CompressionLevel Optimal
Remove-Item -Recurse -Force $stage

$hash = Update-Sha256Sums $zip
$size = (Get-Item $zip).Length
Write-Host "Created $zip ($([math]::Round($size / 1MB, 2)) MB)" -ForegroundColor Green
Write-Host "SHA256  $hash" -ForegroundColor Green
Write-Host "Sums:   $(Join-Path $Dist 'SHA256SUMS.txt')" -ForegroundColor Green
if ($signMode -eq 'none') {
    Write-Host 'Not code-signed: SmartScreen may warn the first time TLP-Desk.exe is started.' -ForegroundColor Yellow
}
