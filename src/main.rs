use std::collections::HashMap;

use jupiter_arbitrage_bot_offchain::*;
use chrono::Utc;
use futures::StreamExt;
use solana_relayer_adapter_rust::Tips;
use jupiter_arbitrage_bot_offchain::submit_with_services;
use solana_sdk::signer::Signer;
use solana_sdk::system_instruction::advance_nonce_account;
use tokio::time::{interval, Duration};
use tracing::{debug, error, info, warn};
use yellowstone_grpc_client::GeyserGrpcClient;
use yellowstone_grpc_proto::geyser::{
    SubscribeRequest, SubscribeRequestFilterTransactions,
};

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    info!("Using RPC only for transaction submission");

    // Start nonce fetcher
    tokio::spawn(async move {
        loop {
            fetch_nonce().await;
        }
    });

    // Start SOL price fetcher
    tokio::spawn({
        let fallback_price = CONFIG.tx_cost.sol_usd;
        async move {
            start_sol_price_fetcher(fallback_price).await;
        }
    });

    let _hash = get_slot();

    let pubkey = keyfile_status().await;
    info!(pubkey = %pubkey.pubkey(), "Wallet loaded");

    info!(
        big_trades = CONFIG.strategy.watch_flows,
        continuous_polling = CONFIG.strategy.poll_quotes,
        polling_interval_ms = CONFIG.strategy.poll_interval_ms,
        submit_transactions = CONFIG.strategy.live_trading,
        "Arbitrage mode configuration"
    );

    match estimate_jupiter_timing().await {
        Ok(t) => info!(
            quote_ms = t.quote_ms,
            swap_build_ms = t.swap_build_ms,
            total_ms = t.quote_ms + t.swap_build_ms,
            "Jupiter timing estimate"
        ),
        Err(e) => warn!(error = %e, "Jupiter timing estimate skipped"),
    }

    // Start continuous polling if enabled
    if CONFIG.strategy.poll_quotes {
        let polling_interval = CONFIG.strategy.poll_interval_ms;
        tokio::spawn(async move {
            continuous_polling_loop(polling_interval).await;
        });
    }

    // Run big trades monitor if enabled
    if CONFIG.strategy.watch_flows {
        run_big_trades_monitor().await?;
    } else if CONFIG.strategy.poll_quotes {
        info!("Big trades monitor disabled; running continuous polling only");
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    } else {
        warn!("Both modes disabled; enable at least one in settings.toml");
    }

    Ok(())
}

// =============================================================================
// CONTINUOUS POLLING MODE
// =============================================================================

async fn continuous_polling_loop(interval_ms: u64) {
    info!(interval_ms, "Starting continuous polling");
    
    let mut ticker = interval(Duration::from_millis(interval_ms));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    
    // Run first poll immediately, then use interval for subsequent polls
    let mut is_first = true;
    loop {
        // Wait for interval only after first poll
        if !is_first {
            ticker.tick().await;
        }
        is_first = false;
        
        for base_token_config in BASE_TOKENS.iter() {
            let mother_token = base_token_config.mint.clone();
            
            // Look up token info from POPULAR_TOKEN_INFO to get correct decimals and symbol
            let (decimal, symbol) = POPULAR_TOKEN_INFO
                .iter()
                .find(|t| t.mint == mother_token.as_str())
                .map(|t| (t.decimals, t.symbol))
                .unwrap_or_else(|| {
                    // Fallback: Check if it's WSOL/SOL
                    if mother_token == "So11111111111111111111111111111111111111112" {
                        (9, "SOL")
                    } else {
                        // Default to 6 decimals and "UNKNOWN" symbol if not found
                        (6, "UNKNOWN")
                    }
                });
            
            // If mother token is SOL, use stablecoin as target; otherwise use configured target
            let target_tokens = if symbol == "SOL" || symbol == "WSOL" || mother_token == "So11111111111111111111111111111111111111112" {
                // For SOL: Use stablecoins (USDC, USDT) as target tokens
                vec![
                    "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".to_string(), // USDC
                    "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB".to_string(), // USDT
                ]
            } else {
                // For other tokens: Use configured target token
                vec![TARGET_TOKEN.clone()]
            };
            
            let min_range = base_token_config.amount_range[0];
            let max_range = base_token_config.amount_range[1];
            
            let sim_start = std::time::Instant::now();
            let quote_data = simulate_amount_in(
                mother_token.clone(),
                decimal,
                symbol.to_string(),
                target_tokens,
                min_range,
                max_range,
                base_token_config.steps as usize,
                base_token_config.min_profit,
                true, // is_polling_mode = true for polling mode
            )
            .await;
            let sim_elapsed_ms = sim_start.elapsed().as_millis();

            if sim_elapsed_ms > 100 {
                debug!(elapsed_ms = %sim_elapsed_ms, %symbol, "simulate_amount_in slow");
            }

            if !quote_data.is_empty() && CONFIG.strategy.live_trading {
                info!(count = quote_data.len(), %symbol, "Found profitable opportunities");
                
                let best_trade = quote_data.into_iter()
                    .max_by_key(|(in_amt, out_amt, _, _, _, _)| *out_amt as i64 - *in_amt as i64);
                
                if let Some((in_amount, out_amount, in_res, out_res, _, target_token)) = best_trade {
                    // Find target token symbol for logging
                    let target_symbol = POPULAR_TOKEN_INFO
                        .iter()
                        .find(|t| t.mint == target_token.as_str())
                        .map(|t| t.symbol)
                        .unwrap_or_else(|| {
                            // Fallback for common stablecoins
                            match target_token.as_str() {
                                "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v" => "USDC",
                                "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB" => "USDT",
                                _ => "UNKNOWN"
                            }
                        });
                    
                    let in_human = in_amount as f64 / 10_f64.powf(decimal as f64);
                    let out_human = out_amount as f64 / 10_f64.powf(decimal as f64);
                    let profit_human = (out_amount as i64 - in_amount as i64) as f64 / 10_f64.powf(decimal as f64);
                    info!(
                        %symbol,
                        %target_symbol,
                        in = %in_human,
                        out = %out_human,
                        profit = %profit_human,
                        "Best opportunity"
                    );
                    
                    let min_profit = base_token_config.min_profit;
                    let token_is_sol = symbol == "SOL" || symbol == "WSOL" || mother_token == "So11111111111111111111111111111111111111112";
                    let gross_profit_raw = (out_amount as i64 - in_amount as i64);
                    let (total_tx_cost_raw, tip_sol) = jupiter_arbitrage_bot_offchain::engine::runtime::calculate_tx_cost_for_trade(
                        &FEES,
                        gross_profit_raw,
                        token_is_sol,
                        decimal,
                    ).await;
                    let total_tx_cost_in_token = total_tx_cost_raw as f64 / 10_f64.powf(decimal as f64);
                    let real_profit = profit_human - total_tx_cost_in_token;

                    if real_profit >= min_profit {
                        info!(
                            %symbol,
                            in = %in_human,
                            out = %out_human,
                            real_profit = %real_profit,
                            tx_cost = %total_tx_cost_in_token,
                            min_profit = %min_profit,
                            "Submitting trade"
                        );
                        tokio::spawn(async move {
                            submit_polling_trade(in_res, out_res, min_profit, decimal, tip_sol).await;
                        });
                    }
                }
            }
        }
    }
}

/// Submit a trade from polling mode. `tip_sol` is the third-party fee (fixed or profit-based) to attach.
async fn submit_polling_trade(
    in_res: jupiter_swap_api_client::quote::QuoteResponse,
    out_res: jupiter_swap_api_client::quote::QuoteResponse,
    min_profit_amount: f64,
    decimal: u8,
    tip_sol: f64,
) {
    let instr_advance_nonce_account = advance_nonce_account(&NONCE_ADDR, &PUBKEY);
    
    let ix = match get_swap_ix(
        in_res,
        out_res,
        (min_profit_amount * 10_f64.powf(decimal as f64)) as u64,
    ).await {
        Ok(ix) => ix,
        Err(e) => {
            error!(error = %e, "Failed to get swap_ix");
            return;
        }
    };

    let mut raw_swap_ixs = Vec::new();
    raw_swap_ixs.extend(ix.setup_instructions);
    raw_swap_ixs.push(ix.swap_instruction);

    let nonce_data = get_nonce();
    let recent_blockhash = nonce_data.blockhash();
    let alts = fetch_alt(ix.address_lookup_table_addresses).await;

    let service_desc = if jupiter_arbitrage_bot_offchain::use_low_latency_submission() {
        "low-latency (Jito/Helius/etc.)"
    } else {
        "RPC"
    };
    info!(
        service = %service_desc,
        time = %Utc::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        "Submitting transaction"
    );

    submit_with_services(
        Tips {
            tip_sol_amount: tip_sol,
            tip_addr_idx: 0,
            cu: Some(FEES.compute_units),
            priority_fee_micro_lamport: Some(FEES.priority_lamports),
            payer: *PUBKEY,
            pure_ix: raw_swap_ixs,
        },
        &*SIGNERS,
        recent_blockhash,
        instr_advance_nonce_account,
        alts,
        1,
    )
    .await;

    info!(service = %service_desc, "Transaction submitted");
}

// =============================================================================
// BIG TRADES MONITOR MODE
// =============================================================================

async fn run_big_trades_monitor() -> Result<(), anyhow::Error> {
    let yellowstone_endpoint = YELLOWSTONE_GRPC_ENDPOINT.as_ref()
        .ok_or_else(|| anyhow::anyhow!("yellowstone_grpc_endpoint not configured"))?;
    let yellowstone_token = YELLOWSTONE_GRPC_TOKEN.as_ref()
        .ok_or_else(|| anyhow::anyhow!("yellowstone_grpc_token not configured"))?;

    let endpoint_url = yellowstone_endpoint
        .strip_prefix("http://")
        .or_else(|| yellowstone_endpoint.strip_prefix("https://"))
        .unwrap_or(yellowstone_endpoint);
    
    let (host, port) = if let Some((h, p)) = endpoint_url.split_once(':') {
        (h, p.parse::<u16>().unwrap_or(10001))
    } else {
        (endpoint_url, 10001)
    };

    info!(%host, %port, "Connecting to Yellowstone gRPC");

    loop {
        info!("Connecting and subscribing to Yellowstone");

        let endpoint = format!("http://{}:{}", host, port);
        let builder = match GeyserGrpcClient::build_from_shared(endpoint) {
            Ok(b) => b,
            Err(e) => {
                error!(error = ?e, "Yellowstone builder error");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        let builder = match builder.x_token(Some(yellowstone_token.clone())) {
            Ok(b) => b,
            Err(e) => {
                error!(error = ?e, "Yellowstone X-Token error");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        let mut client = match builder.connect().await {
            Ok(c) => {
                info!("Yellowstone connected");
                c
            }
            Err(e) => {
                error!(error = ?e, "Yellowstone connection error");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        let mut transactions_map = HashMap::new();
        for (idx, base_token) in BASE_TOKENS.iter().enumerate() {
            let filter = SubscribeRequestFilterTransactions {
                vote: Some(false),
                failed: Some(false),
                account_include: vec![base_token.mint.clone()],
                account_exclude: vec![],
                account_required: vec![],
                signature: None,
            };
            transactions_map.insert(format!("tx_{}", idx), filter);
        }

        let request = SubscribeRequest {
            slots: HashMap::new(),
            accounts: HashMap::new(),
            transactions: transactions_map,
            transactions_status: HashMap::new(),
            blocks: HashMap::new(),
            blocks_meta: HashMap::new(),
            accounts_data_slice: vec![],
            commitment: Some(0),
            ping: None,
            entry: HashMap::new(),
            from_slot: None,
        };

        let (_sink, mut stream) = match client.subscribe_with_request(Some(request)).await {
            Ok((sink, stream)) => {
                info!("Yellowstone subscribed");
                (sink, stream)
            }
            Err(e) => {
                error!(error = ?e, "Yellowstone subscribe error");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        loop {
            match stream.next().await {
                Some(Ok(update)) => {
                    tokio::spawn(async move {
                        process_single_trade_yellowstone(update).await;
                    });
                }
                Some(Err(e)) => {
                    error!(error = ?e, "Yellowstone stream error");
                    break;
                }
                None => {
                    info!("Yellowstone stream ended");
                    break;
                }
            }
        }
    }
}
