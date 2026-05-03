# bmc

Personal polyglot monorepo for Kalshi prediction-market trading. Owner is a senior C++ developer; explanations and code reviews can assume systems-programming fluency.

**Guiding principle: complexity is the primary failure mode.** Every new abstraction, generic, trait, layer, or config knob must earn its keep. Default to saying no — reversing a "no" is cheap, reversing a "yes" is expensive. When in doubt, write less and delete more.

## Layout

Top-level split by language. Each language uses its native tooling without orchestration glue.

```
bmc/
  rust/                              # Rust workspace
    Cargo.toml                       # [workspace] members + shared deps
    crates/
      kalshi-ws/                     # async WebSocket client library (market data, fills feed)
      kalshi-rest/                   # async REST client (orders, positions, balance, fills history)
      kalshi-refdata-download/       # binary: pulls /series /events /markets via REST → NDJSON
      kalshi-book-watch/             # binary: live L2 orderbook for the latest market in a series
      strategy-*/                    # trading strategies (planned)
  scripts/                           # cross-cutting PowerShell wrappers
    build-all.ps1                    # builds every package across all languages
    kalshi-download-refdata.ps1      # snapshots into refdata/<YYYYMMDD>/kalshi/
    kalshi-watch-btc.ps1             # live L2 orderbook for the latest BTC 15-min contract
  refdata/                           # downloaded reference NDJSON, layout: <YYYYMMDD>/<source>/
  CLAUDE.md, README.md
  # python/, notebooks/, sql/ — added when needed, not before
```

Inside `rust/`, `[workspace.dependencies]` in `Cargo.toml` is the single source of truth for versions. Members use `dep = { workspace = true }` and may union extra features locally — they cannot remove workspace features.

## Common commands

Cargo commands run from inside `rust/` (cargo discovers the workspace from there).

```powershell
cd rust
cargo build                                          # whole Rust workspace
cargo test -p kalshi-ws
cargo run -p kalshi-refdata-download                 # raw run; default --out-dir is ./refdata
cargo run -p kalshi-ws --example public_ticker
cargo clippy --all-targets -- -D warnings
```

From the repo root, top-level wrappers handle the multi-language story:

```powershell
.\scripts\build-all.ps1                              # cargo build --workspace --all-targets
.\scripts\build-all.ps1 --release                    # extra args forwarded to cargo
.\scripts\kalshi-download-refdata.ps1                # → refdata/<YYYYMMDD>/kalshi/
.\scripts\kalshi-download-refdata.ps1 --env demo     # extra args forwarded to the binary
```

To run cargo from the repo root without `cd`, use `--manifest-path rust/Cargo.toml`.

## Kalshi specifics

- **Endpoints**:
  - WS prod / demo: `wss://api.elections.kalshi.com/trade-api/ws/v2` / `wss://demo-api.kalshi.co/trade-api/ws/v2`
  - REST prod / demo: `https://api.elections.kalshi.com/trade-api/v2` / `https://demo-api.kalshi.co/trade-api/v2`
- **Auth**: RSA-PSS-SHA256 over `timestamp_ms || METHOD || path` (no query string), base64-standard encoded. Headers `kalshi-access-key`, `kalshi-access-signature`, `kalshi-access-timestamp`. Reuse `kalshi_ws::Credentials::signed_headers(method, path)` — don't reimplement.
- **Header names must be lowercase** — uppercase `&'static str` literals panic in `HeaderName::from_static`.
- **Reference endpoints don't require auth** (`/series`, `/events`, `/markets`, `/exchange/status`). Auth still works if creds are supplied; may give higher rate limits.
- **Orders go via REST, not WebSocket.** `kalshi-rest` covers `POST /portfolio/orders`, cancel, decrease, get, list, plus `/portfolio/{positions,balance,fills}`. The WS feed is read-only — `fill` and `user_orders` channels are post-trade reporting, not order entry.
- **Idempotency for orders**: `kalshi_rest::Client::place_order` always sends a `client_order_id` (auto-generates a UUID v4 if the caller doesn't). Kalshi dedupes on this field, so retries after a network blip are safe.
- **`Mode::Paper`** on `kalshi-rest` hard-refuses `place_order` / `cancel_order` / `decrease_order` before any HTTP — useful for tests and dry-runs. `Mode::Live` is the default; flip to Paper explicitly when scaffolding strategies.
- **REST rate limits**: ~5 RPS per endpoint in practice. The downloader paces at 250ms default and retries 429/5xx with exponential backoff (`Retry-After` honored when present). Don't lower the delay without a reason.
- **REST pagination**: cursor at top level of response; empty/missing `cursor` ⇒ done. `/events` enforces `limit=200`; `/series` returned 9899 records in one page (limit ignored).
- **Numeric fields**: `_dollars` → `f64`, `_fp` (fixed-point) → `i64`, `_ts_ms` → `i64`.

## Environment variables

- `KALSHI_KEY_ID` — API key ID
- `KALSHI_KEY_PEM_PATH` — path to PEM private key (PKCS#1 or PKCS#8 both load)

Picked up automatically by any binary using clap's `env = "..."` attribute.

## Toolchain (Windows)

- Default `stable-x86_64-pc-windows-gnu` requires MinGW for `dlltool.exe` / `gcc.exe` / `ld.exe`. WinLibs MSVCRT installs via `winget install BrechtSanders.WinLibs.POSIX.MSVCRT`. After installing, open a fresh PowerShell so PATH propagates.
- MSVC toolchain alternative needs VS Build Tools + C++ workload (`Microsoft.VisualStudio.2022.BuildTools` with `--add Microsoft.VisualStudio.Workload.VCTools`).

## Conventions

- **Safety and correctness beat speed.** Real money flows through this code; a wrong number is worse than a slow one. The fast path is fine when it's also clearly correct. When the two genuinely conflict, write the correct version, profile, and only then optimize with a comment explaining why the fast version is still sound.
- **Default to no on new abstractions.** Three similar lines beat one wrong abstraction. Don't introduce a trait, generic, layer, or config option until at least the second concrete use case forces it. Removing premature abstractions is much harder than adding the missing one later.
- **Idiomatic Rust, but clarity beats cleverness.** Use the standard idioms (`?` propagation, iterator chains, `Drop`-as-RAII, trait impls, `Result`/`Option` combinators). Avoid GATs, macro DSLs, type-state encodings, and deep trait hierarchies — these almost always cost more than they save in a codebase this size. A `match` on five variants beats a clever trait dispatcher; an explicit loop beats an opaque `try_fold` when the loop is more readable. The type system earns most of its keep through IDE autocomplete and refactor safety; treat it as a productivity tool, not a proof system.
- **Unit-test the unit-testable.** Pure functions, parsers, signature builders, state machines, math — tests ship in the same change. Skip only when the cost is genuinely disproportionate (full async lifecycle requiring real network) and say so explicitly. **No TDD** — prototype until the domain is understood, *then* write tests. Tests written before the design is clear lock in confusion.
- **Integration tests catch the bugs that matter here.** Serde round-trips on every wire variant; in-process `tokio_tungstenite::accept_async` for end-to-end exercises. Invest there before adding more unit tests of glue code.
- **Invest in the debugger and profiler.** Anyone who tells you "you don't need a debugger" is selling something. Time spent learning `rust-gdb`, the VS debugger, or `samply`/`perf` pays back many times over.
- **Errors**: `thiserror` in libraries (`KalshiError`); `anyhow` in binaries. Don't roll your own error trait.
- **Logging**: `tracing` throughout, especially around I/O and reconnect logic. Binaries init with `tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))`. Heavy logging saves debugger sessions.
- **Protocol types**: serde tagged enums (`#[serde(tag = "type", rename_all = "snake_case")]`).
- **Opaque reference data**: `serde_json::Value`. Kalshi reference fields drift with API churn; typed structs there are mostly busywork.
- **Async**: tokio multi-thread runtime. *No `block_on` inside `Drop` or sync contexts.*
- **One process, one runtime, one binary per strategy.** No microservices, no second async runtime, no distributed framework. When the urge to split arises, ask which concrete problem it solves and whether that problem is real yet.

## Architecture: kalshi-ws

Three tasks share an `Arc<Inner>` inside `Client`:

1. **Writer** — drains `mpsc<ClientCommand>`, serializes JSON, sends on the WS sink.
2. **Multiplexer** — reads frames, dispatches data frames to per-`sid` closures. The closure owns the typed `Sender<T>` directly — no trait-object hierarchy, because that hierarchy adds complexity without paying for it here.
3. **Supervisor** — owns the lifecycle. Reconnects with exponential backoff, replays subscription specs, remaps `sid`, resets per-sid `seq` counter, emits `SystemEvent::Reconnected { sub_id, had_seq_gap }`.

`Subscription<T>: Stream<Item = T>` is the public surface; `Drop` issues a best-effort unsubscribe via `try_send`.

Source under `rust/crates/kalshi-ws/`. Detailed design lives in `~/.claude/plans/resilient-orbiting-gadget.md`.

## Out of scope (don't add without asking)

- REST trading client (order placement). The library is WS-only by design; order placement is a separate concern.
- Backtesting framework. Pick one when a real strategy needs it; don't pre-build.
- WASM / browser support.
- A second async runtime (async-std, smol). Tokio only.
