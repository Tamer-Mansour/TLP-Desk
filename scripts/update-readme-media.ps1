<#
.SYNOPSIS
  Regenerate the README / docs screenshots and demo video in docs/media/.

.DESCRIPTION
  1. Builds and runs crates/tlp-desk-app/examples/readme_media, which renders the real UI
     headlessly (egui_kittest + wgpu on a software adapter: WARP on Windows) into
     docs/media/<scene>-<lang>-<theme>.png and renders the demo frames into -RawDir.
  2. Encodes the demo with ffmpeg into docs/media/demo-<lang>.mp4 (H.264) and
     docs/media/demo-<lang>.gif (palette-optimized, for inline display on GitHub).
  3. Prints a size table and checks the budget (each PNG <= 400 KB, each MP4 <= 4 MB, each GIF
     <= 5 MB, docs/media <= 15 MB).

  Output is deterministic: with no UI change a re-run produces byte-identical files, so git only
  shows real changes. Run it in any PR that changes visible UI and commit docs/media/.

  Needs ffmpeg on PATH (or installed with `winget install Gyan.FFmpeg`). Honors CARGO_TARGET_DIR.

.PARAMETER RawDir
  Scratch folder for demo frames (never inside the repo). Default: %TEMP%\tlp-desk-media-raw.

.PARAMETER SkipVideo
  Only regenerate the screenshots (no ffmpeg needed).

.PARAMETER Only
  Render only these scenes (names from `--list`); implies -SkipVideo.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\update-readme-media.ps1
#>
[CmdletBinding()]
param(
    [string]$RawDir = (Join-Path ([IO.Path]::GetTempPath()) 'tlp-desk-media-raw'),
    [switch]$SkipVideo,
    [string[]]$Only = @()
)

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$media = Join-Path $root 'docs\media'
if ($Only.Count -gt 0) { $SkipVideo = $true }

if ([IO.Path]::GetFullPath($RawDir).StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) {
    throw "RawDir must be outside the repository: $RawDir"
}

function Find-Ffmpeg {
    $cmd = Get-Command ffmpeg -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    # winget installs Gyan.FFmpeg without always putting it on PATH for the current shell.
    $winget = Join-Path $env:LOCALAPPDATA 'Microsoft\WinGet\Packages'
    if (Test-Path $winget) {
        $hit = Get-ChildItem $winget -Directory -Filter 'Gyan.FFmpeg*' -ErrorAction SilentlyContinue |
            ForEach-Object { Get-ChildItem $_.FullName -Recurse -Filter ffmpeg.exe -ErrorAction SilentlyContinue } |
            Select-Object -First 1
        if ($hit) { return $hit.FullName }
    }
    return $null
}

$ffmpeg = $null
if (-not $SkipVideo) {
    $ffmpeg = Find-Ffmpeg
    if (-not $ffmpeg) {
        throw 'ffmpeg not found. Install it (winget install Gyan.FFmpeg, or choco install ffmpeg) and put it on PATH, or run with -SkipVideo for screenshots only.'
    }
    Write-Host "ffmpeg: $ffmpeg"
}

# Build the generator (release: rendering 660 demo frames in debug takes ages).
Push-Location $root
try {
    cargo build --release --locked -p tlp-desk-app --example readme_media
    if ($LASTEXITCODE -ne 0) { throw 'building examples/readme_media failed' }
} finally { Pop-Location }
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
$exe = Join-Path $target 'release\examples\readme_media.exe'
if (-not (Test-Path $exe)) { $exe = Join-Path $target 'release/examples/readme_media' }

# Screenshots straight into docs/media, demo frames into the scratch folder.
$genArgs = @('--out', $media)
foreach ($s in $Only) { $genArgs += @('--only', $s) }
if (-not $SkipVideo) {
    if (Test-Path $RawDir) { Remove-Item -Recurse -Force $RawDir }
    $genArgs += @('--frames', $RawDir)
}
& $exe @genArgs
if ($LASTEXITCODE -ne 0) { throw 'readme_media failed' }

# Replace $dest with $src only if the bytes differ (keeps git and mtimes quiet).
function Update-IfChanged([string]$src, [string]$dest) {
    if ((Test-Path $dest) -and
        ((Get-FileHash $src).Hash -eq (Get-FileHash $dest).Hash)) {
        Remove-Item $src
        return 'same'
    }
    Move-Item -Force $src $dest
    return 'updated'
}

if (-not $SkipVideo) {
    # Bit-exact flags and a single thread keep the encoders reproducible run to run.
    $quiet = @('-hide_banner', '-loglevel', 'error', '-y')
    $exact = @('-map_metadata', '-1', '-fflags', '+bitexact')
    foreach ($lang in 'en', 'ar') {
        $frames = Join-Path $RawDir "$lang\f%05d.png"
        $mp4 = Join-Path $RawDir "demo-$lang.mp4"
        & $ffmpeg @quiet -framerate 30 -i $frames @exact -c:v libx264 -preset slow -crf 20 -tune animation `
            -pix_fmt yuv420p -threads 1 -flags:v +bitexact -movflags +faststart $mp4
        if ($LASTEXITCODE -ne 0) { throw "ffmpeg mp4 ($lang) failed" }
        Write-Host ("demo-$lang.mp4  " + (Update-IfChanged $mp4 (Join-Path $media "demo-$lang.mp4")))

        $gif = Join-Path $RawDir "demo-$lang.gif"
        $filter = 'fps=12,scale=800:-1:flags=lanczos,split[a][b];' +
            '[a]palettegen=max_colors=128:stats_mode=diff[p];' +
            '[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle'
        & $ffmpeg @quiet -framerate 30 -i $frames @exact -filter_complex $filter -threads 1 -loop 0 $gif
        if ($LASTEXITCODE -ne 0) { throw "ffmpeg gif ($lang) failed" }
        Write-Host ("demo-$lang.gif  " + (Update-IfChanged $gif (Join-Path $media "demo-$lang.gif")))
    }
}

# Size table and budget.
$limits = @{ '.png' = 400KB; '.mp4' = 4MB; '.gif' = 5MB }
$over = @()
$files = Get-ChildItem $media -File | Sort-Object Name
$files | ForEach-Object {
    $limit = $limits[$_.Extension.ToLowerInvariant()]
    $flag = ''
    if ($limit -and $_.Length -gt $limit) { $flag = '  OVER BUDGET'; $over += $_.Name }
    '{0,-36} {1,10:N0} KB{2}' -f $_.Name, [math]::Ceiling($_.Length / 1KB), $flag
} | Write-Host
$total = ($files | Measure-Object Length -Sum).Sum
Write-Host ('{0,-36} {1,10:N0} KB (budget 15,360 KB)' -f 'TOTAL docs/media', [math]::Ceiling($total / 1KB))
if ($total -gt 15MB) { $over += 'docs/media total' }
if ($over.Count -gt 0) { throw "media over budget: $($over -join ', ')" }
Write-Host 'Done. Review with `git status docs/media` and commit the changes with your UI change.'
