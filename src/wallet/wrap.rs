use alloy::network::{EthereumWallet, TransactionBuilder};
use alloy::primitives::{Bytes, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::{eyre, Result};

use crate::config::{WMON_ADDRESS, WMON_DECIMALS};
use crate::nonce::next_nonce;

// Monad mainnet chain ID
const MONAD_CHAIN_ID: u64 = 143;

// WMON interface (same as WETH)
sol! {
    #[derive(Debug)]
    function deposit() external payable;

    #[derive(Debug)]
    function withdraw(uint256 amount) external;

    #[derive(Debug)]
    function balanceOf(address account) external view returns (uint256);
}

#[derive(Debug, Clone)]
pub struct WrapResult {
    pub operation: String,
    pub amount_in: f64,
    pub amount_out: f64,
    pub tx_hash: String,
    pub gas_used: u64,
    pub gas_cost_mon: f64,
    pub success: bool,
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

/// Wrap MON to WMON
/// Sends native MON to WMON contract, receives WMON tokens
pub async fn wrap_mon<P: Provider>(
    provider: &P,
    signer: &PrivateKeySigner,
    amount: f64,
    rpc_url: &str,
) -> Result<WrapResult> {
    let wallet_address = signer.address();
    let wallet = EthereumWallet::from(signer.clone());

    let amount_wei = to_wei(amount, WMON_DECIMALS);

    println!("\n  Wrap Details:");
    println!("    Amount: {} MON -> WMON", amount);

    // Check MON balance
    let mon_balance = provider.get_balance(wallet_address).await?;
    if mon_balance < amount_wei {
        return Err(eyre!("Insufficient MON balance. Have: {:.6}, Need: {:.6}",
            from_wei(mon_balance, WMON_DECIMALS), amount));
    }

    // Get WMON balance before
    let balance_call = balanceOfCall { account: wallet_address };
    let balance_tx = alloy::rpc::types::TransactionRequest::default()
        .to(WMON_ADDRESS)
        .input(alloy::rpc::types::TransactionInput::new(Bytes::from(balance_call.abi_encode())));
    let result = provider.call(balance_tx.clone()).await?;
    let wmon_before = U256::from_be_slice(&result);

    // Fetch gas price once
    let gas_price = provider.get_gas_price().await.unwrap_or(100_000_000_000);

    // Build deposit transaction with ALL fields set to prevent filler RPC calls
    let deposit_call = depositCall {};
    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(WMON_ADDRESS)
        .from(wallet_address)
        .value(amount_wei)
        .input(alloy::rpc::types::TransactionInput::new(Bytes::from(deposit_call.abi_encode())))
        .gas_limit(60_000)
        .nonce(next_nonce())
        .max_fee_per_gas(gas_price + (gas_price / 10))
        .max_priority_fee_per_gas(gas_price / 10)
        .with_chain_id(MONAD_CHAIN_ID);

    // Create provider with signer
    let url: reqwest::Url = rpc_url.parse()?;
    let provider_with_signer = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(url);

    println!("  -> Wrapping MON to WMON...");

    let pending = provider_with_signer.send_transaction(tx).await?;
    let receipt = pending.get_receipt().await?;

    if !receipt.status() {
        return Ok(WrapResult {
            operation: "WRAP".to_string(),
            amount_in: amount,
            amount_out: 0.0,
            tx_hash: format!("{:?}", receipt.transaction_hash),
            gas_used: receipt.gas_used,
            gas_cost_mon: from_wei(U256::from(receipt.gas_used) * U256::from(receipt.effective_gas_price), WMON_DECIMALS),
            success: false,
            error: Some("Transaction reverted".to_string()),
        });
    }

    // Get WMON balance after
    let result = provider.call(balance_tx).await?;
    let wmon_after = U256::from_be_slice(&result);
    let wmon_received = wmon_after.saturating_sub(wmon_before);

    let gas_cost = U256::from(receipt.gas_used) * U256::from(receipt.effective_gas_price);

    println!("  [OK] Wrap successful!");
    println!("    TX: {:?}", receipt.transaction_hash);

    Ok(WrapResult {
        operation: "WRAP".to_string(),
        amount_in: amount,
        amount_out: from_wei(wmon_received, WMON_DECIMALS),
        tx_hash: format!("{:?}", receipt.transaction_hash),
        gas_used: receipt.gas_used,
        gas_cost_mon: from_wei(gas_cost, WMON_DECIMALS),
        success: true,
        error: None,
    })
}

/// Unwrap WMON to MON
/// Burns WMON tokens, receives native MON
pub async fn unwrap_wmon<P: Provider>(
    provider: &P,
    signer: &PrivateKeySigner,
    amount: f64,
    rpc_url: &str,
) -> Result<WrapResult> {
    let wallet_address = signer.address();
    let wallet = EthereumWallet::from(signer.clone());

    let amount_wei = to_wei(amount, WMON_DECIMALS);

    println!("\n  Unwrap Details:");
    println!("    Amount: {} WMON -> MON", amount);

    // Check WMON balance
    let balance_call = balanceOfCall { account: wallet_address };
    let balance_tx = alloy::rpc::types::TransactionRequest::default()
        .to(WMON_ADDRESS)
        .input(alloy::rpc::types::TransactionInput::new(Bytes::from(balance_call.abi_encode())));
    let result = provider.call(balance_tx).await?;
    let wmon_balance = U256::from_be_slice(&result);

    if wmon_balance < amount_wei {
        return Err(eyre!("Insufficient WMON balance. Have: {:.6}, Need: {:.6}",
            from_wei(wmon_balance, WMON_DECIMALS), amount));
    }

    // Get MON balance before
    let mon_before = provider.get_balance(wallet_address).await?;

    // Fetch gas price once
    let gas_price = provider.get_gas_price().await.unwrap_or(100_000_000_000);

    // Build withdraw transaction with ALL fields set to prevent filler RPC calls
    let withdraw_call = withdrawCall { amount: amount_wei };
    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(WMON_ADDRESS)
        .from(wallet_address)
        .input(alloy::rpc::types::TransactionInput::new(Bytes::from(withdraw_call.abi_encode())))
        .gas_limit(60_000)
        .nonce(next_nonce())
        .max_fee_per_gas(gas_price + (gas_price / 10))
        .max_priority_fee_per_gas(gas_price / 10)
        .with_chain_id(MONAD_CHAIN_ID);

    // Create provider with signer
    let url: reqwest::Url = rpc_url.parse()?;
    let provider_with_signer = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(url);

    println!("  -> Unwrapping WMON to MON...");

    let pending = provider_with_signer.send_transaction(tx).await?;
    let receipt = pending.get_receipt().await?;

    if !receipt.status() {
        return Ok(WrapResult {
            operation: "UNWRAP".to_string(),
            amount_in: amount,
            amount_out: 0.0,
            tx_hash: format!("{:?}", receipt.transaction_hash),
            gas_used: receipt.gas_used,
            gas_cost_mon: from_wei(U256::from(receipt.gas_used) * U256::from(receipt.effective_gas_price), WMON_DECIMALS),
            success: false,
            error: Some("Transaction reverted".to_string()),
        });
    }

    // Get MON balance after
    let mon_after = provider.get_balance(wallet_address).await?;

    // Account for gas cost: MON received = mon_after - mon_before + gas_cost
    let gas_cost = U256::from(receipt.gas_used) * U256::from(receipt.effective_gas_price);
    let mon_received = mon_after.saturating_sub(mon_before).saturating_add(gas_cost);

    println!("  [OK] Unwrap successful!");
    println!("    TX: {:?}", receipt.transaction_hash);

    Ok(WrapResult {
        operation: "UNWRAP".to_string(),
        amount_in: amount,
        amount_out: from_wei(mon_received, WMON_DECIMALS),
        tx_hash: format!("{:?}", receipt.transaction_hash),
        gas_used: receipt.gas_used,
        gas_cost_mon: from_wei(gas_cost, WMON_DECIMALS),
        success: true,
        error: None,
    })
}

/// Print wrap/unwrap result
pub fn print_wrap_result(result: &WrapResult) {
    println!();
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║                    {} RESULT                           ║",
        if result.operation == "WRAP" { "WRAP  " } else { "UNWRAP" });
    println!("╠══════════════════════════════════════════════════════════════╣");

    if result.success {
        let (from_token, to_token) = if result.operation == "WRAP" {
            ("MON", "WMON")
        } else {
            ("WMON", "MON")
        };

        println!("║  Status: SUCCESS                                             ║");
        println!("║  {:>12}: {:>18.6} {}                       ║", "Input", result.amount_in, from_token);
        println!("║  {:>12}: {:>18.6} {}                      ║", "Output", result.amount_out, to_token);
        println!("║  {:>12}: {:>18} gas                       ║", "Gas Used", result.gas_used);
        println!("║  {:>12}: {:>18.6} MON                      ║", "Gas Cost", result.gas_cost_mon);
        println!("╠══════════════════════════════════════════════════════════════╣");
        println!("║  TX: {}  ║", &result.tx_hash[..42]);
    } else {
        println!("║  Status: FAILED                                              ║");
        println!("║  Error: {:54}║", result.error.as_ref().unwrap_or(&"Unknown".to_string()));
    }

    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();
}
