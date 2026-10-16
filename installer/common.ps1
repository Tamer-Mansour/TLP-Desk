# Shared helpers for installer\portable.ps1 and installer\build-setup.ps1 (dot-source it).
# Everything works offline (only the local cargo registry cache is used).

$ErrorActionPreference = 'Stop'
$script:Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
# Output folder: dist\ in the repo, or TLP_DESK_DIST_DIR to keep build output outside the repo.
$script:Dist = if ($env:TLP_DESK_DIST_DIR) { [IO.Path]::GetFullPath($env:TLP_DESK_DIST_DIR) } else { Join-Path $script:Root 'dist' }
New-Item -ItemType Directory -Force $script:Dist | Out-Null
$script:SignScript = Join-Path $PSScriptRoot 'sign.ps1'
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding $false
$script:Utf8Bom = New-Object System.Text.UTF8Encoding $true

# 'ats', 'pfx', 'thumbprint' or 'none': which certificate installer\sign.ps1 would use.
function Get-SigningMode {
    return (& $script:SignScript -Probe)
}

# Signs $Files in place with installer\sign.ps1; with no certificate configured it only prints
# the bilingual "unsigned" warning (or fails when TLP_SIGN_REQUIRED=1).
function Invoke-CodeSign([string[]]$Files) {
    & $script:SignScript @Files
}

function Add-ToolPaths {
    # NASM gives openh264 its assembly fast paths; cargo may not be on PATH in a fresh shell.
    foreach ($p in @((Join-Path $env:LOCALAPPDATA 'bin\NASM'), (Join-Path $env:USERPROFILE '.cargo\bin'))) {
        if ((Test-Path $p) -and -not ($env:Path -split ';' -contains $p)) { $env:Path = "$p;$env:Path" }
    }
}

function Invoke-ReleaseBuild {
    Add-ToolPaths
    Write-Host '==> cargo build --release -p tlp-desk-app -p tlp-desk-cli' -ForegroundColor Cyan
    Push-Location $script:Root
    # Native stderr must not become a terminating error when output is redirected (PS 5.1).
    $eap = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
    try {
        cargo build --release --offline -p tlp-desk-app -p tlp-desk-cli
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
    } finally { $ErrorActionPreference = $eap; Pop-Location }
}

function Get-WorkspaceVersion {
    $cargoToml = Get-Content (Join-Path $script:Root 'Cargo.toml') -Raw
    if ($cargoToml -notmatch '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"') { throw 'workspace version not found' }
    return $Matches[1]
}

# Copies TLP-Desk.exe, TLP-Desk-cli.exe (signed when a certificate is configured), README.txt
# (from $ReadmeSource) and licenses\ into $Stage.
function New-PayloadStage([string]$Stage, [string]$ReadmeSource) {
    # egui's built-in fonts are embedded in the exe: ship their licenses too.
    $lock = Get-Content (Join-Path $script:Root 'Cargo.lock') -Raw
    if ($lock -notmatch 'name = "epaint_default_fonts"\s+version = "([^"]+)"') { throw 'epaint_default_fonts not in Cargo.lock' }
    $fontsVer = $Matches[1]
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
    $eguiFonts = Get-ChildItem (Join-Path $cargoHome 'registry\src') -Directory |
        ForEach-Object { Join-Path $_.FullName "epaint_default_fonts-$fontsVer\fonts" } |
        Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $eguiFonts) { throw "epaint_default_fonts-$fontsVer sources not found under $cargoHome" }

    if (Test-Path $Stage) { Remove-Item -Recurse -Force $Stage }
    New-Item -ItemType Directory -Force (Join-Path $Stage 'licenses') | Out-Null
    # Honour CARGO_TARGET_DIR (build output may live outside the repo, e.g. on another drive).
    $target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $script:Root 'target' }
    Copy-Item (Join-Path $target 'release\TLP-Desk.exe') $Stage
    Copy-Item (Join-Path $target 'release\TLP-Desk-cli.exe') $Stage
    # Sign the staged copies (setup and portable zip alike); target\ keeps cargo's own output.
    Invoke-CodeSign @((Join-Path $Stage 'TLP-Desk.exe'), (Join-Path $Stage 'TLP-Desk-cli.exe'))
    # README as UTF-8 with BOM so every Notepad version shows the Arabic text.
    $readme = Get-Content $ReadmeSource -Raw -Encoding UTF8
    [System.IO.File]::WriteAllText((Join-Path $Stage 'README.txt'), $readme, $script:Utf8Bom)
    Copy-Item (Join-Path $script:Root 'crates\tlp-desk-app\assets\fonts\NotoSansArabic-OFL.txt') (Join-Path $Stage 'licenses')
    # GUI icons (tlp-icons, MIT; Lucide geometry, ISC) are embedded in the exe.
    Get-ChildItem (Join-Path $script:Root 'crates\tlp-desk-app\assets\icons\licenses') -Filter '*.txt' | ForEach-Object {
        Copy-Item $_.FullName (Join-Path $Stage 'licenses')
    }
    Copy-Item (Join-Path $script:Root 'THIRD_PARTY_NOTICES.md') (Join-Path $Stage 'licenses\THIRD_PARTY_NOTICES.txt')
    Get-ChildItem $eguiFonts -Filter '*.txt' | ForEach-Object {
        Copy-Item $_.FullName (Join-Path $Stage "licenses\egui-$($_.Name)")
    }
}

# Adds/replaces the line for $File in dist\SHA256SUMS.txt, keeping lines for other files.
function Update-Sha256Sums([string]$File) {
    $name = Split-Path $File -Leaf
    $hash = (Get-FileHash -Algorithm SHA256 $File).Hash.ToLowerInvariant()
    $sums = Join-Path $script:Dist 'SHA256SUMS.txt'
    $lines = @()
    if (Test-Path $sums) {
        $lines = @(Get-Content $sums | Where-Object {
            $_ -and ($_ -split '\s+', 2)[1] -ne $name -and (Test-Path (Join-Path $script:Dist (($_ -split '\s+', 2)[1])))
        })
    }
    $lines += "$hash  $name"
    [System.IO.File]::WriteAllText($sums, (($lines | Sort-Object { ($_ -split '\s+', 2)[1] }) -join "`n") + "`n", $script:Utf8NoBom)
    return $hash
}
