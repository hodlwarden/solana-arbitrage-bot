//! Transaction cost calculation using SOL price and tx cost config.

use crate::app::config::TxCostConfig;
use crate::chain::TRANSACTION_FEE;

use super::sol_price::SOL_PRICE;

const BASE_TX_FEE_SOL: f64 = TRANSACTION_FEE as f64 / 1_000_000_000.0;

/// Third-party fee in SOL: either fixed `tip_sol` or `gross_profit_sol * third_party_fee_profit_pct` when that option is set.
/// Returns (total_tx_cost_sol, third_party_fee_sol).
pub fn calculate_tx_cost_for_profit(fee: &TxCostConfig, gross_profit_sol: f64) -> (f64, f64) {
    let third_party_fee_sol = match fee.third_party_fee_profit_pct {
        Some(pct) if pct > 0.0 && pct <= 1.0 => (gross_profit_sol * pct).max(0.0),
        _ => fee.tip_sol,
    };
    let total_sol = BASE_TX_FEE_SOL + third_party_fee_sol;
    (total_sol, third_party_fee_sol)
}

/// Transaction cost for a trade: takes gross profit in raw token units and returns (total_tx_cost in raw token units, third_party_fee_sol for submission).
/// Use `sol_price` (USD per SOL) when token is not SOL so that gross profit can be converted to SOL for the fee formula.
pub fn calculate_tx_cost_for_trade_with_sol_price(
    fee: &TxCostConfig,
    gross_profit_raw: i64,
    token_is_sol: bool,
    token_decimal: u8,
    sol_price: f64,
) -> (i64, f64) {
    let pow = 10_f64.powf(token_decimal as f64);
    let gross_profit_sol = if token_is_sol {
        gross_profit_raw as f64 / pow
    } else {
        (gross_profit_raw as f64 / pow) / sol_price
    };
    let (total_sol, third_party_fee_sol) = calculate_tx_cost_for_profit(fee, gross_profit_sol);
    let total_tx_cost_raw = if token_is_sol {
        (total_sol * pow) as i64
    } else {
        (total_sol * sol_price * pow) as i64
    };
    (total_tx_cost_raw, third_party_fee_sol)
}

/// Async version: uses current SOL price from runtime. Returns (total_tx_cost in raw token units, third_party_fee_sol for submission).
pub async fn calculate_tx_cost_for_trade(
    fee: &TxCostConfig,
    gross_profit_raw: i64,
    token_is_sol: bool,
    token_decimal: u8,
) -> (i64, f64) {
    let sol_price = {
        let guard = SOL_PRICE.lock().await;
        guard.unwrap_or(fee.sol_usd)
    };
    calculate_tx_cost_for_trade_with_sol_price(
        fee,
        gross_profit_raw,
        token_is_sol,
        token_decimal,
        sol_price,
    )
}

/// Base transaction fee in lamports = 0.000005 SOL.
/// Formula: (base_tx_fee + tip_sol) * sol_usd. Use only when third-party fee is fixed (no profit-based fee).
pub async fn calculate_tx_cost_usdc(fee: &TxCostConfig) -> f64 {
    let sol_price = {
        let guard = SOL_PRICE.lock().await;
        guard.unwrap_or(fee.sol_usd)
    };
    let (total_sol, _) = calculate_tx_cost_for_profit(fee, 0.0);
    total_sol * sol_price
}
