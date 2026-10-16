<#
.SYNOPSIS
Authenticode-signs TLP-Desk release files (SHA-256 + RFC 3161 timestamp), or says clearly that the
build stays unsigned.

.DESCRIPTION
Shared by installer\build-setup.ps1 and installer\portable.ps1 (TLP-Desk.exe, TLP-Desk-cli.exe),
Inno Setup's SignTool callback (setup.exe and its uninstaller) and .github\workflows\release.yml.
Guide: docs\src\dev\code-signing.md.

The signing method comes from environment variables; the first complete match wins:

  1. Azure Artifact Signing (formerly "Azure Trusted Signing")
       TLP_SIGN_ATS_ENDPOINT   regional endpoint, e.g. https://weu.codesigning.azure.net
       TLP_SIGN_ATS_ACCOUNT    signing account name
       TLP_SIGN_ATS_PROFILE    certificate profile name
       TLP_SIGN_ATS_DLIB       optional: path to Azure.CodeSigning.Dlib.dll (x64). Default: the
                               Artifact Signing Client Tools install folder, else the
                               ArtifactSigning PowerShell module (Invoke-ArtifactSigning).
       TLP_SIGN_ATS_EXCLUDE_CREDENTIALS  optional, comma separated DefaultAzureCredential types to skip
     Azure sign-in comes from DefaultAzureCredential (az login, azure/login in CI, AZURE_* env).
  2. A PFX file
       TLP_SIGN_PFX            path to the .pfx / .p12
       TLP_SIGN_PFX_PASSWORD   its password (may be empty)
  3. A certificate already in the Windows certificate store (also hardware tokens/smart cards)
       TLP_SIGN_THUMBPRINT     SHA-1 thumbprint of the code-signing certificate
       TLP_SIGN_CERT_STORE     optional: "machine" for LocalMachine\My (default CurrentUser\My)
  4. none: one bilingual warning line; the files stay unsigned and SmartScreen will warn.

SignPath signs in its own service after the build (see the guide); nothing to configure here.

Other settings:
  TLP_SIGN_TIMESTAMP_URL   RFC 3161 server(s), comma separated, tried in order. Default:
                           DigiCert then Sectigo (Artifact Signing: Microsoft's server).
                           "none" signs without a timestamp (offline tests only).
  TLP_SIGN_REQUIRED=1      fail instead of producing an unsigned build (release CI sets it).
  TLP_SIGN_ALLOW_UNTRUSTED=1  accept a signature whose chain Windows does not trust; only for a
                           self-signed test certificate, never together with TLP_SIGN_REQUIRED.
  TLP_SIGNTOOL             signtool.exe to use (default: PATH, then the newest Windows SDK).
  TLP_SIGN_DEBUG=1         pass /debug to signtool.

Every signed file is checked with `signtool verify /pa /v`. Secrets (the PFX password, Azure
credentials) are never printed.

.PARAMETER Path
Files to sign in place.

.PARAMETER Probe
Print the configured method (ats, pfx, thumbprint or none) and exit without signing.

.EXAMPLE
powershell -NoProfile -ExecutionPolicy Bypass -File installer\sign.ps1 dist\TLP-Desk-1.0.0-setup.exe

.EXAMPLE
powershell -NoProfile -ExecutionPolicy Bypass -File installer\sign.ps1 -Probe
#>
[CmdletBinding()]
param(
    [switch]$Probe,
    [string]$Description = 'TLP-Desk',
    [string]$DescriptionUrl = 'https://github.com/Tamer-Mansour/TLP-Desk',
    [Parameter(Position = 0, ValueFromRemainingArguments = $true)]
    [string[]]$Path
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0

$AtsVars = @('TLP_SIGN_ATS_ENDPOINT', 'TLP_SIGN_ATS_ACCOUNT', 'TLP_SIGN_ATS_PROFILE')
$Guide = 'docs/src/dev/code-signing.md'

function Get-EnvValue([string]$Name) {
    $v = [Environment]::GetEnvironmentVariable($Name)
    if ($null -eq $v) { return '' }
    return $v.Trim()
}

function Test-EnvFlag([string]$Name) { return (Get-EnvValue $Name) -eq '1' }

# ats | pfx | thumbprint | none. A half-configured method is an error, never a silent fallback.
function Get-SigningMode {
    $set = @($AtsVars | Where-Object { Get-EnvValue $_ })
    if ($set.Count -eq $AtsVars.Count) { return 'ats' }
    if ($set.Count -gt 0) {
        $missing = @($AtsVars | Where-Object { -not (Get-EnvValue $_) }) -join ', '
        throw "incomplete Azure Artifact Signing configuration: $missing not set (see $Guide)"
    }
    if (Get-EnvValue 'TLP_SIGN_PFX') { return 'pfx' }
    if (Get-EnvValue 'TLP_SIGN_THUMBPRINT') { return 'thumbprint' }
    return 'none'
}

function Find-SignTool {
    $explicit = Get-EnvValue 'TLP_SIGNTOOL'
    if ($explicit) {
        if (Test-Path -LiteralPath $explicit -PathType Leaf) { return $explicit }
        throw "TLP_SIGNTOOL points to a missing file: $explicit"
    }
    $onPath = Get-Command signtool.exe -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($onPath) { return $onPath.Source }
    # x64 signtool also matches the x64 Artifact Signing dlib.
    $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    if (Test-Path -LiteralPath $kits) {
        $best = Get-ChildItem -LiteralPath $kits -Directory |
            Where-Object { $_.Name -match '^\d+(\.\d+){3}$' } |
            Sort-Object { [version]$_.Name } -Descending |
            ForEach-Object { Join-Path $_.FullName 'x64\signtool.exe' } |
            Where-Object { Test-Path -LiteralPath $_ } |
            Select-Object -First 1
        if ($best) { return $best }
    }
    throw 'signtool.exe not found: install the Windows SDK (component "Windows SDK Signing Tools for Desktop Apps") or set TLP_SIGNTOOL'
}

function Find-AtsDlib {
    $explicit = Get-EnvValue 'TLP_SIGN_ATS_DLIB'
    if ($explicit) {
        if (Test-Path -LiteralPath $explicit -PathType Leaf) { return $explicit }
        throw "TLP_SIGN_ATS_DLIB points to a missing file: $explicit"
    }
    # Per-user install folders of the client tools MSI (renamed with the service in 2025).
    $roots = @(
        (Join-Path $env:LOCALAPPDATA 'Microsoft\MicrosoftArtifactSigningClientTools'),
        (Join-Path $env:LOCALAPPDATA 'Microsoft\MicrosoftTrustedSigningClientTools')
    )
    foreach ($root in $roots) {
        if (-not (Test-Path -LiteralPath $root)) { continue }
        $dll = Get-ChildItem -LiteralPath $root -Recurse -Filter 'Azure.CodeSigning.Dlib.dll' -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -notmatch '\\(x86|arm64)\\' } |
            Select-Object -First 1
        if ($dll) { return $dll.FullName }
    }
    return ''
}

function Get-TimestampUrls([string]$Mode) {
    $configured = Get-EnvValue 'TLP_SIGN_TIMESTAMP_URL'
    if ($configured) { return @($configured -split '[,;\s]+' | Where-Object { $_ }) }
    if ($Mode -eq 'ats') { return @('http://timestamp.acs.microsoft.com') }
    return @('http://timestamp.digicert.com', 'http://timestamp.sectigo.com')
}

# Runs a native tool and returns its exit code and output lines; never throws on stderr (PS 5.1).
function Invoke-Native([string]$Exe, [string[]]$Arguments) {
    $eap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $lines = @(& $Exe @Arguments 2>&1 | ForEach-Object { "$_" })
        $code = $LASTEXITCODE
    } finally { $ErrorActionPreference = $eap }
    return @{ Code = $code; Output = $lines }
}

function Write-Indented([string[]]$Lines, [string]$Secret) {
    foreach ($line in $Lines) {
        if ($Secret) { $line = $line.Replace($Secret, '***') }
        if ($line.Trim()) { Write-Host "    $line" }
    }
}

function Write-UnsignedNotice([int]$Count) {
    Write-Host ("UNSIGNED BUILD ($Count file(s)): no code-signing certificate is configured, so Windows SmartScreen " +
        "will warn when the download is opened. See $Guide.") -ForegroundColor Yellow
    Write-Host 'بناء غير موقَّع: لم تُضبط شهادة توقيع للشيفرة، لذا سيعرض SmartScreen في Windows تحذيرًا عند فتح الملف المنزَّل.' -ForegroundColor Yellow
}

# Checks that a signature is present and that Windows' default Authenticode policy accepts it.
function Test-Signature([string]$SignTool, [string]$File) {
    $leaf = Split-Path $File -Leaf
    $sig = Get-AuthenticodeSignature -LiteralPath $File
    if (-not $sig.SignerCertificate) { throw "no Authenticode signature on $File after signing" }
    $stamp = if ($sig.TimeStamperCertificate) { "timestamped by $($sig.TimeStamperCertificate.Subject)" } else { 'NOT timestamped' }
    $verify = Invoke-Native $SignTool @('verify', '/pa', '/v', $File)
    if ($verify.Code -eq 0) {
        Write-Host "    verified: $leaf, signer $($sig.SignerCertificate.Subject), $stamp" -ForegroundColor Green
        return
    }
    if (Test-EnvFlag 'TLP_SIGN_ALLOW_UNTRUSTED') {
        Write-Indented $verify.Output ''
        Write-Warning ("$leaf is signed by $($sig.SignerCertificate.Subject) ($stamp) but Windows does not trust " +
            "the certificate ($($sig.Status)); accepted because TLP_SIGN_ALLOW_UNTRUSTED=1 (test certificates only)")
        return
    }
    Write-Indented $verify.Output ''
    throw "signtool verify /pa failed for ${File}: $($sig.StatusMessage)"
}

# Signer-specific signtool arguments and a printable (secret-free) label.
function Get-SignerArgs([string]$Mode, [string]$TempDir) {
    switch ($Mode) {
        'pfx' {
            $pfx = Get-EnvValue 'TLP_SIGN_PFX'
            if (-not (Test-Path -LiteralPath $pfx -PathType Leaf)) { throw "TLP_SIGN_PFX points to a missing file: $pfx" }
            $a = @('/f', $pfx)
            # Not trimmed: a password may legitimately start or end with a space.
            $pw = [Environment]::GetEnvironmentVariable('TLP_SIGN_PFX_PASSWORD')
            if ($pw) { $a += @('/p', $pw) }
            return @{ Args = $a; Label = "PFX file $(Split-Path $pfx -Leaf)"; Secret = $pw }
        }
        'thumbprint' {
            # Copying from certmgr adds spaces and an invisible left-to-right mark: keep hex only.
            $tp = (Get-EnvValue 'TLP_SIGN_THUMBPRINT') -replace '[^0-9A-Fa-f]', ''
            if ($tp.Length -ne 40) { throw 'TLP_SIGN_THUMBPRINT must be a 40-hex-digit SHA-1 thumbprint' }
            $a = @('/sha1', $tp, '/s', 'My')
            $where = 'CurrentUser\My'
            if ((Get-EnvValue 'TLP_SIGN_CERT_STORE') -eq 'machine') { $a += '/sm'; $where = 'LocalMachine\My' }
            return @{ Args = $a; Label = "certificate $tp in $where"; Secret = '' }
        }
        'ats' {
            $dlib = Find-AtsDlib
            if (-not $dlib) { return $null }
            $meta = [ordered]@{
                Endpoint               = Get-EnvValue 'TLP_SIGN_ATS_ENDPOINT'
                CodeSigningAccountName = Get-EnvValue 'TLP_SIGN_ATS_ACCOUNT'
                CertificateProfileName = Get-EnvValue 'TLP_SIGN_ATS_PROFILE'
                CorrelationId          = "TLP-Desk-$([Environment]::MachineName)"
            }
            $exclude = @((Get-EnvValue 'TLP_SIGN_ATS_EXCLUDE_CREDENTIALS') -split '[,;\s]+' | Where-Object { $_ })
            if ($exclude.Count -gt 0) { $meta.ExcludeCredentials = $exclude }
            $json = Join-Path $TempDir 'metadata.json'
            [IO.File]::WriteAllText($json, ($meta | ConvertTo-Json), (New-Object Text.UTF8Encoding $false))
            $a = @('/dlib', $dlib, '/dmdf', $json)
            return @{ Args = $a; Label = "Azure Artifact Signing $($meta.CodeSigningAccountName)/$($meta.CertificateProfileName)"; Secret = '' }
        }
    }
    throw "unknown signing mode $Mode"
}

# Fallback when the client tools are not installed: the ArtifactSigning PowerShell module
# (Install-Module ArtifactSigning), or its predecessor TrustedSigning.
function Invoke-AtsModule([string[]]$Files, [string[]]$TimestampUrls) {
    $cmd = Get-Command Invoke-ArtifactSigning, Invoke-TrustedSigning -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $cmd) {
        throw ('Azure Artifact Signing is configured but neither Azure.CodeSigning.Dlib.dll nor the ArtifactSigning ' +
            'PowerShell module was found. Install one: winget install -e --id Microsoft.Azure.ArtifactSigningClientTools ' +
            "(or set TLP_SIGN_ATS_DLIB), or Install-Module ArtifactSigning -Scope CurrentUser. See $Guide.")
    }
    $names = @($cmd.Parameters.Keys)
    $p = @{
        Endpoint               = Get-EnvValue 'TLP_SIGN_ATS_ENDPOINT'
        CertificateProfileName = Get-EnvValue 'TLP_SIGN_ATS_PROFILE'
        FileDigest             = 'SHA256'
        TimestampRfc3161       = $TimestampUrls[0]
        TimestampDigest        = 'SHA256'
    }
    # The account parameter was renamed together with the service.
    if ($names -contains 'SigningAccountName') { $p.SigningAccountName = Get-EnvValue 'TLP_SIGN_ATS_ACCOUNT' }
    else { $p.CodeSigningAccountName = Get-EnvValue 'TLP_SIGN_ATS_ACCOUNT' }
    if ($cmd.Parameters['Files'].ParameterType -eq [string]) { $p.Files = $Files -join ',' } else { $p.Files = $Files }
    if ($names -contains 'Description') { $p.Description = $Description }
    if ($names -contains 'DescriptionUrl') { $p.DescriptionUrl = $DescriptionUrl }
    Write-Host "==> $($cmd.Name) ($($Files.Count) file(s))" -ForegroundColor Cyan
    & $cmd @p
}

# ---------------------------------------------------------------------------------------------

$mode = Get-SigningMode
if ($Probe) { return $mode }

$files = @($Path | Where-Object { $_ })
if ($files.Count -eq 0) { throw 'usage: sign.ps1 <file> [<file> ...] | -Probe' }
$files = @($files | ForEach-Object {
        if (-not (Test-Path -LiteralPath $_ -PathType Leaf)) { throw "file to sign not found: $_" }
        (Resolve-Path -LiteralPath $_).ProviderPath
    })

if ($mode -eq 'none') {
    if (Test-EnvFlag 'TLP_SIGN_REQUIRED') {
        throw "TLP_SIGN_REQUIRED=1 but no signing method is configured (TLP_SIGN_ATS_*, TLP_SIGN_PFX or TLP_SIGN_THUMBPRINT); see $Guide"
    }
    Write-UnsignedNotice $files.Count
    return
}
if ((Test-EnvFlag 'TLP_SIGN_REQUIRED') -and (Test-EnvFlag 'TLP_SIGN_ALLOW_UNTRUSTED')) {
    throw 'TLP_SIGN_ALLOW_UNTRUSTED=1 is for test certificates and cannot be combined with TLP_SIGN_REQUIRED=1'
}

$timestamps = Get-TimestampUrls $mode
if ($timestamps -contains 'none') {
    if (Test-EnvFlag 'TLP_SIGN_REQUIRED') { throw 'TLP_SIGN_TIMESTAMP_URL=none is not allowed with TLP_SIGN_REQUIRED=1' }
    Write-Warning 'signing WITHOUT a timestamp: the signature becomes invalid when the certificate expires (tests only)'
    $timestamps = @('none')
}

$signTool = Find-SignTool
$temp = Join-Path ([IO.Path]::GetTempPath()) ("tlp-sign-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $temp | Out-Null
try {
    $signer = Get-SignerArgs $mode $temp
    if ($null -eq $signer) {
        # Artifact Signing without the client tools: the module signs, then we verify like any other.
        Invoke-AtsModule $files $timestamps
    } else {
        Write-Host "==> signing $($files.Count) file(s) with $($signer.Label) [$signTool]" -ForegroundColor Cyan
        foreach ($file in $files) {
            $done = $false
            $last = $null
            # Timestamp servers are occasionally down: two passes over the list, with a pause.
            foreach ($pass in 1..2) {
                foreach ($ts in $timestamps) {
                    $a = @('sign', '/fd', 'SHA256') + $signer.Args + @('/d', $Description, '/du', $DescriptionUrl)
                    if ($ts -ne 'none') { $a += @('/tr', $ts, '/td', 'SHA256') }
                    if (Test-EnvFlag 'TLP_SIGN_DEBUG') { $a += '/debug' }
                    $a += @('/v', $file)
                    $last = Invoke-Native $signTool $a
                    if ($last.Code -eq 0) { $done = $true; break }
                    Write-Warning "signtool sign failed for $(Split-Path $file -Leaf) (exit $($last.Code), timestamp $ts); retrying"
                    Start-Sleep -Seconds (2 * $pass)
                }
                if ($done) { break }
            }
            if (-not $done) {
                Write-Indented $last.Output $signer.Secret
                throw "could not sign $file (see the signtool output above)"
            }
            Write-Host "    signed:   $(Split-Path $file -Leaf)"
        }
    }
    foreach ($file in $files) { Test-Signature $signTool $file }
} finally {
    Remove-Item -Recurse -Force -LiteralPath $temp -ErrorAction SilentlyContinue
}
