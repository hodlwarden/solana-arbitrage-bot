# Solana Arbitrage Bot

**A Solana arbitrage bot** built in Rust. Discovers and executes profitable DEX swap opportunities on Solana via the [Jupiter](https://jupiter.ag) aggregator. RPC-only execution (no Jito/BloxRoute). Supports **continuous quote polling** and optional **Yellowstone gRPC** big-trade monitoring.

*Keywords: solana arbitrage bot, Solana arbitrage, Jupiter arbitrage bot, DEX arbitrage Rust, Solana trading bot, Yellowstone gRPC, Jupiter API.*

**Contact:** [Telegram — t.me/hodlwarden](https://t.me/hodlwarden)

### How it works

1. **Discovery** — **Polling:** sweeps a notional range in a grid, requests Jupiter quotes, keeps opportunities above min profit after fees. **Big-trades:** optional Yellowstone gRPC subscription triggers quote simulation on large flows.
2. **Execution** — Builds swap instructions via Jupiter API, advances nonce, submits via RPC with requested compute and priority fee.

**Workflow:**

![Architecture: Config → Discovery → Jupiter Quotes → Fee check → Execution](images/architecture-diagram.png)

**Profit calculation** (execute only when net profit ≥ min profit):

![Profit calculation: notional grid, gross profit, tx cost, net profit, execute if ≥ min_profit](images/profit-calculation-flow.png)

---

## Test Results

**$0.006 Profit** - 
[$77 -> $0.006 Profit](https://solscan.io/tx/4ASCHbwF2q3ZeeKJgcUx93mtTwHHYwu29bmerU3KJPmGupMziqFvnQScuam8Yx4e458TSRwd9QhxC1HSiHT6EZLc#balance_change)

**$0.011 Profit** - 
[$77 -> $0.011 Profit](https://solscan.io/tx/4uQ4sANAv6oGoBeqE28T7CNQ1fDMX7EsduA87yhhBwpVXGyspVwHokkGa9oC11UEY7Kw6DK5sdWngHgDC7hz9GAS#balance_change)

---

## Features

- **Dual discovery modes**
  - **Continuous polling** — Periodically fetches Jupiter quotes across configurable amount ranges and tokens.
  - **Big-trades monitor** — Subscribes to Yellowstone gRPC for large on-chain flows and reacts with quote simulation.
- **RPC-only execution** — All transactions sent via your RPC/submit endpoint (no bundled relayer).
- **Multi-token support** — Configure base tokens (e.g. USDC, SOL) with notional ranges, grid steps, and min-profit thresholds.
- **Transaction cost awareness** — Estimates fee (compute, priority, tip) and SOL price to filter only profitable trades.
- **Nonce-based submission** — Uses a durable nonce account for reliable transaction lifecycle.

---

## Prerequisites

- **Rust** (stable, e.g. 1.70+): [rustup](https://rustup.rs)
- **Solana RPC** — A node or provider (e.g. Helius, QuickNode, Triton) with `submitTransaction` support.
- **Wallet** — Keypair file for the bot and a funded **nonce account**.
- **Jupiter API** — Either the public Jupiter API or a self-hosted proxy; configurable in `Config.toml`.
- **Yellowstone gRPC** (optional) — Only if you enable big-trades monitoring; requires endpoint and auth token.

---

## Quick Start

1. **Clone and build**

   ```bash
   git clone https://github.com/hodlwarden/solana-arbitrage-bot.git solana-arbitrage-bot && cd solana-arbitrage-bot
   cargo build --release
   ```

2. **Configure**

   Copy the included `Config.toml` to a private file (e.g. `settings.toml`) or edit in place. **Do not commit secrets.** Set at least:

   - `signer_keypair_path`, `rpc_endpoint`, `submit_endpoint`
   - `dex_api` endpoint (Jupiter API or proxy)
   - `nonce_account_pubkey`, `instruments`, and `[fees]`

   The app loads `settings.toml` first, then falls back to `Config.toml`.

3. **Run**

   ```bash
   cargo run --release
   # Or: ./target/release/jupiter_arbitrage_bot_offchain  # if the binary name matches
   ```

   Set `RUST_LOG=info` (or `debug`) to control log level.

---

## Configuration

Configuration is TOML-based. Example structure (see `Config.toml` in the repo for full reference):

| Section       | Purpose |
|---------------|---------|
| `[connection]` | `signer_keypair_path`, `rpc_endpoint`, `submit_endpoint`; optional `geyser_endpoint`, `geyser_auth_token` for Yellowstone. |
| `[dex_api]`   | Jupiter API `endpoint` and optional `auth_token`. |
| `[strategy]`  | `instruments` (base tokens with mint, notional range, grid steps, min profit), `nonce_account_pubkey`, `default_quote_mint`, `polling_enabled` / `poll_interval_ms`, `geyser_watch_enabled`, `execution_enabled`. |
| `[fees]`      | `compute_unit_limit`, `priority_fee_lamports`, `relay_tip_sol`; optional `third_party_fee_profit_pct` (e.g. `0.5` = 50% of gross profit in SOL); optional `sol_price_usd` fallback. |

### Third-party fee (fixed vs profit-based)

Transaction cost includes a **base network fee** plus an optional **third-party fee** (e.g. relay/tip). You can set that third-party fee in two ways:

- **Fixed** — A constant amount in SOL per trade. Use `relay_tip_sol` (or alias `tip_sol` / `third_party_fee`).
- **Profit-based** — A fraction of the trade’s **gross profit in SOL**. Use `third_party_fee_profit_pct` (0.0–1.0). When set, the third-party fee is computed as **gross profit (in SOL) × this value**, and that amount is attached as the tip when submitting the transaction.

**Example (profit-based):**  
If gross profit is **0.1 SOL** and you set `third_party_fee_profit_pct = 0.5`, the third-party fee is **0.05 SOL**. Net profit (after base fee and this tip) is then used to decide if the trade meets `min_profit` and is submitted.

**How it works:**

1. **Discovery** — For each candidate trade, the bot converts gross profit to SOL (using SOL price for non-SOL tokens), then computes total tx cost = base fee + third-party fee (fixed or `gross_profit_sol × third_party_fee_profit_pct`). Only trades with **net profit ≥ min_profit** are kept.
2. **Execution** — When submitting, the bot sends exactly the computed third-party fee (the same value used in the profitability check) as the tip, so relay/third-party gets the configured share of profit.

**Config examples:**

```toml
# Fixed third-party fee: 0.00001 SOL per trade (no third_party_fee_profit_pct)
[fees]
relay_tip_sol = 0.00001
```

```toml
# Profit-based: 50% of gross profit in SOL as third-party fee (e.g. 0.1 SOL profit → 0.05 SOL fee)
[fees]
relay_tip_sol = 0.00001   # fallback when profit-based is 0 or not used
third_party_fee_profit_pct = 0.5
```

If `third_party_fee_profit_pct` is set and in range (0, 1], it overrides `relay_tip_sol` for that trade’s third-party fee; otherwise `relay_tip_sol` is used. Profit-based fee scales with opportunity size, so you can share a percentage of each trade’s profit with a relay or service instead of a fixed amount.

---

Legacy key names (e.g. `[credential]`, `wallet_path`, `base_tokens`, `live_trading`) are still accepted via aliases.

---

## Project Layout

| Path        | Description |
|------------|-------------|
| `src/app/` | Configuration and runtime settings (node, swap API, strategy, fees). |
| `src/chain/` | Chain data and constants (program maps, token info, fee constants). |
| `src/engine/` | Arbitrage engine: Jupiter integration, discovery (polling + big-trades), execution, runtime (nonce, blockhash, SOL price, fee cost). |

