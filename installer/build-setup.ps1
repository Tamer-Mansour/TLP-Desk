# Builds the single Windows installer:
#   dist\TLP-Desk-<version>-setup.exe   (everything in one file; Arabic + English wizard)
#   dist\SHA256SUMS.txt                 (line for the setup exe added/updated)
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File installer\build-setup.ps1
# Needs Inno Setup 6.6+ (free):
#   winget install --id JRSoftware.InnoSetup -e --accept-source-agreements --accept-package-agreements
# Works offline once Inno Setup and the cargo registry cache are present.
. (Join-Path $PSScriptRoot 'common.ps1')

# Fail the build (instead of warning) if build.rs cannot embed the exe icon/version info.
$env:TLP_DESK_REQUIRE_EXE_ICON = '1'
Invoke-ReleaseBuild
$version = Get-WorkspaceVersion

$iscc = @(
    (Get-Command ISCC.exe -ErrorAction SilentlyContinue | ForEach-Object Source),
    (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe'),
    (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
    (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe')
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $iscc) { throw 'ISCC.exe (Inno Setup 6) not found - install it with winget (see the header of this script)' }

$stage = Join-Path $Dist 'setup-stage'
New-PayloadStage -Stage $stage -ReadmeSource (Join-Path $PSScriptRoot 'README-setup.txt')

Write-Host "==> $iscc" -ForegroundColor Cyan
& $iscc /Q "/DMyAppVersion=$version" "/DStageDir=$stage" (Join-Path $PSScriptRoot 'TLP-Desk.iss')
if ($LASTEXITCODE -ne 0) { throw "ISCC failed ($LASTEXITCODE)" }
Remove-Item -Recurse -Force $stage

$setup = Join-Path $Dist "TLP-Desk-$version-setup.exe"
if (-not (Test-Path $setup)) { throw "expected $setup" }
$hash = Update-Sha256Sums $setup
$size = (Get-Item $setup).Length
Write-Host "Created $setup ($([math]::Round($size / 1MB, 2)) MB)" -ForegroundColor Green
Write-Host "SHA256  $hash" -ForegroundColor Green
Write-Host 'Not code-signed: SmartScreen may show "Windows protected your PC" -> More info -> Run anyway.' -ForegroundColor Yellow
