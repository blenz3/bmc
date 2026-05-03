<#
.SYNOPSIS
    Live L2 orderbook for the currently-trading BTC 15-minute Kalshi contract.

.DESCRIPTION
    Wrapper around the kalshi-book-watch binary. Defaults to the BTC 15-minute
    series (KXBTC15M) and discovers the soonest-closing in-future open market —
    i.e., the contract currently trading. Streams its L2 orderbook to the
    console with ANSI redraws.

    Other BTC series exposed by Kalshi:
        KXBTC      — hourly
        KXBTC15M   — 15-minute (default)
        KXBTCD     — daily
        KXBTCY     — year-end

    To watch a different one:
        .\scripts\kalshi-watch-btc.ps1 --series-ticker KXBTC

    Skip discovery entirely and watch a known market:
        .\scripts\kalshi-watch-btc.ps1 --ticker KXBTC-26MAY031215-T100000

    Any extra arguments (after the script's own flags, if any) are forwarded
    to the binary, so depth and render interval are tunable:
        .\scripts\kalshi-watch-btc.ps1 --depth 20 --render-interval-ms 100

.NOTES
    Builds in release mode by default. Override with
    $env:WATCH_BUILD_PROFILE = "debug" to iterate faster on code changes.

    Auth is optional — the orderbook channel is public. KALSHI_KEY_ID /
    KALSHI_KEY_PEM_PATH env vars are picked up automatically if set.
#>

[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$ExtraArgs
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$rustDir  = Join-Path $repoRoot "rust"

$profile = if ($env:WATCH_BUILD_PROFILE) { $env:WATCH_BUILD_PROFILE } else { "release" }
$profileFlag = if ($profile -eq "release") { "--release" } else { $null }

Push-Location $rustDir
try {
    $cargoArgs = @("run")
    if ($profileFlag) { $cargoArgs += $profileFlag }
    $cargoArgs += @(
        "-p", "kalshi-book-watch", "--",
        "--series-ticker", "KXBTC15M"
    )
    if ($ExtraArgs) { $cargoArgs += $ExtraArgs }
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) {
        # Code 130 is SIGINT/Ctrl+C on Unix; on Windows the exit can be 0 or
        # the cargo wrapper exit. Don't treat clean shutdown as failure.
        if ($LASTEXITCODE -ne 130) {
            throw "kalshi-book-watch exited with code $LASTEXITCODE"
        }
    }
}
finally {
    Pop-Location
}
