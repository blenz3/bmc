<#
.SYNOPSIS
    Snapshots Kalshi reference data into refdata/<YYYYMMDD>/kalshi/ at the repo root.

.DESCRIPTION
    Resolves the repo root from the script's own location (so it works from any
    cwd), creates refdata/<YYYYMMDD>/kalshi/, then runs the
    kalshi-refdata-download binary with --out-dir pointing there.

    The per-source subdirectory (kalshi/) leaves room for additional venues
    (polymarket/, manifold/, ...) under the same dated snapshot folder.

    By default, /events and /markets are filtered to status=open — settled
    history is huge and not needed for most operational use cases. /series is
    a catalog of recurring templates; Kalshi's API doesn't accept a status
    filter on it, so we always fetch the full series list (it's small — one
    page of ~10k records).

    To include everything, run the binary directly with --markets-status all
    --events-status all (the script's hardcoded filters would conflict with
    duplicate flags through ExtraArgs).

    Any extra arguments are forwarded to the binary, so you can do:
        .\scripts\kalshi-download-refdata.ps1 --env demo --request-delay-ms 500

.NOTES
    Uses release build by default for the small CPU win on serde + reqwest;
    set $env:DOWNLOAD_REFDATA_PROFILE = "debug" to override.
#>

[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$ExtraArgs
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$rustDir  = Join-Path $repoRoot "rust"
$date     = Get-Date -Format "yyyyMMdd"
$outDir   = Join-Path (Join-Path (Join-Path $repoRoot "refdata") $date) "kalshi"

New-Item -ItemType Directory -Path $outDir -Force | Out-Null
Write-Host "writing to $outDir"

$profile = if ($env:DOWNLOAD_REFDATA_PROFILE) { $env:DOWNLOAD_REFDATA_PROFILE } else { "release" }
$profileFlag = if ($profile -eq "release") { "--release" } else { $null }

Push-Location $rustDir
try {
    $cargoArgs = @("run")
    if ($profileFlag) { $cargoArgs += $profileFlag }
    $cargoArgs += @(
        "-p", "kalshi-refdata-download", "--",
        "--out-dir", $outDir,
        "--markets-status", "open",
        "--events-status", "open"
    )
    if ($ExtraArgs) { $cargoArgs += $ExtraArgs }
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) {
        throw "kalshi-refdata-download exited with code $LASTEXITCODE"
    }
}
finally {
    Pop-Location
}

Write-Host "done. snapshot: $outDir"
