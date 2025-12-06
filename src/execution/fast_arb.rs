//! Fast DEX-to-DEX Arbitrage Module
//!
//! Optimized for <1.2s execution by:
//! - Skipping wait_for_next_block
//! - Sending both TXs back-to-back
//! - Parallel receipt waiting with timeout
//! - 20ms polling (optimized for Monad's fast blocks)
//! - Dynamic safety buffer based on slippage
//! - Skipping approval/balance checks
//!
//! CRITICAL MONAD GAS FIX:
//! Monad charges gas_limit, NOT gas_used!
//! We use eth_estimateGas + 10% buffer instead of hardcoded limits.

use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, Bytes, TxHash, U256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionReceipt;
use alloy::sol;
use alloy::sol_types::SolCall;
use chrono::Local;
use eyre::{eyre, Result};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::{interval, timeout};

use crate::config::{RouterConfig, RouterType, WMON_ADDRESS, USDC_ADDRESS, WMON_DECIMALS, USDC_DECIMALS};
use crate::nonce::next_nonce;
use super::routers::build_swap_calldata;
use super::SwapDirection;

// Monad mainnet chain ID
const MONAD_CHAIN_ID: u64 = 143;

// Gas estimation buffer (10% for Monad - keep tight to minimize costs!)
const GAS_BUFFER_PERCENT: u64 = 15;

// Fallback gas limits (only used if estimation fails)
const FALLBACK_GAS_LIMIT_SIMPLE: u64 = 250_000;
const FALLBACK_GAS_LIMIT_COMPLEX: u64 = 400_000;

// ERC20 interface for approvals
sol! {
    #[derive(Debug)]
    function approve(address spender, uint256 amount) external returns (bool);

    #[derive(Debug)]
    function allowance(address owner, address spender) external view returns (uint256);
}

/// Result of a fast arbitrage execution
#[derive(Debug, Clone)]
pub struct FastArbResult {
    // Swap 1: Sell WMON for USDC
    pub swap1_tx_hash: String,
    pub swap1_gas_used: u64,
    pub swap1_gas_estimated: u64,  // NEW: Track estimated vs actual
    pub swap1_success: bool,

    // Swap 2: Buy WMON with USDC
    pub swap2_tx_hash: String,
    pub swap2_gas_used: u64,
    pub swap2_gas_estimated: u64,  // NEW: Track estimated vs actual
    pub swap2_success: bool,

    // Amounts
    pub wmon_in: f64,
    pub usdc_intermediate: f64,  // Estimated USDC from swap 1
    pub wmon_out: f64,           // Estimated WMON from swap 2

    // Actual vs Estimated tracking
    pub wmon_out_actual: Option<f64>,        // Filled after balance check
    pub estimation_error_bps: Option<i32>,   // (actual - estimated) / estimated * 10000

    // Profit/Loss
    pub gross_profit_wmon: f64,
    pub profit_bps: i32,

    // Gas costs
    pub total_gas_cost_wei: U256,
    pub total_gas_cost_mon: f64,
    pub total_gas_used: u64,      // Combined gas used for both swaps
    pub total_gas_estimated: u64, // NEW: Combined gas estimated (what we paid for on Monad!)

    // Timing
    pub total_time_ms: u128,
    pub swap1_time_ms: u128,
    pub swap2_time_ms: u128,
    pub execution_time_ms: u128,  // Alias for total_time_ms for logging

    // Overall success
    pub success: bool,
    pub error: Option<String>,
}

/// Convert human amount to U256 with proper decimals
fn to_wei(amount: f64, decimals: u8) -> U256 {
    let multiplier = U256::from(10u64).pow(U256::from(decimals));
    let amount_scaled = (amount * 1e18) as u128;
    U256::from(amount_scaled) * multiplier / U256::from(10u64).pow(U256::from(18u8))
}

/// Get fallback gas limit for a router type (only used if estimation fails)
fn get_fallback_gas_limit(router_type: RouterType) -> u64 {
    match router_type {
        RouterType::UniswapV3 => FALLBACK_GAS_LIMIT_SIMPLE,
        RouterType::PancakeV3 => FALLBACK_GAS_LIMIT_COMPLEX, // Multicall wrapper
        RouterType::LfjLB => FALLBACK_GAS_LIMIT_COMPLEX,     // Complex path routing
        RouterType::MondayTrade => FALLBACK_GAS_LIMIT_SIMPLE,
    }
}

/// Estimate gas for a transaction using eth_estimateGas
/// Returns estimated gas + buffer, or fallback if estimation fails
async fn estimate_gas_with_buffer<P: Provider>(
    provider: &P,
    to: Address,
    from: Address,
    calldata: &Bytes,
    router_type: RouterType,
) -> u64 {
    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(to)
        .from(from)
        .input(alloy::rpc::types::TransactionInput::new(calldata.clone()));

    match provider.estimate_gas(tx).await {
        Ok(estimated) => {
            // Add buffer: estimated * (100 + buffer%) / 100
            let with_buffer = estimated * (100 + GAS_BUFFER_PERCENT) / 100;
            println!("    Gas estimated: {} + {}% buffer = {}", estimated, GAS_BUFFER_PERCENT, with_buffer);
            with_buffer
        }
        Err(e) => {
            let fallback = get_fallback_gas_limit(router_type);
            println!("    ⚠ Gas estimation failed ({}), using fallback: {}", e, fallback);
            fallback
        }
    }
}

/// Wait for transaction receipt with FAST 20ms polling
/// Times out after 15 seconds (faster than standard 30s)
async fn wait_for_receipt_fast<P: Provider>(
    provider: &P,
    tx_hash: TxHash,
) -> Result<TransactionReceipt> {
    let mut poll_interval = interval(Duration::from_millis(20)); // 20ms polling for Monad's fast blocks
    let deadline = Duration::from_secs(15); // 15s vs 30s

    timeout(deadline, async {
        loop {
            poll_interval.tick().await;
            if let Some(receipt) = provider.get_transaction_receipt(tx_hash).await? {
                return Ok::<_, eyre::Report>(receipt);
            }
        }
    })
    .await
    .map_err(|_| eyre::eyre!("Transaction confirmation timeout after 15s"))?
}

/// Pre-build swap transaction calldata
pub fn build_fast_swap_tx(
    router: &RouterConfig,
    direction: SwapDirection,
    amount_in: U256,
    amount_out_min: U256,
    recipient: Address,
) -> Result<Bytes> {
    let (token_in, token_out) = match direction {
        SwapDirection::Sell => (WMON_ADDRESS, USDC_ADDRESS),
        SwapDirection::Buy => (USDC_ADDRESS, WMON_ADDRESS),
    };

    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() + 300;

    build_swap_calldata(
        router.router_type,
        token_in,
        token_out,
        amount_in,
        amount_out_min,
        recipient,
        router.pool_fee,
        deadline,
    )
}

/// Execute fast DEX-to-DEX arbitrage
///
/// MONAD GAS OPTIMIZATION:
/// Uses eth_estimateGas + 10% buffer instead of hardcoded limits.
/// On Monad, you pay for gas_limit, not gas_used!
///
/// # Arguments
/// * `provider_with_signer` - Provider with wallet for sending transactions
/// * `signer_address` - Wallet address
/// * `sell_router` - Router to sell WMON on (higher price)
/// * `buy_router` - Router to buy WMON on (lower price)
/// * `amount` - Amount of WMON to start with
/// * `sell_price` - Expected price on sell DEX
/// * `buy_price` - Expected price on buy DEX
/// * `slippage_bps` - Slippage tolerance in bps
/// * `gas_price` - Pre-fetched gas price
pub async fn execute_fast_arb<P: Provider>(
    provider_with_signer: &P,
    signer_address: Address,
    sell_router: &RouterConfig,
    buy_router: &RouterConfig,
    amount: f64,
    sell_price: f64,
    buy_price: f64,
    slippage_bps: u32,
    gas_price: u128,
) -> Result<FastArbResult> {
    let total_start = std::time::Instant::now();

    // Calculate expected amounts
    let wmon_in_wei = to_wei(amount, WMON_DECIMALS);
    let expected_usdc = amount * sell_price;
    let expected_usdc_wei = to_wei(expected_usdc, USDC_DECIMALS);

    // Dynamic buffer: slippage 200bps → 99%, slippage 100bps → 99.5%, slippage 50bps → 99.75%
    let safety_factor = 1.0 - (slippage_bps as f64 / 20000.0);
    let usdc_for_swap2 = expected_usdc * safety_factor;
    let usdc_for_swap2_wei = to_wei(usdc_for_swap2, USDC_DECIMALS);

    let expected_wmon_back = usdc_for_swap2 / buy_price;

    // Calculate min outputs with slippage
    let slippage_multiplier = 1.0 - (slippage_bps as f64 / 10000.0);
    let min_usdc_out = expected_usdc * slippage_multiplier;
    let min_usdc_out_wei = to_wei(min_usdc_out, USDC_DECIMALS);
    let min_wmon_out = expected_wmon_back * slippage_multiplier;
    let min_wmon_out_wei = to_wei(min_wmon_out, WMON_DECIMALS);

    println!("  Pre-calculating swap parameters...");
    println!("    WMON In: {:.6}", amount);
    println!("    Expected USDC: {:.6} (using {:.6} for swap 2)", expected_usdc, usdc_for_swap2);
    println!("    Expected WMON back: {:.6}", expected_wmon_back);
    println!("    Min USDC out: {:.6} ({}bps slippage)", min_usdc_out, slippage_bps);
    println!("    Min WMON out: {:.6} ({}bps slippage)", min_wmon_out, slippage_bps);

    // ═══════════════════════════════════════════════════════════════════════
    // PRE-BUILD BOTH TRANSACTIONS (optimization: ready before sending)
    // ═══════════════════════════════════════════════════════════════════════

    // Build swap 1 calldata: Sell WMON -> USDC
    let swap1_calldata = build_fast_swap_tx(
        sell_router,
        SwapDirection::Sell,
        wmon_in_wei,
        min_usdc_out_wei,
        signer_address,
    )?;

    // Build swap 2 calldata: Buy USDC -> WMON
    let swap2_calldata = build_fast_swap_tx(
        buy_router,
        SwapDirection::Buy,
        usdc_for_swap2_wei,
        min_wmon_out_wei,
        signer_address,
    )?;

    // ═══════════════════════════════════════════════════════════════════════
    // MONAD GAS FIX: Estimate gas dynamically instead of hardcoded limits!
    // On Monad, you pay gas_limit, not gas_used. Every extra gas unit costs money.
    // ═══════════════════════════════════════════════════════════════════════
    
    println!("\n  Estimating gas (MONAD: you pay gas_limit, not gas_used!)...");
    
    let swap1_gas_limit = estimate_gas_with_buffer(
        provider_with_signer,
        sell_router.address,
        signer_address,
        &swap1_calldata,
        sell_router.router_type,
    ).await;

    let swap2_gas_limit = estimate_gas_with_buffer(
        provider_with_signer,
        buy_router.address,
        signer_address,
        &swap2_calldata,
        buy_router.router_type,
    ).await;

    println!("    Swap 1 gas limit: {} (will be charged on Monad)", swap1_gas_limit);
    println!("    Swap 2 gas limit: {} (will be charged on Monad)", swap2_gas_limit);
    println!("    TOTAL gas budget: {} (this is your actual cost!)", swap1_gas_limit + swap2_gas_limit);

    // Build swap 1 transaction
    let swap1_nonce = next_nonce();
    let swap1_tx = alloy::rpc::types::TransactionRequest::default()
        .to(sell_router.address)
        .from(signer_address)
        .input(alloy::rpc::types::TransactionInput::new(swap1_calldata))
        .gas_limit(swap1_gas_limit)
        .nonce(swap1_nonce)
        .max_fee_per_gas(gas_price + (gas_price / 10))
        .max_priority_fee_per_gas(gas_price / 10)
        .with_chain_id(MONAD_CHAIN_ID);

    // Build swap 2 transaction (nonce = swap1 + 1)
    let swap2_nonce = next_nonce();
    let swap2_tx = alloy::rpc::types::TransactionRequest::default()
        .to(buy_router.address)
        .from(signer_address)
        .input(alloy::rpc::types::TransactionInput::new(swap2_calldata))
        .gas_limit(swap2_gas_limit)
        .nonce(swap2_nonce)
        .max_fee_per_gas(gas_price + (gas_price / 10))
        .max_priority_fee_per_gas(gas_price / 10)
        .with_chain_id(MONAD_CHAIN_ID);

    println!("\n  Sending transactions back-to-back...");

    // ═══════════════════════════════════════════════════════════════════════
    // SEND BOTH TRANSACTIONS BACK-TO-BACK (optimization: no waiting between)
    // ═══════════════════════════════════════════════════════════════════════

    let swap1_start = std::time::Instant::now();

    // Send swap 1
    let swap1_pending = match timeout(
        Duration::from_secs(10),
        provider_with_signer.send_transaction(swap1_tx)
    ).await {
        Ok(Ok(pending)) => pending,
        Ok(Err(e)) => {
            return Ok(FastArbResult {
                swap1_tx_hash: String::new(),
                swap1_gas_used: 0,
                swap1_gas_estimated: swap1_gas_limit,
                swap1_success: false,
                swap2_tx_hash: String::new(),
                swap2_gas_used: 0,
                swap2_gas_estimated: swap2_gas_limit,
                swap2_success: false,
                wmon_in: amount,
                usdc_intermediate: 0.0,
                wmon_out: 0.0,
                wmon_out_actual: None,
                estimation_error_bps: None,
                gross_profit_wmon: 0.0,
                profit_bps: 0,
                total_gas_cost_wei: U256::ZERO,
                total_gas_cost_mon: 0.0,
                total_gas_used: 0,
                total_gas_estimated: swap1_gas_limit + swap2_gas_limit,
                total_time_ms: total_start.elapsed().as_millis(),
                swap1_time_ms: 0,
                swap2_time_ms: 0,
                execution_time_ms: total_start.elapsed().as_millis(),
                success: false,
                error: Some(format!("Swap 1 send failed: {}", e)),
            });
        }
        Err(_) => {
            return Ok(FastArbResult {
                swap1_tx_hash: String::new(),
                swap1_gas_used: 0,
                swap1_gas_estimated: swap1_gas_limit,
                swap1_success: false,
                swap2_tx_hash: String::new(),
                swap2_gas_used: 0,
                swap2_gas_estimated: swap2_gas_limit,
                swap2_success: false,
                wmon_in: amount,
                usdc_intermediate: 0.0,
                wmon_out: 0.0,
                wmon_out_actual: None,
                estimation_error_bps: None,
                gross_profit_wmon: 0.0,
                profit_bps: 0,
                total_gas_cost_wei: U256::ZERO,
                total_gas_cost_mon: 0.0,
                total_gas_used: 0,
                total_gas_estimated: swap1_gas_limit + swap2_gas_limit,
                total_time_ms: total_start.elapsed().as_millis(),
                swap1_time_ms: 0,
                swap2_time_ms: 0,
                execution_time_ms: total_start.elapsed().as_millis(),
                success: false,
                error: Some("Swap 1 send timeout".to_string()),
            });
        }
    };

    let swap1_hash = *swap1_pending.tx_hash();
    println!("    Swap 1 sent: {:?}", swap1_hash);

    // Send swap 2 IMMEDIATELY (no waiting for swap 1 confirmation!)
    let swap2_start = std::time::Instant::now();

    let swap2_pending = match timeout(
        Duration::from_secs(10),
        provider_with_signer.send_transaction(swap2_tx)
    ).await {
        Ok(Ok(pending)) => pending,
        Ok(Err(e)) => {
            // Swap 1 was sent but swap 2 failed - still try to get swap 1 receipt
            println!("    Swap 2 send failed: {}", e);
            let swap1_receipt = wait_for_receipt_fast(provider_with_signer, swap1_hash).await?;
            let swap1_time = swap1_start.elapsed().as_millis();

            return Ok(FastArbResult {
                swap1_tx_hash: format!("{:?}", swap1_hash),
                swap1_gas_used: swap1_receipt.gas_used,
                swap1_gas_estimated: swap1_gas_limit,
                swap1_success: swap1_receipt.status(),
                swap2_tx_hash: String::new(),
                swap2_gas_used: 0,
                swap2_gas_estimated: swap2_gas_limit,
                swap2_success: false,
                wmon_in: amount,
                usdc_intermediate: expected_usdc,
                wmon_out: 0.0,
                wmon_out_actual: None,
                estimation_error_bps: None,
                gross_profit_wmon: 0.0 - amount,
                profit_bps: -10000,
                total_gas_cost_wei: U256::from(swap1_gas_limit) * U256::from(swap1_receipt.effective_gas_price),
                total_gas_cost_mon: (swap1_gas_limit as f64 * swap1_receipt.effective_gas_price as f64) / 1e18,
                total_gas_used: swap1_receipt.gas_used,
                total_gas_estimated: swap1_gas_limit,
                total_time_ms: total_start.elapsed().as_millis(),
                swap1_time_ms: swap1_time,
                swap2_time_ms: 0,
                execution_time_ms: total_start.elapsed().as_millis(),
                success: false,
                error: Some(format!("Swap 2 send failed: {}", e)),
            });
        }
        Err(_) => {
            let swap1_receipt = wait_for_receipt_fast(provider_with_signer, swap1_hash).await?;
            let swap1_time = swap1_start.elapsed().as_millis();

            return Ok(FastArbResult {
                swap1_tx_hash: format!("{:?}", swap1_hash),
                swap1_gas_used: swap1_receipt.gas_used,
                swap1_gas_estimated: swap1_gas_limit,
                swap1_success: swap1_receipt.status(),
                swap2_tx_hash: String::new(),
                swap2_gas_used: 0,
                swap2_gas_estimated: swap2_gas_limit,
                swap2_success: false,
                wmon_in: amount,
                usdc_intermediate: expected_usdc,
                wmon_out: 0.0,
                wmon_out_actual: None,
                estimation_error_bps: None,
                gross_profit_wmon: 0.0 - amount,
                profit_bps: -10000,
                total_gas_cost_wei: U256::from(swap1_gas_limit) * U256::from(swap1_receipt.effective_gas_price),
                total_gas_cost_mon: (swap1_gas_limit as f64 * swap1_receipt.effective_gas_price as f64) / 1e18,
                total_gas_used: swap1_receipt.gas_used,
                total_gas_estimated: swap1_gas_limit,
                total_time_ms: total_start.elapsed().as_millis(),
                swap1_time_ms: swap1_time,
                swap2_time_ms: 0,
                execution_time_ms: total_start.elapsed().as_millis(),
                success: false,
                error: Some("Swap 2 send timeout".to_string()),
            });
        }
    };

    let swap2_hash = *swap2_pending.tx_hash();
    println!("    Swap 2 sent: {:?}", swap2_hash);

    // ═══════════════════════════════════════════════════════════════════════
    // WAIT FOR BOTH RECEIPTS IN PARALLEL (optimization: concurrent waiting)
    // ═══════════════════════════════════════════════════════════════════════

    println!("  Waiting for confirmations (parallel)...");

    // Add explicit timeout to prevent hanging if one receipt never arrives
    let receipt_timeout = Duration::from_secs(15);
    let (receipt1_result, receipt2_result) = tokio::join!(
        timeout(receipt_timeout, wait_for_receipt_fast(provider_with_signer, swap1_hash)),
        timeout(receipt_timeout, wait_for_receipt_fast(provider_with_signer, swap2_hash))
    );

    let swap1_time = swap1_start.elapsed().as_millis();
    let swap2_time = swap2_start.elapsed().as_millis();

    let swap1_receipt = receipt1_result.map_err(|_| eyre!("Swap 1 receipt timeout"))??;
    let swap2_receipt = receipt2_result.map_err(|_| eyre!("Swap 2 receipt timeout"))??;

    println!("    Swap 1 confirmed: {} (gas used: {}, estimated: {})",
        if swap1_receipt.status() { "SUCCESS" } else { "REVERTED" },
        swap1_receipt.gas_used,
        swap1_gas_limit);
    println!("    Swap 2 confirmed: {} (gas used: {}, estimated: {})",
        if swap2_receipt.status() { "SUCCESS" } else { "REVERTED" },
        swap2_receipt.gas_used,
        swap2_gas_limit);

    // ═══════════════════════════════════════════════════════════════════════
    // MONAD GAS COST CALCULATION
    // CRITICAL: On Monad, you pay gas_limit * gas_price, NOT gas_used * gas_price!
    // ═══════════════════════════════════════════════════════════════════════
    
    // Calculate actual cost (what Monad charged us = gas_limit * price)
    let swap1_gas_cost = U256::from(swap1_gas_limit) * U256::from(swap1_receipt.effective_gas_price);
    let swap2_gas_cost = U256::from(swap2_gas_limit) * U256::from(swap2_receipt.effective_gas_price);
    let total_gas_cost_wei = swap1_gas_cost + swap2_gas_cost;
    let total_gas_cost_mon = total_gas_cost_wei.to::<u128>() as f64 / 1e18;

    let both_success = swap1_receipt.status() && swap2_receipt.status();

    // Calculate profit (estimated - we skipped balance checks for speed)
    let gross_profit = if both_success {
        expected_wmon_back - amount
    } else {
        0.0 - amount  // Lost the input if either failed
    };
    let profit_bps = if amount > 0.0 {
        (gross_profit / amount * 10000.0) as i32
    } else {
        0
    };

    let total_gas_used = swap1_receipt.gas_used + swap2_receipt.gas_used;
    let total_gas_estimated = swap1_gas_limit + swap2_gas_limit;
    let execution_time = total_start.elapsed().as_millis();

    // Log gas efficiency
    let gas_efficiency = (total_gas_used as f64 / total_gas_estimated as f64) * 100.0;
    println!("\n  ⛽ GAS EFFICIENCY: {:.1}% (used {} of {} budgeted)", 
             gas_efficiency, total_gas_used, total_gas_estimated);
    
    if gas_efficiency < 80.0 {
        println!("    💡 Consider reducing buffer - you're overpaying by {:.1}%", 100.0 - gas_efficiency);
    }

    let result = FastArbResult {
        swap1_tx_hash: format!("{:?}", swap1_hash),
        swap1_gas_used: swap1_receipt.gas_used,
        swap1_gas_estimated: swap1_gas_limit,
        swap1_success: swap1_receipt.status(),
        swap2_tx_hash: format!("{:?}", swap2_hash),
        swap2_gas_used: swap2_receipt.gas_used,
        swap2_gas_estimated: swap2_gas_limit,
        swap2_success: swap2_receipt.status(),
        wmon_in: amount,
        usdc_intermediate: usdc_for_swap2,
        wmon_out: if both_success { expected_wmon_back } else { 0.0 },
        wmon_out_actual: None,  // Can be filled after balance check
        estimation_error_bps: None,
        gross_profit_wmon: gross_profit,
        profit_bps,
        total_gas_cost_wei,
        total_gas_cost_mon,
        total_gas_used,
        total_gas_estimated,
        total_time_ms: execution_time,
        swap1_time_ms: swap1_time,
        swap2_time_ms: swap2_time,
        execution_time_ms: execution_time,
        success: both_success,
        error: if both_success { None } else { Some("One or both swaps reverted".to_string()) },
    };

    Ok(result)
}

/// Print the fast arb result in a nice format
pub fn print_fast_arb_result(result: &FastArbResult, sell_dex: &str, buy_dex: &str) {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");

    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("  FAST ARB RESULT | {}", timestamp);
    println!("═══════════════════════════════════════════════════════════════");
    println!();
    println!("  Route: WMON --({})-> USDC --({})-> WMON", sell_dex, buy_dex);
    println!();
    println!("  SWAP 1 (Sell on {}):", sell_dex);
    println!("    Status:       {}", if result.swap1_success { "SUCCESS" } else { "REVERTED" });
    println!("    TX:           {}", result.swap1_tx_hash);
    println!("    Gas Used:     {}", result.swap1_gas_used);
    println!("    Gas Limit:    {} (CHARGED on Monad!)", result.swap1_gas_estimated);
    println!("    Time:         {}ms", result.swap1_time_ms);
    println!();
    println!("  SWAP 2 (Buy on {}):", buy_dex);
    println!("    Status:       {}", if result.swap2_success { "SUCCESS" } else { "REVERTED" });
    println!("    TX:           {}", result.swap2_tx_hash);
    println!("    Gas Used:     {}", result.swap2_gas_used);
    println!("    Gas Limit:    {} (CHARGED on Monad!)", result.swap2_gas_estimated);
    println!("    Time:         {}ms", result.swap2_time_ms);
    println!();
    println!("  AMOUNTS (estimated):");
    println!("    WMON In:         {:>12.6} WMON", result.wmon_in);
    println!("    USDC (mid):      {:>12.6} USDC", result.usdc_intermediate);
    println!("    WMON Out:        {:>12.6} WMON", result.wmon_out);
    println!();
    println!("  PROFIT/LOSS:");
    let profit_color = if result.gross_profit_wmon >= 0.0 { "32" } else { "31" };
    println!("    Gross P/L:       \x1b[1;{}m{:>+12.6} WMON ({:+}bps)\x1b[0m",
        profit_color, result.gross_profit_wmon, result.profit_bps);
    println!();
    println!("  GAS (MONAD - charged by gas_limit!):");
    println!("    Gas Used:        {:>12} (actual execution)", result.total_gas_used);
    println!("    Gas Limit:       {:>12} (WHAT YOU PAID FOR!)", result.total_gas_estimated);
    println!("    Gas Cost:        {:>12.6} MON", result.total_gas_cost_mon);
    
    let efficiency = (result.total_gas_used as f64 / result.total_gas_estimated as f64) * 100.0;
    let eff_color = if efficiency > 80.0 { "32" } else { "33" };
    println!("    Efficiency:      \x1b[1;{}m{:>11.1}%\x1b[0m", eff_color, efficiency);
    println!();
    println!("  TIMING:");
    println!("    Total Time:      {}ms", result.total_time_ms);
    println!();

    if result.success {
        if result.gross_profit_wmon > 0.0 {
            println!("  \x1b[1;32mARBITRAGE SUCCESSFUL\x1b[0m");
        } else {
            println!("  \x1b[1;33mARBITRAGE COMPLETED (unprofitable)\x1b[0m");
        }
    } else {
        println!("  \x1b[1;31mARBITRAGE FAILED\x1b[0m");
        if let Some(ref err) = result.error {
            println!("  Error: {}", err);
        }
    }

    println!();
    println!("═══════════════════════════════════════════════════════════════");
}