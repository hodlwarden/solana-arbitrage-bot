use std::time::Instant;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;

use futures::future::join_all;
use jupiter_swap_api_client::quote::QuoteResponse;

use crate::*;

// Mutex to ensure thread-safe file writes
static LOG_MUTEX: Mutex<()> = Mutex::new(());

/// Write a log message to logs.txt (thread-safe, opens file in append mode for each write)
fn write_log(message: &str) {
    let _guard = LOG_MUTEX.lock().unwrap();
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open("logs.txt")
    {
        if writeln!(file, "{}", message).is_ok() {
            // Flush immediately to ensure logs are written
            let _ = file.flush();
        }
    }
    // Silently fail if file can't be opened - don't spam console
}

pub async fn simulate_amount_in(
    mother_token: String,
    mother_token_decimal: u8,
    mother_token_symbol: String,
    target_tokens: Vec<String>,
    from_f: f64,
    to_f: f64,
    steps: usize,
    min_profit_amount: f64,
    is_polling_mode: bool,
) -> Vec<(u64, u64, QuoteResponse, QuoteResponse, u128, String)> {
    let sim_start = Instant::now();
    let ratio = (to_f / from_f).powf(1.0 / (steps as f64 - 1.0));

    let min_profit_amount = (min_profit_amount * 10_f64.powf(mother_token_decimal as f64)) as u64;

    let amounts: Vec<u64> = (0..steps)
        .map(|i| {
            let sol = from_f * ratio.powi(i as i32);
            (sol * 10_f64.powf(mother_token_decimal as f64)) as u64
        })
        .collect();

    // Create futures for all combinations of amounts and target tokens
    let mut futures = Vec::new();
    for &in_amount in &amounts {
        for output_token in &target_tokens {
            let mother_token = mother_token.clone();
            let output_token = output_token.clone();
            let is_polling = is_polling_mode;
            futures.push(async move {
                let start = Instant::now();
                let quote_result = if is_polling {
                    get_quote_polling(in_amount, &mother_token, &output_token).await
                } else {
                    get_quote_big_trade(in_amount, &mother_token, &output_token).await
                };
                match quote_result {
                    Ok((in_amount, out_amount, in_res, out_res)) => {
                        let elapsed = start.elapsed().as_micros();
                        Ok::<_, anyhow::Error>((in_amount, out_amount, in_res, out_res, elapsed, output_token))
                    }
                    Err(e) => {
                        // Silently fail for individual quotes - we'll log summary
                        Err(e)
                    }
                }
            });
        }
    }

    let results: Vec<Result<(u64, u64, QuoteResponse, QuoteResponse, u128, String), anyhow::Error>> =
        join_all(futures).await;

    let sim_elapsed_ms = sim_start.elapsed().as_millis();
    write_log(&format!(
        "[SIMULATE] ⏱ simulate_amount_in took {} ms ({} steps × {} targets)",
        sim_elapsed_ms,
        steps,
        target_tokens.len()
    ));

    let total_quotes = results.len();
    let ok_results: Vec<(u64, u64, QuoteResponse, QuoteResponse, u128, String)> =
        results.into_iter().filter_map(|r| r.ok()).collect();
    
    let successful_quotes = ok_results.len();
    let failed_quotes = total_quotes - successful_quotes;
    
    if failed_quotes > 0 {
        write_log(&format!(
            "[SIMULATE] ⚠️ {} quotes failed out of {} total",
            failed_quotes,
            total_quotes
        ));
    }

    let sol_price = crate::engine::runtime::sol_price::get_sol_price_usdc(FEES.sol_usd).await;
    let token_is_sol = mother_token == "So11111111111111111111111111111111111111112";

    // Log all trades (both profitable and unprofitable) and filter profitable ones
    let mut profitable_trades: Vec<(u64, u64, QuoteResponse, QuoteResponse, u128, String)> = Vec::new();
    let mut unprofitable_count = 0;
    
    for (in_amount, out_amount, in_res, out_res, elapsed, target_token) in ok_results {
        let gross_profit = out_amount as i64 - in_amount as i64;
        // Transaction cost can be fixed or profit-based (third_party_fee_profit_pct)
        let (total_tx_cost, _tip_sol) = crate::engine::runtime::calculate_tx_cost_for_trade_with_sol_price(
            &FEES,
            gross_profit,
            token_is_sol,
            mother_token_decimal,
            sol_price,
        );
        let net_profit = gross_profit - total_tx_cost;
        let profit_after_min = net_profit - min_profit_amount as i64;
        
        // Find target token symbol for better logging
        let target_symbol = POPULAR_TOKEN_INFO
            .iter()
            .find(|t| t.mint == target_token)
            .map(|t| t.symbol)
            .unwrap_or("UNKNOWN");
        
        // Convert amounts to human-readable format
        let in_human = in_amount as f64 / 10_f64.powf(mother_token_decimal as f64);
        let out_human = out_amount as f64 / 10_f64.powf(mother_token_decimal as f64);
        let gross_profit_human = gross_profit as f64 / 10_f64.powf(mother_token_decimal as f64);
        let net_profit_human = net_profit as f64 / 10_f64.powf(mother_token_decimal as f64);
        let min_profit_human = min_profit_amount as f64 / 10_f64.powf(mother_token_decimal as f64);
        let tx_cost_human = total_tx_cost as f64 / 10_f64.powf(mother_token_decimal as f64);
        
        if profit_after_min > 0 {
            // Profitable trade
            write_log(&format!(
                "[SIMULATE] ✅ Profitable: {} -> {} -> {}: in={:.6} {}, out={:.6} {}, gross_profit={:.6} {}, net_profit={:.6} {} (tx_cost={:.6} {}, min_required={:.6} {})",
                mother_token_symbol,
                target_symbol,
                mother_token_symbol,
                in_human, mother_token_symbol,
                out_human, mother_token_symbol,
                gross_profit_human, mother_token_symbol,
                net_profit_human, mother_token_symbol,
                tx_cost_human, mother_token_symbol,
                min_profit_human, mother_token_symbol
            ));
            
            profitable_trades.push((in_amount, out_amount, in_res, out_res, elapsed, target_token));
        } else {
            // Unprofitable trade - log it too
            unprofitable_count += 1;
            // write_log(&format!(
            //     "[SIMULATE] ❌ Unprofitable: {} -> {} -> {}: in={:.6} {}, out={:.6} {}, gross_profit={:.6} {}, net_profit={:.6} {} (tx_cost={:.6} {}, min_required={:.6} {}, shortfall={:.6} {})",
            //     mother_token_symbol,
            //     target_symbol,
            //     mother_token_symbol,
            //     in_human, mother_token_symbol,
            //     out_human, mother_token_symbol,
            //     gross_profit_human, mother_token_symbol,
            //     net_profit_human, mother_token_symbol,
            //     tx_cost_human, mother_token_symbol,
            //     min_profit_human, mother_token_symbol,
            //     -profit_after_min as f64 / 10_f64.powf(mother_token_decimal as f64), mother_token_symbol
            // ));
        }
    }
    
    // Always log summary with all results
    // write_log(&format!(
    //     "[SIMULATE] 📊 Summary: {} successful quotes, {} profitable trades, {} unprofitable trades",
    //     successful_quotes,
    //     profitable_trades.len(),
    //     unprofitable_count
    // ));
    
    profitable_trades
}
