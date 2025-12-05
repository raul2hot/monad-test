use alloy::primitives::{Address, Bytes, U256};
use alloy::providers::Provider;
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;

use crate::config::{WMON_ADDRESS, USDC_ADDRESS, WMON_DECIMALS, USDC_DECIMALS};

// ERC20 balanceOf
sol! {
    #[derive(Debug)]
    function balanceOf(address account) external view returns (uint256);
}

#[derive(Debug, Clone)]
pub struct WalletBalances {
    pub mon_balance: U256,       // Native MON (18 decimals)
    pub mon_human: f64,
    pub wmon_balance: U256,      // Wrapped MON (18 decimals)
    pub wmon_human: f64,
    pub usdc_balance: U256,      // USDC (6 decimals)
    pub usdc_human: f64,
    pub wallet_address: Address,
}

/// Convert U256 to human-readable with proper decimals
fn from_wei(amount: U256, decimals: u8) -> f64 {
    let divisor = 10u64.pow(decimals as u32) as f64;
    let amount_u128: u128 = amount.try_into().unwrap_or(0);
    amount_u128 as f64 / divisor
}

/// Get all token balances for a wallet
pub async fn get_balances<P: Provider>(
    provider: &P,
    wallet_address: Address,
) -> Result<WalletBalances> {
    // Get native MON balance
    let mon_balance = provider.get_balance(wallet_address).await?;

    // Get WMON balance
    let wmon_call = balanceOfCall { account: wallet_address };
    let wmon_tx = alloy::rpc::types::TransactionRequest::default()
        .to(WMON_ADDRESS)
        .input(alloy::rpc::types::TransactionInput::new(Bytes::from(wmon_call.abi_encode())));
    let wmon_result = provider.call(wmon_tx).await?;
    let wmon_balance = U256::from_be_slice(&wmon_result);

    // Get USDC balance
    let usdc_call = balanceOfCall { account: wallet_address };
    let usdc_tx = alloy::rpc::types::TransactionRequest::default()
        .to(USDC_ADDRESS)
        .input(alloy::rpc::types::TransactionInput::new(Bytes::from(usdc_call.abi_encode())));
    let usdc_result = provider.call(usdc_tx).await?;
    let usdc_balance = U256::from_be_slice(&usdc_result);

    Ok(WalletBalances {
        mon_balance,
        mon_human: from_wei(mon_balance, WMON_DECIMALS), // MON has 18 decimals like ETH
        wmon_balance,
        wmon_human: from_wei(wmon_balance, WMON_DECIMALS),
        usdc_balance,
        usdc_human: from_wei(usdc_balance, USDC_DECIMALS),
        wallet_address,
    })
}

/// Print wallet balances in a formatted way
pub fn print_balances(balances: &WalletBalances) {
    println!();
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║                      WALLET BALANCES                         ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  Wallet: {:?}  ║", balances.wallet_address);
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║                                                              ║");
    println!("║  {:>12}: {:>18.6} MON                      ║", "MON", balances.mon_human);
    println!("║  {:>12}: {:>18.6} WMON                     ║", "WMON", balances.wmon_human);
    println!("║  {:>12}: {:>18.6} USDC                     ║", "USDC", balances.usdc_human);
    println!("║                                                              ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  Total MON (MON + WMON): {:>18.6} MON          ║", balances.mon_human + balances.wmon_human);
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();
}
