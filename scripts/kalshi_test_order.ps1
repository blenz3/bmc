<#
.SYNOPSIS
    One-off: buy ~$1 worth of contracts on the higher-probability side of the
    current BTC 15-min contract.

.DESCRIPTION
    1. Discover the soonest-closing open KXBTC15M market via Kalshi's public
       REST endpoint (no auth needed for discovery).
    2. Inspect yes_bid / yes_ask to compute the unified implied YES probability
       (the mid).
    3. Pick the side whose implied probability is > 50% -- i.e., the favorite.
       Compute the price you'd pay to buy that side at the top of book:
         - YES favored: pay yes_ask cents per contract
         - NO  favored: pay no_ask cents per contract = 100 - yes_bid
    4. Compute count = floor(100 / ask_price) so total cost <= $1.00.
    5. Hand off to kalshi-cli to place the order (IOC limit at the current
       ask).

.PARAMETER Real
    Send a real order. Without this switch the script runs in paper mode --
    discovery and sizing happen normally, but kalshi-cli is invoked with
    --paper, so the destructive call is short-circuited with a clear message
    and no order ever reaches Kalshi. Pass -Real on the command line to allow
    actual order submission. kalshi-cli will still prompt for Y/N confirmation
    before sending.

.PARAMETER WaitSecs
    If no liquid (two-sided) market is found on the first try, poll Kalshi's
    /markets every 2 seconds for up to this many seconds before giving up.
    Useful around the boundary between 15-min windows when the new market
    has just opened and a market maker hasn't put up a quote yet. Default 0
    (no wait).

.NOTES
    Auth pulls from KALSHI_KEY_ID / KALSHI_KEY_PEM_PATH (same as kalshi-cli).

    All non-ASCII characters avoided; PowerShell 5.1 reads .ps1 files in the
    system default codepage and mojibakes UTF-8 em-dashes / multiplication
    signs into a parser failure even inside comments.

.EXAMPLE
    # Dry run -- no order placed regardless of how you answer the prompt.
    .\scripts\kalshi_test_order.ps1

.EXAMPLE
    # Live -- the kalshi-cli confirmation prompt is the last gate before send.
    .\scripts\kalshi_test_order.ps1 -Real
#>

[CmdletBinding()]
param(
    [switch]$Real,

    # When set, retry discovery up to this many seconds while waiting for a
    # market maker to put up a quote on the active 15-min market. 0 = no wait.
    [int]$WaitSecs = 0,

    # Override the series prefix searched for. KXBTC15M is the BTC 15-minute
    # series — heavy MM activity but books are often extreme/one-sided when
    # the underlying has strongly committed. Try KXBTC (hourly), KXBTCD
    # (daily) if 15-min markets are unbuyable.
    [string]$Series = "KXBTC15M"
)

$repoRoot   = Split-Path -Parent $PSScriptRoot
$scriptsDir = Join-Path $repoRoot "scripts"
$paper      = -not $Real.IsPresent

# --- Step 1: discover the active 15-min BTC market ---------------------------
#
# Note: /markets's yes_bid/yes_ask are last-trade-driven and frequently null
# even on markets with active resting orderbooks. The dedicated
# /markets/{ticker}/orderbook endpoint reports live resting bids -- use that
# for liquidity detection, not /markets.

$baseUrl    = "https://api.elections.kalshi.com"
$marketsUrl = "$baseUrl/trade-api/v2/markets?status=open&series_ticker=$Series&limit=1000"

function Get-MarketOrderbook {
    param([string]$BaseUrl, [string]$Ticker)
    $url = "$BaseUrl/trade-api/v2/markets/$Ticker/orderbook"
    return Invoke-RestMethod -Method Get -Uri $url
}

# Internal price unit: deci-cents (1/10000 of a dollar). Lets us preserve
# Kalshi's sub-penny tick precision losslessly while still being convenient
# integer arithmetic.
$script:PRICE_DENOM = 10000

# Normalize one [price, size] entry into @{ priceDC; size; priceDollars }.
# Kalshi's wire format puts both fields as strings with 4 decimal places
# (e.g., ["0.0100", "73020.21"]).
function ConvertTo-PriceLevel {
    param($Entry)
    if ($null -eq $Entry) { return $null }
    if ($Entry -is [System.Collections.IList] -and $Entry.Count -ge 2) {
        try {
            $pDollars = [double]$Entry[0]
            $sFloat   = [double]$Entry[1]
            return @{
                priceDC      = [int][math]::Round($pDollars * $script:PRICE_DENOM)
                priceDollars = $pDollars
                size         = [int][math]::Round($sFloat)
            }
        } catch { return $null }
    }
    return $null
}

# Pull yes/no level arrays out of an /orderbook response. Kalshi's actual
# response uses orderbook_fp.{yes_dollars, no_dollars} with string-encoded
# dollar prices. Older or alternative shapes use orderbook.{yes, no} with
# integer-cent arrays. Handle both for resilience.
function Get-OrderbookSides {
    param($OrderbookResponse)

    $ob       = $null
    $yesField = $null
    $noField  = $null

    if ($OrderbookResponse.orderbook_fp) {
        $ob       = $OrderbookResponse.orderbook_fp
        $yesField = "yes_dollars"
        $noField  = "no_dollars"
    } elseif ($OrderbookResponse.orderbook) {
        $ob       = $OrderbookResponse.orderbook
        $yesField = "yes"
        $noField  = "no"
    } else {
        return @{ yes = @(); no = @() }
    }

    $yesRaw = $ob.$yesField
    $noRaw  = $ob.$noField

    $yesEntries = @()
    $noEntries  = @()
    if ($yesRaw) {
        foreach ($e in $yesRaw) {
            $lvl = ConvertTo-PriceLevel $e
            if ($lvl) { $yesEntries += $lvl }
        }
    }
    if ($noRaw) {
        foreach ($e in $noRaw) {
            $lvl = ConvertTo-PriceLevel $e
            if ($lvl) { $noEntries += $lvl }
        }
    }
    return @{ yes = $yesEntries; no = $noEntries }
}

# Walks open KXBTC15M markets in close_time order and returns the first one
# with two-sided live liquidity (per /orderbook), along with that orderbook.
function Find-LiquidMarket {
    param([string]$BaseUrl, [string]$MarketsUrl)
    $response = Invoke-RestMethod -Method Get -Uri $MarketsUrl
    $markets  = $response.markets
    if (-not $markets) {
        return @{ active = $null; orderbook = $null; allMarkets = @(); attempted = @() }
    }
    $nowUtc = (Get-Date).ToUniversalTime()
    $sorted = @($markets `
        | Where-Object {
            try { ([DateTime]::Parse($_.close_time)).ToUniversalTime() -gt $nowUtc }
            catch { $false }
        } `
        | Sort-Object { ([DateTime]::Parse($_.close_time)).ToUniversalTime() })

    $attempted = @()
    foreach ($m in $sorted) {
        try {
            $obResp = Get-MarketOrderbook -BaseUrl $BaseUrl -Ticker $m.ticker
            $sides  = Get-OrderbookSides -OrderbookResponse $obResp
            $attempted += @{ ticker = $m.ticker; yesCount = $sides.yes.Count; noCount = $sides.no.Count }
            if ($sides.yes.Count -gt 0 -and $sides.no.Count -gt 0) {
                return @{
                    active     = $m
                    orderbook  = $sides
                    allMarkets = $markets
                    attempted  = $attempted
                }
            }
        } catch {
            $attempted += @{ ticker = $m.ticker; error = $_.Exception.Message }
        }
    }
    return @{ active = $null; orderbook = $null; allMarkets = $markets; attempted = $attempted }
}

Write-Host "Discovering active $Series market (via /orderbook)..."
$result = Find-LiquidMarket -BaseUrl $baseUrl -MarketsUrl $marketsUrl

if (-not $result.active -and $WaitSecs -gt 0) {
    $deadline = (Get-Date).AddSeconds($WaitSecs)
    Write-Host "No two-sided orderbook yet; waiting up to $WaitSecs s..."
    while ((Get-Date) -lt $deadline -and -not $result.active) {
        Start-Sleep -Seconds 2
        $result = Find-LiquidMarket -BaseUrl $baseUrl -MarketsUrl $marketsUrl
        $remaining = [int]($deadline - (Get-Date)).TotalSeconds
        if (-not $result.active) {
            Write-Host "  still no liquid orderbook (remaining: ${remaining}s)..."
        }
    }
}

$active   = $result.active
$obSides  = $result.orderbook
if (-not $active) {
    Write-Host ""
    Write-Host "No KXBTC15M market with a two-sided live orderbook right now."
    Write-Host "Markets attempted (in close_time order):"
    if ($result.attempted.Count -eq 0) {
        Write-Host "  (none -- /markets returned $(@($result.allMarkets).Count) markets, none with future close)"
    } else {
        foreach ($a in $result.attempted) {
            if ($a.error) {
                Write-Host ("  {0,-40}  orderbook fetch error: {1}" -f $a.ticker, $a.error)
            } else {
                Write-Host ("  {0,-40}  yes_levels={1,3}  no_levels={2,3}" -f $a.ticker, $a.yesCount, $a.noCount)
            }
        }
        # Diagnostic: dump the raw orderbook response for the first attempted
        # market so we can see whether the venue is genuinely empty or whether
        # the response has a shape my parser isn't recognizing.
        $firstTicker = $result.attempted[0].ticker
        try {
            $rawOb = Get-MarketOrderbook -BaseUrl $baseUrl -Ticker $firstTicker
            Write-Host ""
            Write-Host "Raw /orderbook response for $firstTicker (for debugging):"
            Write-Host ($rawOb | ConvertTo-Json -Depth 5 -Compress:$false)
        } catch {
            Write-Host "(could not fetch raw orderbook for diagnostic: $($_.Exception.Message))"
        }
        # Also dump the market metadata so we can see volume, status, etc.
        $firstMarket = $result.allMarkets | Where-Object { $_.ticker -eq $firstTicker } | Select-Object -First 1
        if ($firstMarket) {
            Write-Host ""
            Write-Host "Market metadata for $firstTicker (selected fields):"
            $firstMarket | Select-Object ticker, event_ticker, status, open_time, close_time, `
                yes_bid, yes_ask, last_price, volume, volume_24h, liquidity, open_interest |
                Format-List | Out-String | Write-Host
        }
    }
    Write-Host "Try -WaitSecs 60 to poll for resting liquidity, or wait for the next 15-min window."
    exit 1
}

$ticker = $active.ticker

Write-Host ""
Write-Host "Market: $ticker"
Write-Host "  closes:    $($active.close_time)"

# --- Step 2: print the live top of book to stdout ----------------------------
# All prices are in deci-cents (priceDC) to preserve sub-penny precision.
# PRICE_DENOM = 10000 means $1.00 maps to 10000.

$yesSorted = @($obSides.yes | Sort-Object -Property { [int]$_.priceDC } -Descending)
$noSorted  = @($obSides.no  | Sort-Object -Property { [int]$_.priceDC } -Descending)

$bestYesBidDC = [int]$yesSorted[0].priceDC
$bestNoBidDC  = [int]$noSorted[0].priceDC
$bestYesAskDC = $script:PRICE_DENOM - $bestNoBidDC

$depth = 10
Write-Host ""
Write-Host "Top of book (live, from /markets/$ticker/orderbook):"
Write-Host ("  {0,12}  {1,10}    {2,12}  {3,10}" -f "YES bid", "size", "NO bid", "size")
Write-Host ("  {0}" -f ("-" * 52))
$rows = [math]::Min($depth, [math]::Max($yesSorted.Count, $noSorted.Count))
for ($i = 0; $i -lt $rows; $i++) {
    $yesCell = "{0,12}  {1,10}" -f "", ""
    $noCell  = "{0,12}  {1,10}" -f "", ""
    if ($i -lt $yesSorted.Count) {
        $yp = "`${0:0.0000}" -f $yesSorted[$i].priceDollars
        $ys = $yesSorted[$i].size
        $yesCell = "{0,12}  {1,10}" -f $yp, $ys
    }
    if ($i -lt $noSorted.Count) {
        $np = "`${0:0.0000}" -f $noSorted[$i].priceDollars
        $ns = $noSorted[$i].size
        $noCell = "{0,12}  {1,10}" -f $np, $ns
    }
    Write-Host ("  {0}    {1}" -f $yesCell, $noCell)
}

$bestYesBidD = "{0:0.0000}" -f ($bestYesBidDC / [double]$script:PRICE_DENOM)
$bestYesAskD = "{0:0.0000}" -f ($bestYesAskDC / [double]$script:PRICE_DENOM)
$bestNoBidD  = "{0:0.0000}" -f ($bestNoBidDC  / [double]$script:PRICE_DENOM)
$spreadD     = "{0:0.0000}" -f (($bestYesAskDC - $bestYesBidDC) / [double]$script:PRICE_DENOM)
Write-Host ""
Write-Host "  best YES bid:  `$$bestYesBidD"
Write-Host "  best YES ask:  `$$bestYesAskD  [= 1.00 - best NO bid `$$bestNoBidD]"
Write-Host "  spread:        `$$spreadD"

# --- Step 3: pick the favored side and the buy price -----------------------

if ($bestYesBidDC -le 0 -or $bestYesAskDC -le 0 -or
    $bestYesBidDC -ge $script:PRICE_DENOM -or $bestYesAskDC -ge $script:PRICE_DENOM) {
    Write-Error "Invalid live quote (yes_bid=$bestYesBidDC, yes_ask=$bestYesAskDC deci-cents) -- market may be one-sided"
    exit 1
}

# Mid in deci-cents; expressing it as a fraction of $1 gives the implied YES probability.
$yesMidDC = ($bestYesBidDC + $bestYesAskDC) / 2.0
$noMidDC  = $script:PRICE_DENOM - $yesMidDC

if ($yesMidDC -ge $noMidDC) {
    $side        = "yes"
    $askPriceDC  = $bestYesAskDC      # cost to buy YES at top of book (deci-cents)
    $priceFlag   = "--yes-price"
    $favored     = "YES"
    $impliedProbPct = ($yesMidDC / [double]$script:PRICE_DENOM) * 100.0
} else {
    $side        = "no"
    $askPriceDC  = $script:PRICE_DENOM - $bestYesBidDC  # cost to buy NO via cross-match
    $priceFlag   = "--no-price"
    $favored     = "NO"
    $impliedProbPct = ($noMidDC / [double]$script:PRICE_DENOM) * 100.0
}

# kalshi-cli's --yes-price / --no-price take INTEGER CENTS (1..=99). On
# sub-penny markets we lose precision when rounding from deci-cents.
$askPriceCents  = [int][math]::Round($askPriceDC / 100.0)
if ($askPriceCents -lt 1) { $askPriceCents = 1 }
if ($askPriceCents -gt 99) { $askPriceCents = 99 }
$askPriceFromCentsDC = $askPriceCents * 100
$wouldFill = $askPriceFromCentsDC -ge $askPriceDC  # rounded limit covers the actual ask?

$yesMidPctText      = "{0:0.0}%" -f (($yesMidDC / [double]$script:PRICE_DENOM) * 100.0)
$impliedPctText     = "{0:F1}%" -f $impliedProbPct
$askDollarsExact    = "{0:0.0000}" -f ($askPriceDC      / [double]$script:PRICE_DENOM)
$askDollarsRounded  = "{0:0.00}"   -f ($askPriceCents   / 100.0)

Write-Host ""
Write-Host "Implied YES mid:        $yesMidPctText"
Write-Host "Favored side:           $favored ($impliedPctText)"
Write-Host "Top-of-book buy price:  `$$askDollarsExact (live ask)"
Write-Host "Order limit (rounded):  $askPriceCents cents (`$$askDollarsRounded)"
if (-not $wouldFill) {
    Write-Host ""
    Write-Host "WARNING: sub-penny ask `$$askDollarsExact rounds to integer-cent limit"
    Write-Host "         `$$askDollarsRounded which is BELOW the live ask. The order will likely"
    Write-Host "         not fill (IOC will cancel cleanly). kalshi-cli's --yes-price/--no-price"
    Write-Host "         flags only accept integer cents 1..=99 right now."
}

# --- Step 4: size the order to ~$1 -------------------------------------------

$count = [int][math]::Floor(100 / $askPriceCents)
if ($count -lt 1) { $count = 1 }
$totalCents = $count * $askPriceCents
$totalDollars = "{0:0.00}" -f ($totalCents / 100.0)
Write-Host "Order size:             $count contracts x $askPriceCents cents = `$$totalDollars"

# --- Step 5: hand off to kalshi-cli for actual placement ---------------------

$cliArgs = @()
if ($paper) { $cliArgs += "--paper" }
$cliArgs += @(
    "place",
    "--ticker",     $ticker,
    "--side",       $side,
    "--action",     "buy",
    "--count",      $count,
    $priceFlag,     $askPriceCents,
    "--tif",        "ioc"
)

Write-Host ""
if ($paper) {
    Write-Host "Mode: DRY RUN (paper mode, --paper passed to kalshi-cli)."
    Write-Host "      No order will reach Kalshi. Re-run with -Real to send for real."
} else {
    Write-Host "Mode: LIVE -- this will place a real order if you answer Y at the prompt."
}
Write-Host ""

& (Join-Path $scriptsDir "kalshi-cli.ps1") @cliArgs
exit $LASTEXITCODE
