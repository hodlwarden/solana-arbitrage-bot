# Solana Arbitrage Bot

**A Solana arbitrage bot** built in Rust. Discovers and executes profitable DEX swap opportunities on Solana via the [Jupiter](https://jupiter.ag) aggregator. RPC-only execution (no Jito/BloxRoute). Supports **continuous quote polling** and optional **Yellowstone gRPC** big-trade monitoring.

*Keywords: solana arbitrage bot, Solana arbitrage, Jupiter arbitrage bot, DEX arbitrage Rust, Solana trading bot, Yellowstone gRPC, Jupiter API.*

---

## Test Results

[$77 -> $0.006 Profit](https://solscan.io/tx/4ASCHbwF2q3ZeeKJgcUx93mtTwHHYwu29bmerU3KJPmGupMziqFvnQScuam8Yx4e458TSRwd9QhxC1HSiHT6EZLc#balance_change)

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
| `[fees]`      | `compute_unit_limit`, `priority_fee_lamports`, `relay_tip_sol`; optional `sol_price_usd` fallback. |

Legacy key names (e.g. `[credential]`, `wallet_path`, `base_tokens`, `live_trading`) are still accepted via aliases.

---

## Project Layout

| Path        | Description |
|------------|-------------|
| `src/app/` | Configuration and runtime settings (node, swap API, strategy, fees). |
| `src/chain/` | Chain data and constants (program maps, token info, fee constants). |
| `src/engine/` | Arbitrage engine: Jupiter integration, discovery (polling + big-trades), execution, runtime (nonce, blockhash, SOL price, fee cost). |

---

## How It Works

1. **Discovery**
   - **Polling:** On a timer, for each configured base token, the bot sweeps a notional range (e.g. 10–600 USDC) in a grid, requests Jupiter quotes (e.g. base → USDC/SOL), and keeps opportunities above the minimum profit after fees.
   - **Big-trades:** If enabled, a Yellowstone gRPC subscription filters transactions touching configured token mints; large flows trigger quote simulation and optional execution.
2. **Execution**
   - Builds swap instructions via the Jupiter API, advances the nonce, then submits the transaction through the configured RPC/submit endpoint with the requested compute units and priority fee.

---

## Contact

Telegram: [Hodlwarden](https://t.me/hodlwarden)
