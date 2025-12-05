use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, Bytes, U256};
use alloy::providers::Provider;
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::{eyre, Result};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::{RouterConfig, RouterType, WMON_ADDRESS, USDC_ADDRESS, WMON_DECIMALS, USDC_DECIMALS};
use crate::nonce::next_nonce;
use super::routers::build_swap_calldata;

// Monad mainnet chain ID
const MONAD_CHAIN_ID: u64 = 143;

// ERC20 interface for approvals and balance checks
sol! {
    #[derive(Debug)]
    function approve(address spender, uint256 amount) external returns (bool);

    #[derive(Debug)]
    function allowance(address owner, address spender) external view returns (uint256);

    #[derive(Debug)]
    function balanceOf(address account) external view returns (uint256);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapDirection {
    Buy,   // USDC -> WMON
    Sell,  // WMON -> USDC
}

#[derive(Debug, Clone)]
pub struct SwapParams {
    pub router: RouterConfig,
    pub direction: SwapDirection,
    pub amount_in: f64,          // Human-readable amount
    pub slippage_bps: u32,       // e.g., 100 = 1%
    pub expected_price: f64,     // From price monitor
}

#[derive(Debug, Clone)]
pub struct SwapResult {
    pub dex_name: String,
    pub direction: SwapDirection,
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: U256,
    pub amount_in_human: f64,
    pub amount_out: U256,
    pub amount_out_human: f64,
    pub expected_price: f64,
    pub executed_price: f64,
    pub price_impact_bps: i32,
    pub gas_used: u64,
    pub gas_price: u128,
    pub gas_cost_wei: U256,
    pub tx_hash: String,
    pub success: bool,
    pub error: Option<String>,
}

/// Convert human amount to U256 with proper decimals
fn to_wei(amount: f64, decimals: u8) -> U256 {
    let multiplier = 10u64.pow(decimals as u32);
    let wei_amount = (amount * multiplier as f64) as u64;
    U256::from(wei_amount)
}

/// Convert U256 to human-readable with proper decimals
fn from_wei(amount: U256, decimals: u8) -> f64 {
    let divisor = 10u64.pow(decimals as u32) as f64;
    let amount_u128: u128 = amount.try_into().unwrap_or(0);
    amount_u128 as f64 / divisor
}

/// Get hardcoded gas limit for a router type (eliminates gas estimation RPC calls)
fn get_gas_limit_for_router(router_type: RouterType) -> u64 {
    match router_type {
        RouterType::UniswapV3 => 280_000,
        RouterType::PancakeV3 => 280_000,
        RouterType::LfjLB => 420_000,
        RouterType::MondayTrade => 280_000,
    }
}

/// Check that router has sufficient approval. Does NOT send approval TX.
/// If approval is missing, returns error instructing user to run prepare-arb.
pub async fn check_approval<P: Provider>(
    provider: &P,
    wallet_address: Address,
    token: Address,
    spender: Address,
    amount: U256,
) -> Result<()> {
    let allowance_call = allowanceCall {
        owner: wallet_address,
        spender,
    };

    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(token)
        .input(alloy::rpc::types::TransactionInput::new(Bytes::from(allowance_call.abi_encode())));

    let result = provider.call(tx).await?;
    let current_allowance = U256::from_be_slice(&result);

    if current_allowance >= amount {
        return Ok(());
    }

    Err(eyre!(
        "Insufficient allowance for router {:?}. Run 'cargo run -- prepare-arb' first.",
        spender
    ))
}

/// Execute a swap on the specified DEX
///
/// # Arguments
/// * `provider` - Provider for read-only calls
/// * `provider_with_signer` - Pre-built provider with wallet for sending transactions
/// * `signer_address` - Wallet address for the signer
/// * `params` - Swap parameters
/// * `gas_price` - Pre-fetched gas price (avoids RPC call per swap)
/// * `skip_balance_check` - If true, skip balance checks (saves ~400ms per swap)
pub async fn execute_swap<P: Provider, S: Provider>(
    provider: &P,
    provider_with_signer: &S,
    signer_address: Address,
    params: SwapParams,
    gas_price: u128,
    skip_balance_check: bool,
) -> Result<SwapResult> {
    let wallet_address = signer_address;

    // Determine token addresses and decimals based on direction
    let (token_in, token_out, decimals_in, decimals_out) = match params.direction {
        SwapDirection::Sell => (WMON_ADDRESS, USDC_ADDRESS, WMON_DECIMALS, USDC_DECIMALS),
        SwapDirection::Buy => (USDC_ADDRESS, WMON_ADDRESS, USDC_DECIMALS, WMON_DECIMALS),
    };

    // Convert to wei
    let amount_in = to_wei(params.amount_in, decimals_in);

    // Calculate minimum output based on expected price and slippage
    let expected_out = match params.direction {
        SwapDirection::Sell => params.amount_in * params.expected_price,  // WMON * price = USDC
        SwapDirection::Buy => params.amount_in / params.expected_price,   // USDC / price = WMON
    };

    let slippage_multiplier = 1.0 - (params.slippage_bps as f64 / 10000.0);
    let min_out = expected_out * slippage_multiplier;
    let amount_out_min = to_wei(min_out, decimals_out);

    println!("\n  Swap Details:");
    println!("    Amount In:  {} {}", params.amount_in, if params.direction == SwapDirection::Sell { "WMON" } else { "USDC" });
    println!("    Expected Out: {:.6} {}", expected_out, if params.direction == SwapDirection::Sell { "USDC" } else { "WMON" });
    println!("    Min Out ({:.2}% slip): {:.6}", params.slippage_bps as f64 / 100.0, min_out);

    // Check approval (does NOT send TX - run prepare-arb first)
    check_approval(provider, wallet_address, token_in, params.router.address, amount_in).await?;

    // Get deadline (5 minutes from now)
    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() + 300;

    // Build swap calldata
    let calldata = build_swap_calldata(
        params.router.router_type,
        token_in,
        token_out,
        amount_in,
        amount_out_min,
        wallet_address,
        params.router.pool_fee,
        deadline,
    )?;

    println!("  → Executing swap on {}...", params.router.name);

    // Check balance before (skip if skip_balance_check is true)
    let balance_before = if skip_balance_check {
        U256::ZERO
    } else {
        let balance_before_call = balanceOfCall { account: wallet_address };
        let balance_tx = alloy::rpc::types::TransactionRequest::default()
            .to(token_out)
            .input(alloy::rpc::types::TransactionInput::new(Bytes::from(balance_before_call.abi_encode())));
        let result = provider.call(balance_tx).await?;
        U256::from_be_slice(&result)
    };

    // Use hardcoded gas limit based on router type (eliminates gas estimation RPC call)
    let gas_limit = get_gas_limit_for_router(params.router.router_type);

    println!("    Gas Limit: {} (hardcoded), gas_price: {} gwei",
             gas_limit, gas_price / 1_000_000_000);

    // Build transaction with ALL fields set to prevent filler RPC calls
    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(params.router.address)
        .from(wallet_address)
        .input(alloy::rpc::types::TransactionInput::new(calldata))
        .gas_limit(gas_limit)
        .nonce(next_nonce())
        .max_fee_per_gas(gas_price + (gas_price / 10))  // Add 10% buffer
        .max_priority_fee_per_gas(gas_price / 10)       // 10% tip
        .with_chain_id(MONAD_CHAIN_ID);

    // Use pre-built provider with signer (passed in to avoid rebuilding per swap)
    let start = std::time::Instant::now();
    let send_result = provider_with_signer.send_transaction(tx).await;

    match send_result {
        Ok(pending) => {
            let receipt = pending.get_receipt().await?;
            let elapsed = start.elapsed();

            // Check balance after (skip if skip_balance_check is true - use expected output)
            let amount_out = if skip_balance_check {
                // Estimate output based on expected price (no RPC call)
                let expected_out = match params.direction {
                    SwapDirection::Sell => params.amount_in * params.expected_price,
                    SwapDirection::Buy => params.amount_in / params.expected_price,
                };
                to_wei(expected_out, decimals_out)
            } else {
                let balance_after_call = balanceOfCall { account: wallet_address };
                let balance_tx = alloy::rpc::types::TransactionRequest::default()
                    .to(token_out)
                    .input(alloy::rpc::types::TransactionInput::new(Bytes::from(balance_after_call.abi_encode())));
                let result = provider.call(balance_tx).await?;
                let balance_after = U256::from_be_slice(&result);
                balance_after.saturating_sub(balance_before)
            };
            let amount_out_human = from_wei(amount_out, decimals_out);

            // Calculate executed price
            let executed_price = match params.direction {
                SwapDirection::Sell => amount_out_human / params.amount_in,  // USDC / WMON
                SwapDirection::Buy => params.amount_in / amount_out_human,   // USDC / WMON
            };

            let price_impact_bps = ((executed_price - params.expected_price) / params.expected_price * 10000.0) as i32;

            let gas_used = receipt.gas_used;
            let gas_price = receipt.effective_gas_price;
            let gas_cost_wei = U256::from(gas_used) * U256::from(gas_price);

            println!("  ✓ Swap completed in {:?}", elapsed);
            println!("    TX: {:?}", receipt.transaction_hash);

            Ok(SwapResult {
                dex_name: params.router.name.to_string(),
                direction: params.direction,
                token_in,
                token_out,
                amount_in,
                amount_in_human: params.amount_in,
                amount_out,
                amount_out_human,
                expected_price: params.expected_price,
                executed_price,
                price_impact_bps,
                gas_used,
                gas_price,
                gas_cost_wei,
                tx_hash: format!("{:?}", receipt.transaction_hash),
                success: receipt.status(),
                error: if receipt.status() { None } else { Some("Transaction reverted".to_string()) },
            })
        }
        Err(e) => {
            Ok(SwapResult {
                dex_name: params.router.name.to_string(),
                direction: params.direction,
                token_in,
                token_out,
                amount_in,
                amount_in_human: params.amount_in,
                amount_out: U256::ZERO,
                amount_out_human: 0.0,
                expected_price: params.expected_price,
                executed_price: 0.0,
                price_impact_bps: 0,
                gas_used: 0,
                gas_price: 0,
                gas_cost_wei: U256::ZERO,
                tx_hash: String::new(),
                success: false,
                error: Some(e.to_string()),
            })
        }
    }
}
