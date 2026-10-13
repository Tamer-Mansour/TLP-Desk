# Vendors the curated icon allow-list from a local tlp-icons checkout into
# crates\tlp-desk-app\assets\icons\ (static SVG files + license texts + README with the commit used).
#
# Dev-time only: building the app never runs this and needs neither the network nor the checkout.
#
# Usage (PowerShell 5.1+):
#   git clone --depth 1 https://github.com/Tamer-Mansour/tlp-icons C:\tlp-build\ref\tlp-icons
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\vendor-icons.ps1 -Source C:\tlp-build\ref\tlp-icons
#
# tlp-icons stores every icon as inner SVG markup on a 24x24 grid (stroke = currentColor, width 2,
# round caps/joins). Precedence follows tlp-icons' own build: originals\ > overrides\ > upstream\
# (lucide.json first, then tabler.json). Part classes (p1, p2, ...) only drive the animations, which
# egui does not need: they are dropped. Class "f" (solid fill) becomes fill="currentColor"; class
# "d" (optional duotone body, transparent by default) is dropped.
param(
    [Parameter(Mandatory = $true)][string]$Source
)
$ErrorActionPreference = 'Stop'

$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$out = Join-Path $root 'crates\tlp-desk-app\assets\icons'
$allowFile = Join-Path $out 'allowlist.txt'
$utf8 = New-Object System.Text.UTF8Encoding $false
$Source = (Resolve-Path $Source).Path

function Read-Json([string]$path) {
    return ([System.IO.File]::ReadAllText($path, $utf8) | ConvertFrom-Json)
}

# ---------------------------------------------------------------- allow-list
$entries = @()
foreach ($line in [System.IO.File]::ReadAllLines($allowFile, $utf8)) {
    $t = $line.Trim()
    if (-not $t -or $t.StartsWith('#')) { continue }
    $parts = $t -split '\s+', 3
    if ($parts.Count -lt 3) { throw "allowlist: bad line '$line'" }
    $entries += [pscustomobject]@{ Variant = $parts[0]; Name = $parts[1]; Purpose = $parts[2] }
}
$dupes = $entries | Group-Object Variant | Where-Object Count -gt 1
if ($dupes) { throw "allowlist: duplicate variants: $($dupes.Name -join ', ')" }

# ---------------------------------------------------------------- sources
$commit = (git -C $Source rev-parse HEAD).Trim()
$commitDate = (git -C $Source log -1 --format=%cs).Trim()
$pkg = Read-Json (Join-Path $Source 'package.json')
$upstream = [ordered]@{}
$versions = @{}
foreach ($id in @('lucide', 'tabler')) {
    $snap = Read-Json (Join-Path $Source "upstream\$id.json")
    $versions[$id] = $snap.version
    foreach ($icon in $snap.icons) {
        if (-not $upstream.Contains($icon.name)) {
            $upstream[$icon.name] = [pscustomobject]@{ Source = $id; Svg = $icon.svg; Categories = @($icon.categories) }
        }
    }
}

# Same rule as tlp-icons' scripts/build.mjs for its `r` (directional / mirror in RTL) flag.
$rtlWords = @('left', 'right', 'forward', 'back', 'backward', 'reply', 'undo', 'redo', 'send', 'login', 'logout', 'log', 'previous', 'next', 'skip', 'rewind', 'fast', 'indent', 'outdent', 'corner', 'quote', 'share', 'exit', 'enter')
function Test-Rtl([string]$name, [string[]]$categories) {
    if ($name -match '(^|-)(align|text-align|panel|layout|sidebar|columns|chart|scale|flip)(-|$)') { return $false }
    $tokens = $name -split '-'
    if ($tokens | Where-Object { $rtlWords -contains $_ }) { return $true }
    return ($categories -contains 'arrows') -and (($tokens -contains 'left') -or ($tokens -contains 'right'))
}

# Hand-made markup (overrides/originals): strip presentation attributes like tlp-icons' build does.
function Clear-Presentation([string]$svg) {
    return ([regex]::Replace($svg, '\s(stroke|fill|style|stroke-width|color)="[^"]*"', '')).Trim()
}

function ConvertTo-Static([string]$inner) {
    return [regex]::Replace($inner, '\sclass="([^"]*)"', {
            param($m)
            if (($m.Groups[1].Value -split '\s+') -contains 'f') { ' fill="currentColor"' } else { '' }
        })
}

$head = '<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">'

# ---------------------------------------------------------------- write
$written = @{}
$rows = @()
$usedTabler = $false
foreach ($e in $entries) {
    $name = $e.Name
    $orig = Join-Path $Source "originals\$name.json"
    $over = Join-Path $Source "overrides\$name.json"
    if (Test-Path $orig) {
        $o = Read-Json $orig
        $inner = Clear-Presentation $o.svg
        $src = 'tlp-icons original'
        $rtl = Test-Rtl $name @()
    } elseif ($upstream.Contains($name)) {
        $up = $upstream[$name]
        if (Test-Path $over) {
            $inner = Clear-Presentation (Read-Json $over).svg
            $src = "$($up.Source) (tlp-icons override)"
        } else {
            $inner = $up.Svg
            $src = $up.Source
        }
        $rtl = Test-Rtl $name $up.Categories
        if ($up.Source -eq 'tabler') { $usedTabler = $true }
    } else {
        throw "icon '$name' ($($e.Variant)) not found in $Source"
    }
    $svg = $head + (ConvertTo-Static $inner) + "</svg>`n"
    if (-not $written.ContainsKey($name)) {
        [System.IO.File]::WriteAllText((Join-Path $out "$name.svg"), $svg, $utf8)
        $written[$name] = $true
    }
    $rows += "| ``$($e.Variant)`` | ``$name`` | $src | $(if ($rtl) { 'yes' } else { '' }) | $($e.Purpose) |"
}

# Remove SVGs that are no longer on the list.
Get-ChildItem $out -Filter '*.svg' | Where-Object { -not $written.ContainsKey($_.BaseName) } | ForEach-Object {
    Write-Host "removing stale $($_.Name)"
    Remove-Item $_.FullName
}

# ---------------------------------------------------------------- licenses
Copy-Item (Join-Path $Source 'LICENSE') (Join-Path $out 'LICENSE-tlp-icons.txt')
Copy-Item (Join-Path $Source 'licenses\lucide.LICENSE.txt') (Join-Path $out 'LICENSE-lucide.txt')
$tablerLicense = Join-Path $out 'LICENSE-tabler.txt'
$licenses = '`LICENSE-tlp-icons.txt` (MIT), `LICENSE-lucide.txt` (ISC)'
if ($usedTabler) {
    Copy-Item (Join-Path $Source 'licenses\tabler.LICENSE.txt') $tablerLicense
    $licenses += ', `LICENSE-tabler.txt` (MIT)'
} elseif (Test-Path $tablerLicense) {
    Remove-Item $tablerLicense
}

# ---------------------------------------------------------------- README
$readme = @"
# Icons

Generated by ``scripts/vendor-icons.ps1`` from the allow-list in ``allowlist.txt``. Do not edit the
SVG files by hand: change the allow-list and re-run the script.

| | |
| --- | --- |
| Source | [tlp-icons](https://github.com/Tamer-Mansour/tlp-icons) $($pkg.version), commit ``$commit`` ($commitDate) |
| Upstream snapshots | Lucide $($versions['lucide']) (ISC), Tabler Icons $($versions['tabler']) (MIT; only if a row below says tabler) |
| Licenses | $licenses |

Each file is the static frame of the tlp-icons icon (animations are not used in the app): a 24x24
viewBox, ``stroke="currentColor"``, stroke width 2, round caps and joins. The app embeds the files with
``include_bytes!`` and rasterizes them with resvg at the exact pixel size, white, then tints them with
the theme color (see ``src/ui/icons.rs``).

"RTL" marks icons tlp-icons flags as directional. The app's own list (``Icon::is_directional``) decides
what is mirrored in Arabic: arrows that mean "forward/back", connect/disconnect, send, undo/redo.
``arrow-right-left`` (port forwarding) is symmetric in meaning and is not mirrored.

| Icon variant | tlp-icons name | Source | RTL | Used for |
| --- | --- | --- | --- | --- |
$($rows -join "`n")
"@
[System.IO.File]::WriteAllText((Join-Path $out 'README.md'), $readme.Replace("`r`n", "`n") + "`n", $utf8)

Write-Host "Vendored $($written.Count) icons from tlp-icons $commit into $out" -ForegroundColor Green
