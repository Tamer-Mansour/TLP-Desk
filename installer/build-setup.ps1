# Builds the single Windows installer:
#   dist\TLP-Desk-<version>-setup.exe   (everything in one file; Arabic + English wizard;
#                                        exes, setup and uninstaller signed if configured)
#   dist\SHA256SUMS.txt                 (line for the setup exe added/updated)
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File installer\build-setup.ps1
# Needs Inno Setup 6.6+ (free):
#   winget install --id JRSoftware.InnoSetup -e --accept-source-agreements --accept-package-agreements
# Works offline once Inno Setup and the cargo registry cache are present.
. (Join-Path $PSScriptRoot 'common.ps1')

# Code signing (optional): see installer\sign.ps1 and docs\src\dev\code-signing.md. Without a
# certificate the build still works and prints that the setup is unsigned.

# Fail the build (instead of warning) if build.rs cannot embed the exe icon/version info.
$env:TLP_DESK_REQUIRE_EXE_ICON = '1'
# Asked first so a half-configured certificate fails before the long cargo build.
$signMode = Get-SigningMode
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
# Inno's VersionInfoVersion must be numeric a.b.c.d: "1.0.0-rc.1" -> "1.0.0.1", "1.0.0" -> "1.0.0.0".
if ($version -notmatch '^(\d+)\.(\d+)\.(\d+)(?:-[A-Za-z]+\.(\d+))?') { throw "unsupported version $version" }
$numeric = "$($Matches[1]).$($Matches[2]).$($Matches[3]).$(if ($Matches[4]) { $Matches[4] } else { '0' })"
$isccArgs = @('/Q', "/O$Dist", "/DMyAppVersion=$version", "/DMyAppNumericVersion=$numeric", "/DStageDir=$stage")
if ($signMode -ne 'none') {
    # Defines the "tlpsign" Sign Tool that TLP-Desk.iss uses (SignTool=tlpsign $f) to sign the
    # setup and its uninstaller. Inno replaces $q with a quote and $p with "$f" (the quoted file).
    # The certificate settings reach sign.ps1 through the inherited environment, never this line.
    $shell = (Get-Process -Id $PID).Path
    $isccArgs += '/DSignSetup'
    $isccArgs += "/Stlpsign=`$q$shell`$q -NoProfile -ExecutionPolicy Bypass -File `$q$SignScript`$q `$p"
}
& $iscc @isccArgs (Join-Path $PSScriptRoot 'TLP-Desk.iss')
if ($LASTEXITCODE -ne 0) { throw "ISCC failed ($LASTEXITCODE)" }
Remove-Item -Recurse -Force $stage

$setup = Join-Path $Dist "TLP-Desk-$version-setup.exe"
if (-not (Test-Path $setup)) { throw "expected $setup" }
$hash = Update-Sha256Sums $setup
$size = (Get-Item $setup).Length
Write-Host "Created $setup ($([math]::Round($size / 1MB, 2)) MB)" -ForegroundColor Green
Write-Host "SHA256  $hash" -ForegroundColor Green
if ($signMode -eq 'none') {
    Write-Host 'Not code-signed: SmartScreen may show "Windows protected your PC" -> More info -> Run anyway.' -ForegroundColor Yellow
} else {
    $sig = Get-AuthenticodeSignature -LiteralPath $setup
    if (-not $sig.SignerCertificate) { throw "$setup has no signature although signing is configured ($signMode)" }
    Write-Host "Signed  $($sig.SignerCertificate.Subject) [$($sig.Status)]" -ForegroundColor Green
}
