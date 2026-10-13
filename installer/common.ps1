# Shared helpers for installer\portable.ps1 and installer\build-setup.ps1 (dot-source it).
# Everything works offline (only the local cargo registry cache is used).

$ErrorActionPreference = 'Stop'
$script:Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$script:Dist = Join-Path $script:Root 'dist'
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding $false
$script:Utf8Bom = New-Object System.Text.UTF8Encoding $true

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

# Copies tlp-desk.exe, tlp-desk-cli.exe, README.txt (from $ReadmeSource) and licenses\ into $Stage.
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
    Copy-Item (Join-Path $script:Root 'target\release\tlp-desk.exe') $Stage
    Copy-Item (Join-Path $script:Root 'target\release\tlp-desk-cli.exe') $Stage
    # README as UTF-8 with BOM so every Notepad version shows the Arabic text.
    $readme = Get-Content $ReadmeSource -Raw -Encoding UTF8
    [System.IO.File]::WriteAllText((Join-Path $Stage 'README.txt'), $readme, $script:Utf8Bom)
    Copy-Item (Join-Path $script:Root 'crates\tlp-desk-app\assets\fonts\NotoSansArabic-OFL.txt') (Join-Path $Stage 'licenses')
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
