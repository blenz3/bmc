<#
.SYNOPSIS
    Wrapper for the kalshi-cli binary (REST trading CLI).

.DESCRIPTION
    Forwards all arguments to the binary verbatim. Auth is picked up from
    KALSHI_KEY_ID and KALSHI_KEY_PEM_PATH env vars (or the binary's
    --key-id / --key-pem flags).

    Read-only commands:
        .\scripts\kalshi-cli.ps1 balance
        .\scripts\kalshi-cli.ps1 positions
        .\scripts\kalshi-cli.ps1 orders --status resting
        .\scripts\kalshi-cli.ps1 fills --limit 20
        .\scripts\kalshi-cli.ps1 order ord_abc123
        .\scripts\kalshi-cli.ps1 --json positions       # JSON for piping into jq

    Destructive commands prompt for confirmation by default; -y skips:
        .\scripts\kalshi-cli.ps1 place --ticker KX-FOO --side yes --action buy --count 10 --yes-price 56 --tif gtc
        .\scripts\kalshi-cli.ps1 cancel ord_abc123
        .\scripts\kalshi-cli.ps1 decrease ord_abc123 --to 5
        .\scripts\kalshi-cli.ps1 -y cancel ord_abc123    # skip prompt

    Dry-run / paper mode short-circuits destructive calls with a clear error:
        .\scripts\kalshi-cli.ps1 --paper place --ticker KX-FOO --side yes --action buy --count 10 --yes-price 56

.NOTES
    Uses release build by default (faster startup for interactive CLI use).
    Override with $env:KALSHI_CLI_BUILD_PROFILE = "debug" to iterate on code.

    cargo's own output is suppressed via -q so the binary's stdout/stderr
    pass through cleanly — important for piping --json output into jq.
#>

[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$ExtraArgs
)

# Note: not setting $ErrorActionPreference = "Stop" here. Cargo writes
# informational warnings to stderr; with Stop semantics, PowerShell would
# treat those as terminating errors and corrupt the binary's exit code.

$repoRoot = Split-Path -Parent $PSScriptRoot
$rustDir  = Join-Path $repoRoot "rust"

$profile = if ($env:KALSHI_CLI_BUILD_PROFILE) { $env:KALSHI_CLI_BUILD_PROFILE } else { "release" }
$profileFlag = if ($profile -eq "release") { "--release" } else { $null }

$code = 0
Push-Location $rustDir
try {
    $cargoArgs = @("run", "-q")
    if ($profileFlag) { $cargoArgs += $profileFlag }
    $cargoArgs += @("-p", "kalshi-cli", "--")
    if ($ExtraArgs) { $cargoArgs += $ExtraArgs }
    & cargo @cargoArgs
    $code = $LASTEXITCODE
}
finally {
    Pop-Location
}
exit $code
