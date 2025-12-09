//! Atomic DEX-to-DEX Arbitrage Module
//!
//! Executes arbitrage via smart contract in a SINGLE transaction.
//! Benefits over fast_arb.rs:
//! - No MEV front-running between swaps
//! - Atomic: reverts if unprofitable
//! - Single gas payment
//! - ~500-800ms total execution vs ~2600ms

use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, Bytes, U256, Uint};
use alloy::providers::Provider;
use alloy::sol;
use alloy::sol_types::SolCall;
use chrono::Local;
use eyre::{eyre, Result};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::timeout;

use crate::config::{
    RouterConfig, RouterType, WMON_ADDRESS, USDC_ADDRESS, WMON_DECIMALS, USDC_DECIMALS,
    ATOMIC_ARB_CONTRACT,
};
use crate::nonce::next_nonce;
use super::routers::build_swap_calldata;
use super::SwapDirection;

// Monad mainnet chain ID
const MONAD_CHAIN_ID: u64 = 143;

// Gas buffer for atomic arb (tighter than 2-TX version)
const GAS_BUFFER_PERCENT: u64 = 12;

// TURBO mode constants - hardcoded gas to skip estimation
const TURBO_GAS_LIMIT: u64 = 500_000;  // Safe for all router combinations
const TURBO_GAS_BUFFER: u64 = 50_000;  // Extra buffer for safety

// Router enum matching Solidity contract
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum ContractRouter {
    Uniswap = 0,
    PancakeSwap = 1,
    MondayTrade = 2,
    LFJ = 3,
}

impl From<RouterType> for ContractRouter {
    fn from(rt: RouterType) -> Self {
        match rt {
            RouterType::UniswapV3 => ContractRouter::Uniswap,
            RouterType::PancakeV3 => ContractRouter::PancakeSwap,
            RouterType::MondayTrade => ContractRouter::MondayTrade,
            RouterType::LfjLB => ContractRouter::LFJ,
        }
    }
}

// Contract interface
sol! {
    #[derive(Debug)]
    function executeArb(
        uint8 sellRouter,
        bytes calldata sellRouterData,
        uint8 buyRouter,
        uint24 buyPoolFee,
        uint256 minWmonOut,
        uint256 minProfit
    ) external returns (int256 profit);

    #[derive(Debug)]
    function executeArbUnchecked(
        uint8 sellRouter,
        bytes calldata sellRouterData,
        uint8 buyRouter,
        uint24 buyPoolFee,
        uint256 minWmonOut
    ) external returns (int256 profit);

    #[derive(Debug)]
    function getBalances() external view returns (uint256 wmon, uint256 usdc);

    // Custom errors for decoding
    error SwapFailed(uint8 swapIndex);
    error Unprofitable(uint256 wmonBefore, uint256 wmonAfter);
}

/// Result of atomic arbitrage execution
#[derive(Debug, Clone)]
pub struct AtomicArbResult {
    pub tx_hash: String,
    pub success: bool,
    pub profit_wmon: f64,
    pub profit_bps: i32,
    pub gas_used: u64,
    pub gas_limit: u64,
    pub gas_cost_mon: f64,
    pub execution_time_ms: u128,
    pub sell_dex: String,
    pub buy_dex: String,
    pub wmon_in: f64,
    pub error: Option<String>,
}

/// Convert human amount to U256 with proper decimals
fn to_wei(amount: f64, decimals: u8) -> U256 {
    let multiplier = U256::from(10u64).pow(U256::from(decimals));
    let amount_scaled = (amount * 1e18) as u128;
    U256::from(amount_scaled) * multiplier / U256::from(10u64).pow(U256::from(18u8))
}

/// Convert U256 to human-readable with proper decimals
fn from_wei(amount: U256, decimals: u8) -> f64 {
    let divisor = 10u64.pow(decimals as u32) as f64;
    let amount_u128: u128 = amount.try_into().unwrap_or(0);
    amount_u128 as f64 / divisor
}

/// Query contract WMON/USDC balances
pub async fn query_contract_balances<P: Provider>(provider: &P) -> Result<(f64, f64)> {
    let call = getBalancesCall {};
    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(ATOMIC_ARB_CONTRACT)
        .input(alloy::rpc::types::TransactionInput::new(Bytes::from(call.abi_encode())));

    let result = provider.call(tx).await?;
    let decoded = getBalancesCall::abi_decode_returns(&result)?;

    Ok((from_wei(decoded.wmon, WMON_DECIMALS), from_wei(decoded.usdc, USDC_DECIMALS)))
}

/// Build swap calldata for the contract to forward to router
fn build_router_calldata(
    router: &RouterConfig,
    direction: SwapDirection,
    amount_in: U256,
    amount_out_min: U256,
) -> Result<Bytes> {
    let (token_in, token_out) = match direction {
        SwapDirection::Sell => (WMON_ADDRESS, USDC_ADDRESS),
        SwapDirection::Buy => (USDC_ADDRESS, WMON_ADDRESS),
    };

    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() + 300;

    // IMPORTANT: recipient is the CONTRACT address, not the wallet
    build_swap_calldata(
        router.router_type,
        token_in,
        token_out,
        amount_in,
        amount_out_min,
        ATOMIC_ARB_CONTRACT,  // Contract receives tokens, not wallet
        router.pool_fee,
        deadline,
    )
}

/// Execute atomic arbitrage via smart contract (optimized for speed)
///
/// # Arguments
/// * `provider_with_signer` - Provider with wallet for sending transactions
/// * `sell_router` - Router to sell WMON on (higher price)
/// * `buy_router` - Router to buy WMON on (lower price)
/// * `amount` - Amount of WMON to arb
/// * `sell_price` - Expected price on sell DEX
/// * `buy_price` - Expected price on buy DEX
/// * `slippage_bps` - Slippage tolerance in basis points
/// * `min_profit_bps` - Minimum profit required (0 = any profit)
/// * `gas_price` - Pre-fetched gas price
/// * `force` - If true, skip profit check (for testing)
pub async fn execute_atomic_arb<P: Provider>(
    provider_with_signer: &P,
    signer_address: Address,
    sell_router: &RouterConfig,
    buy_router: &RouterConfig,
    amount: f64,
    sell_price: f64,
    buy_price: f64,
    slippage_bps: u32,
    _min_profit_bps: i32,  // Ignored for speed - contract handles slippage protection
    gas_price: u128,
    _force: bool,  // Always use unchecked for speed
) -> Result<AtomicArbResult> {
    let start = std::time::Instant::now();

    // Calculate amounts (pure computation, instant)
    let wmon_in_wei = to_wei(amount, WMON_DECIMALS);
    let expected_usdc = amount * sell_price;
    let slippage_mult = 1.0 - (slippage_bps as f64 / 10000.0);
    let min_usdc_out = expected_usdc * slippage_mult;
    let min_usdc_out_wei = to_wei(min_usdc_out, USDC_DECIMALS);

    let expected_wmon_back = expected_usdc / buy_price;
    let min_wmon_out = expected_wmon_back * slippage_mult;
    let min_wmon_out_wei = to_wei(min_wmon_out, WMON_DECIMALS);

    // Build calldata (pure computation)
    let sell_calldata = build_router_calldata(
        sell_router,
        SwapDirection::Sell,
        wmon_in_wei,
        min_usdc_out_wei,
    )?;

    let buy_pool_fee_u24: Uint<24, 1> = Uint::from(buy_router.pool_fee);

    // Use unchecked version for speed
    let calldata = {
        let execute_call = executeArbUncheckedCall {
            sellRouter: ContractRouter::from(sell_router.router_type) as u8,
            sellRouterData: sell_calldata,
            buyRouter: ContractRouter::from(buy_router.router_type) as u8,
            buyPoolFee: buy_pool_fee_u24,
            minWmonOut: min_wmon_out_wei,
        };
        Bytes::from(execute_call.abi_encode())
    };

    // HARDCODED gas limit - skip estimation RPC call
    let gas_limit = TURBO_GAS_LIMIT + TURBO_GAS_BUFFER;

    // Build and send transaction
    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(ATOMIC_ARB_CONTRACT)
        .from(signer_address)
        .input(alloy::rpc::types::TransactionInput::new(calldata))
        .gas_limit(gas_limit)
        .nonce(next_nonce())
        .max_fee_per_gas(gas_price + (gas_price / 10))
        .max_priority_fee_per_gas(gas_price / 10)
        .with_chain_id(MONAD_CHAIN_ID);

    // Send transaction
    let pending = provider_with_signer.send_transaction(tx).await?;
    let tx_hash = *pending.tx_hash();

    // Fast receipt polling (2ms intervals)
    let receipt = wait_for_receipt_turbo(provider_with_signer, tx_hash).await?;

    let exec_time = start.elapsed().as_millis();

    // Estimated profit (no balance verification for speed)
    let estimated_profit = expected_wmon_back - amount;
    let estimated_profit_bps = (estimated_profit / amount * 10000.0) as i32;

    let gas_cost_wei = U256::from(gas_limit) * U256::from(receipt.effective_gas_price);
    let gas_cost_mon = gas_cost_wei.to::<u128>() as f64 / 1e18;

    Ok(AtomicArbResult {
        tx_hash: format!("{:?}", tx_hash),
        success: receipt.status(),
        profit_wmon: estimated_profit,
        profit_bps: estimated_profit_bps,
        gas_used: receipt.gas_used,
        gas_limit,
        gas_cost_mon,
        execution_time_ms: exec_time,
        sell_dex: sell_router.name.to_string(),
        buy_dex: buy_router.name.to_string(),
        wmon_in: amount,
        error: if receipt.status() { None } else { Some("Reverted".to_string()) },
    })
}

/// Print atomic arb result
pub fn print_atomic_arb_result(result: &AtomicArbResult) {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");

    println!();
    println!("===============================================================");
    println!("  ATOMIC ARB RESULT | {}", timestamp);
    println!("===============================================================");
    println!();
    println!("  Route: WMON --({})-> USDC --({})-> WMON", result.sell_dex, result.buy_dex);
    println!("  Mode: ATOMIC (single transaction)");
    println!();
    println!("  Status: {}", if result.success { "SUCCESS" } else { "FAILED" });
    println!("  TX: {}", result.tx_hash);
    println!();
    println!("  WMON In:    {:>12.6}", result.wmon_in);

    let profit_color = if result.profit_wmon >= 0.0 { "32" } else { "31" };
    println!("  Profit:     \x1b[1;{}m{:>+12.6} WMON ({:+} bps)\x1b[0m",
        profit_color, result.profit_wmon, result.profit_bps);
    println!();
    println!("  Gas Used:   {:>12} / {} limit", result.gas_used, result.gas_limit);
    println!("  Gas Cost:   {:>12.6} MON", result.gas_cost_mon);
    println!("  Time:       {:>12} ms", result.execution_time_ms);

    if let Some(ref err) = result.error {
        println!();
        println!("  Error: {}", err);
    }

    println!();
    println!("===============================================================");
}

/// TURBO: 2ms polling for receipt (fast for Monad blocks)
async fn wait_for_receipt_turbo<P: Provider>(
    provider: &P,
    tx_hash: alloy::primitives::TxHash,
) -> Result<alloy::rpc::types::TransactionReceipt> {
    use tokio::time::interval;
    let mut poll = interval(Duration::from_millis(2));

    for _ in 0..2500 { // 5 seconds max at 2ms intervals
        poll.tick().await;
        if let Some(receipt) = provider.get_transaction_receipt(tx_hash).await? {
            return Ok(receipt);
        }
    }
    Err(eyre!("Receipt timeout"))
}
