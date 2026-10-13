# TLP-Desk quality gate: release build, clippy with warnings as errors, tests.
# Usage: pwsh scripts/check.ps1   (from anywhere)
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

function Step($name, [scriptblock]$cmd) {
    Write-Host "==> $name" -ForegroundColor Cyan
    & $cmd
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAILED: $name" -ForegroundColor Red
        exit $LASTEXITCODE
    }
}

Step 'fmt'    { cargo fmt --all -- --check }
Step 'build'  { cargo build --release --workspace }
Step 'clippy' { cargo clippy --workspace --all-targets -- -D warnings }
Step 'test'   { cargo test --workspace }

Write-Host 'All checks passed.' -ForegroundColor Green
