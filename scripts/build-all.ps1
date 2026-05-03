<#
.SYNOPSIS
    Build every package in the monorepo across all languages.

.DESCRIPTION
    Today this is just the Rust workspace (lib + bin + examples + tests via
    --all-targets). When other languages land (python/, notebooks/, sql/),
    add a new "==> <language>" block below.

    Any extra arguments are forwarded to cargo, so:
        .\scripts\build-all.ps1 --release
        .\scripts\build-all.ps1 -p kalshi-ws

.NOTES
    Exits non-zero on the first failed sub-build so CI can rely on the status.
#>

[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$ExtraArgs
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$rustDir  = Join-Path $repoRoot "rust"

Write-Host "==> Rust workspace ($rustDir)" -ForegroundColor Cyan
Push-Location $rustDir
try {
    $cargoArgs = @("build", "--workspace", "--all-targets") + $ExtraArgs
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed (exit $LASTEXITCODE)"
    }
}
finally {
    Pop-Location
}

Write-Host "==> done" -ForegroundColor Green
