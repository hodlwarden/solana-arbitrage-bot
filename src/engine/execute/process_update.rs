use chrono::Utc;
use futures::{Stream, StreamExt, future::join_all};
// Temporarily disabled: use helius_laserstream::SubscribeUpdate;
use solana_relayer_adapter_rust::Tips;
use crate::submit_with_services;
use solana_sdk::{
    system_instruction::advance_nonce_account,
    transaction::Transaction,
    signature::Signer,
    bs58,
};
use yellowstone_grpc_proto::prelude::SubscribeUpdate;

use std::time::Instant;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;

use crate::*;

// Mutex to ensure thread-safe file writes for big trades and submissions
static BIG_TRADE_LOG_MUTEX: Mutex<()> = Mutex::new(());

/// Write a log message to big_trades.txt (thread-safe, opens file in append mode for each write)
fn write_big_trade_log(message: &str) {
    let _guard = BIG_TRADE_LOG_MUTEX.lock().unwrap();
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open("big_trades.txt")
    {
        if writeln!(file, "{}", message).is_ok() {
            let _ = file.flush();
        }
    }
}

/// Process a single trade update asynchronously Lil Jit
/// Temporarily disabled - needs to be refactored for Yellowstone gRPC
// async fn process_single_trade(sub_update: SubscribeUpdate) {
pub async fn process_single_trade_yellowstone(sub_update: yellowstone_grpc_proto::prelude::SubscribeUpdate) {
    // Only process transaction updates (ignore account, slot, block, ping, etc.)
    match &sub_update.update_oneof {
        Some(yellowstone_grpc_proto::geyser::subscribe_update::UpdateOneof::Transaction(_)) => {
            // This is a transaction update, continue processing
        },
        _ => return, // Not a transaction update, skip
    };
    
    let (mother_token, changes, program_names, unique_tokens, tx_id) =
        match extract_big_trade(&sub_update) {
            Some(data) => data,
            None => {
                // extract_big_trade filters out trades that don't meet criteria
                // (threshold, program names, token changes, etc.)
                return;
            },
        };

    // Log big trade discovery details (file + console), easy to read
    let timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let mother_symbol = &mother_token.5;
    let mother_mint = &mother_token.0;
    let min_profit = mother_token.4;
    let change_lines: Vec<String> = changes
        .iter()
        .map(|c| {
            let sym = POPULAR_TOKEN_INFO
                .iter()
                .find(|t| t.mint == c.mint)
                .map(|t| t.symbol)
                .unwrap_or("?");
            format!("    {}  delta: {:+.6}  pre: {:.6}  post: {:.6}", sym, c.delta, c.pre_balance, c.post_balance)
        })
        .collect();
    let programs_short = program_names.join(", ");
    let log_block = format!(
        "[{}] [BIG_TRADE_DISCOVERED]\n  tx_id:     {}\n  mother:    {} ({})\n  min_profit: {:.6}\n  changes:\n{}\n  programs:  {}",
        timestamp,
        tx_id,
        mother_symbol,
        mother_mint,
        min_profit,
        change_lines.join("\n"),
        programs_short
    );
    write_big_trade_log(&log_block);
    // println!("{}\n", log_block);

    let simulate_start = Instant::now();

    let min_range = mother_token
        .2
        .iter()
        .copied()
        .reduce(f64::min)
        .unwrap_or(0.0);
    let max_range = mother_token
        .2
        .iter()
        .copied()
        .reduce(f64::max)
        .unwrap_or(0.0);

    // Use only tokens that appeared in the big trade (no POPULAR_TOKEN_INFO) to reduce quote count
    let target_tokens: Vec<String> = unique_tokens.clone();

    // Run simulation with original input amount range (big trade mode)
    let mut quote_data = simulate_amount_in(
        mother_token.clone().0,
        mother_token.clone().1,
        mother_token.clone().5,
        target_tokens.clone(),
        min_range,
        max_range,
        mother_token.3 as usize,
        mother_token.4,
        false, // is_polling_mode = false for big trade mode
    )
    .await;

    // If submit_transactions is enabled, run additional simulation with larger amounts (1-5000 USDC)
    if CONFIG.strategy.live_trading {
        let quote_data_large = simulate_amount_in(
            mother_token.clone().0,
            mother_token.clone().1,
            mother_token.clone().5,
            target_tokens,
            min_range,
            max_range,
            mother_token.3 as usize,
            mother_token.4,
            false, // is_polling_mode = false for big trade mode
        )
        .await;
        
        // Combine results from both simulations
        quote_data.extend(quote_data_large);
    }
    let simulate_elapsed = simulate_start.elapsed();
    
    // If only simulating (not submitting), return early after simulation
    if !CONFIG.strategy.live_trading {
        return;
    }
    
    if quote_data.is_empty() {
        return;
    }

    let sol_price = crate::engine::runtime::get_sol_price_usdc(FEES.sol_usd).await;
    let token_is_sol = mother_token.0 == "So11111111111111111111111111111111111111112";

    // Per-trade tx cost (and tip for submission) when using profit-based third-party fee
    let with_costs: Vec<(u64, u64, _, _, u128, String, i64, f64)> = quote_data
        .into_iter()
        .map(|(in_amount, out_amount, in_res, out_res, elapsed, target_token)| {
            let gross_profit = out_amount as i64 - in_amount as i64;
            let (total_tx_cost, tip_sol) =
                crate::engine::runtime::calculate_tx_cost_for_trade_with_sol_price(
                    &FEES,
                    gross_profit,
                    token_is_sol,
                    mother_token.1,
                    sol_price,
                );
            (in_amount, out_amount, in_res, out_res, elapsed, target_token, total_tx_cost, tip_sol)
        })
        .collect();

    let best = with_costs
        .into_iter()
        .max_by_key(|(in_amount, out_amount, _, _, _, _, total_tx_cost, _)| {
            *out_amount as i64 - *in_amount as i64 - *total_tx_cost
        });
    let (in_amount, out_amount, in_res, out_res, _elapsed, _target_token, total_tx_cost, tip_sol_for_submit) =
        match best {
            Some(t) => t,
            None => return,
        };

    let gross_profit = out_amount as i64 - in_amount as i64;
    let net_profit = gross_profit - total_tx_cost;
    let pow_dec = 10_f64.powf(mother_token.1 as f64);
    let total_tx_cost_usdc =
        (total_tx_cost as f64 / pow_dec) * if token_is_sol { sol_price } else { 1.0 };
    
    // Log big trade with profitable opportunity found
    let timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let token_symbol = mother_token.5.clone();
    let token_mint = mother_token.0.clone();
    
    // Calculate total delta for the mother token
    let total_delta: f64 = changes.iter()
        .filter(|c| c.mint == token_mint)
        .map(|c| c.delta.abs())
        .sum();
    
    // Format programs and unique_tokens as comma-separated strings
    let programs_str = program_names.join(", ");
    let unique_tokens_str = unique_tokens.join(", ");
    
    let big_trade_log = format!(
        "[{}] [BIG_TRADE] Profitable opportunity found: token={} ({}), tx_id={}, delta={:.6}, programs=[{}], unique_tokens=[{}], in_amount={}, out_amount={}, gross_profit={}, net_profit={}",
        timestamp,
        token_symbol,
        token_mint,
        tx_id,
        total_delta,
        programs_str,
        unique_tokens_str,
        in_amount,
        out_amount,
        gross_profit,
        net_profit
    );
    println!("{}", big_trade_log);
    write_big_trade_log(&big_trade_log);

    // Human-readable submit log (token units + USD tx cost from SOL price)
    let dec = mother_token.1;
    let pow = 10_f64.powf(dec as f64);
    let in_human = in_amount as f64 / pow;
    let out_human = out_amount as f64 / pow;
    let gross_human = gross_profit as f64 / pow;
    let net_human = net_profit as f64 / pow;
    let tx_cost_human = total_tx_cost as f64 / pow;
    println!(
        "[{}] [SUBMIT] 🚀 Submitting most profitable trade:\n  in={:.6} {}  out={:.6} {}  gross_profit={:.6} {}  net_profit={:.6} {}  tx_cost={:.6} {} (≈ ${:.4} USD)",
        timestamp,
        in_human, token_symbol,
        out_human, token_symbol,
        gross_human, token_symbol,
        net_human, token_symbol,
        tx_cost_human, token_symbol,
        total_tx_cost_usdc
    );
    
    // Capture values for logging
    let log_in_amount = in_amount;
    let log_out_amount = out_amount;
    let log_total_tx_cost = total_tx_cost;
    let log_tx_id = tx_id.clone();
    let log_mother_token_symbol = mother_token.5.clone();
    
    let tip_sol_submit = tip_sol_for_submit;
    let tasks = std::iter::once((in_amount, out_amount, in_res, out_res, _elapsed, _target_token))
        .map(|(_in_amount, _out_amount, in_res, out_res, _elapsed, _target_token)| {
            let log_in_amount = log_in_amount;
            let log_out_amount = log_out_amount;
            let log_total_tx_cost = log_total_tx_cost;
            let log_tx_id = log_tx_id.clone();
            let log_mother_token_symbol = log_mother_token_symbol.clone();
            let tip_sol_amount = tip_sol_submit;

            tokio::spawn(async move {
                let instr_advance_nonce_account = advance_nonce_account(&NONCE_ADDR, &PUBKEY);
                let ix = get_swap_ix(
                    in_res,
                    out_res,
                    (mother_token.4 * 10_f64.powf(mother_token.1 as f64)) as u64,
                )
                .await
                .unwrap();

                let mut raw_swap_ixs = Vec::new();
                raw_swap_ixs.extend(ix.setup_instructions);
                raw_swap_ixs.push(ix.swap_instruction);

                let nonce_data = get_nonce();
                let recent_blockhash = nonce_data.blockhash();

                let alts = fetch_alt(ix.address_lookup_table_addresses).await;

                // Build transaction to get signature before submitting
                // Note: This builds a simplified version for signature calculation
                // The actual transaction built by ultra_submit may differ slightly
                let mut all_instructions = vec![instr_advance_nonce_account.clone()];
                all_instructions.extend(raw_swap_ixs.clone());
                
                let mut tx = Transaction::new_with_payer(&all_instructions, Some(&PUBKEY));
                tx.sign(&**SIGNERS, recent_blockhash);
                let submitted_tx_signature = bs58::encode(tx.signatures[0]).into_string();

                let service_name = if crate::use_low_latency_submission() {
                    "low-latency"
                } else {
                    "RPC"
                };

                submit_with_services(
                    Tips {
                        tip_sol_amount,
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
                
                let timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S%.3f");
                let submit_msg = format!(
                    "[{}] [SUBMIT] ✅ Transaction submission completed via {}",
                    timestamp,
                    service_name
                );
                println!("{}", submit_msg);
                
                // Log successful submission with details including submitted tx signature
                let gross_profit = log_out_amount as i64 - log_in_amount as i64;
                let net_profit = gross_profit - log_total_tx_cost;
                let submission_log = format!(
                    "[{}] [SUBMIT_SUCCESS] token={}, in_amount={}, out_amount={}, gross_profit={}, net_profit={}, tx_cost={}, service={}, original_tx_id={}, submitted_tx_signature={}",
                    timestamp,
                    log_mother_token_symbol,
                    log_in_amount,
                    log_out_amount,
                    gross_profit,
                    net_profit,
                    log_total_tx_cost,
                    service_name,
                    log_tx_id,
                    submitted_tx_signature
                );
                write_big_trade_log(&submission_log);
            })
        });

    join_all(tasks).await;
    
    /* Original Helius Laserstream code (for reference):
    let (mother_token, _changes, _program_names, unique_tokens, _tx_id) =
        match extract_big_trade(&sub_update) {
            Some(data) => data,
            _ => return,
        };

    let simulate_start = Instant::now();

    let simulate_start = Instant::now();

    let min_range = mother_token
        .2
        .iter()
        .copied()
        .reduce(f64::min)
        .unwrap_or(0.0);
    let max_range = mother_token
        .2
        .iter()
        .copied()
        .reduce(f64::max)
        .unwrap_or(0.0);

    // Combine unique tokens from transaction with popular tokens for more opportunities
    let mut target_tokens: Vec<String> = unique_tokens.clone();
    // Add all popular tokens, excluding mother token and duplicates
    for token_info in POPULAR_TOKEN_INFO.iter() {
        let mint = token_info.mint.to_string();
        if mint != mother_token.0 && !target_tokens.contains(&mint) {
            target_tokens.push(mint);
        }
    }

    // Run simulation with original input amount range
    let mut quote_data = simulate_amount_in(
        mother_token.clone().0,
        mother_token.clone().1,
        mother_token.clone().5,
        target_tokens.clone(),
        min_range,
        max_range,
        mother_token.3 as usize,
        mother_token.4,
    )
    .await;

    // If submit_transactions is enabled, run additional simulation with larger amounts (1-5000 USDC)
    if CONFIG.strategy.live_trading {
        let quote_data_large = simulate_amount_in(
            mother_token.clone().0,
            mother_token.clone().1,
            mother_token.clone().5,
            target_tokens,
            1.0,  // Start from 1 USDC
            5000.0,  // Up to 5000 USDC
            mother_token.3 as usize,
            mother_token.4,
        )
        .await;
        
        // Combine results from both simulations
        quote_data.extend(quote_data_large);
    }
    let simulate_elapsed = simulate_start.elapsed();
    // Temporarily disabled: timestamped log
    // println!(
    //     "[{}] ⏱ Simulate took {} µs | Found {} profitable opportunities",
    //     Utc::now().format("%Y-%m-%d %H:%M:%S%.3f"),
    //     simulate_elapsed.as_micros(),
    //     quote_data.len()
    // );
    
    // If only simulating (not submitting), return early after simulation
    if !CONFIG.strategy.live_trading {
        return;
    }
    
    if quote_data.is_empty() {
        // Temporarily disabled: timestamped log
        // println!(
        //     "[{}] ⚠️ No profitable trades found. Check min_profit_amount threshold (current: {})",
        //     Utc::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        //     mother_token.4
        // );
        return;
    }
    
    // Temporarily disabled: timestamped log
    // println!(
    //     "[{}] ✅ Submitting {} arbitrage transactions...",
    //     Utc::now().format("%Y-%m-%d %H:%M:%S%.3f"),
    //     quote_data.len()
    // );

    // Select only the most profitable trade (highest profit)
    let most_profitable = quote_data
        .into_iter()
        .max_by_key(|(in_amount, out_amount, _, _, _, _)| {
            *out_amount as i64 - *in_amount as i64
        });
    
    let most_profitable = match most_profitable {
        Some(trade) => trade,
        None => return, // Should not happen since we checked empty above
    };
    
    let tasks = std::iter::once(most_profitable)
        .map(|(in_amount, out_amount, in_res, out_res, _elapsed, _target_token)| {
            tokio::spawn(async move {
                let instr_advance_nonce_account = advance_nonce_account(&NONCE_ADDR, &PUBKEY);
                let ix = get_swap_ix(
                    in_res,
                    out_res,
                    (mother_token.4 * 10_f64.powf(mother_token.1 as f64)) as u64,
                )
                .await
                .unwrap();

                let mut raw_swap_ixs = Vec::new();
                raw_swap_ixs.extend(ix.setup_instructions);
                raw_swap_ixs.push(ix.swap_instruction);

                let nonce_data = get_nonce();
                let recent_blockhash = nonce_data.blockhash();

                let alts = fetch_alt(ix.address_lookup_table_addresses).await;

                ultra_submit(
                    Tips {
                        tip_sol_amount: FEES.tip_sol,
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
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .await;
            })
        });

    join_all(tasks).await;
    */
}

// Temporarily disabled: Helius Laserstream version
// pub async fn process_updates<S, E>(mut stream: S) -> Result<(), Box<dyn std::error::Error>>
// where
//     S: Stream<Item = Result<SubscribeUpdate, E>> + Unpin,
//     E: std::error::Error + Send + Sync + 'static,
// {
//     while let Some(update) = stream.next().await {
//         match update {
//             Ok(sub_update) => {
//                 // Spawn each trade processing in a separate task for parallel execution
//                 tokio::spawn(async move {
//                     process_single_trade(sub_update).await;
//                 });
//             }
//             Err(e) => {
//                 // Temporarily disabled: timestamped log
//                 // println!(
//                 //     "[{}] stream error: {}",
//                 //     Utc::now().format("%Y-%m-%d %H:%M:%S%.3f"),
//                 //     e
//                 // );
//             }
//         }
//     }
//
//     Ok(())
// }

// Yellowstone gRPC version - processing is now done directly in main.rs
// This function is kept for potential future use but currently not used
