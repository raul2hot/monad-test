//! Wallet and balance management

use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;
use std::str::FromStr;

sol! {
    function balanceOf(address account) external view returns (uint256);
    function allowance(address owner, address spender) external view returns (uint256);
    function approve(address spender, uint256 amount) external returns (bool);
    function decimals() external view returns (uint8);
}

#[derive(Debug, Clone)]
pub struct WalletInfo {
    pub address: Address,
    pub mon_balance: U256,   // WMON (wrapped)
    pub usdc_balance: U256,
}

impl WalletInfo {
    pub fn print(&self) {
        let mon_human = self.mon_balance.to_string().parse::<f64>().unwrap_or(0.0) / 1e18;
        let usdc_human = self.usdc_balance.to_string().parse::<f64>().unwrap_or(0.0) / 1e6;
        println!("Wallet: {:?}", self.address);
        println!("  WMON Balance: {:.6} WMON", mon_human);
        println!("  USDC Balance: {:.6} USDC", usdc_human);
    }
}

#[derive(Debug, Clone)]
pub struct FullWalletInfo {
    pub address: Address,
    pub native_mon: U256,    // Native MON (gas token)
    pub wmon_balance: U256,  // WMON (wrapped, ERC-20)
    pub usdc_balance: U256,
}

impl FullWalletInfo {
    pub fn print(&self) {
        let native = self.native_mon.to_string().parse::<f64>().unwrap_or(0.0) / 1e18;
        let wmon = self.wmon_balance.to_string().parse::<f64>().unwrap_or(0.0) / 1e18;
        let usdc = self.usdc_balance.to_string().parse::<f64>().unwrap_or(0.0) / 1e6;
        println!("Wallet: {:?}", self.address);
        println!("  Native MON:   {:.6} MON (gas token)", native);
        println!("  WMON Balance: {:.6} WMON (wrapped, tradeable)", wmon);
        println!("  USDC Balance: {:.6} USDC", usdc);
    }
}

pub async fn get_balances<P: Provider>(
    provider: &P,
    wallet: Address,
    wmon: &str,
    usdc: &str,
) -> Result<WalletInfo> {
    let wmon_addr = Address::from_str(wmon)?;
    let usdc_addr = Address::from_str(usdc)?;

    // Get WMON balance
    let call = balanceOfCall { account: wallet };
    let tx = TransactionRequest::default()
        .to(wmon_addr)
        .input(call.abi_encode().into());
    let result = provider.call(tx).await?;
    let mon_balance = balanceOfCall::abi_decode_returns(&result)?;

    // Get USDC balance
    let call = balanceOfCall { account: wallet };
    let tx = TransactionRequest::default()
        .to(usdc_addr)
        .input(call.abi_encode().into());
    let result = provider.call(tx).await?;
    let usdc_balance = balanceOfCall::abi_decode_returns(&result)?;

    Ok(WalletInfo {
        address: wallet,
        mon_balance,
        usdc_balance,
    })
}

pub async fn check_allowance<P: Provider>(
    provider: &P,
    token: &str,
    owner: Address,
    spender: &str,
) -> Result<U256> {
    let token_addr = Address::from_str(token)?;
    let spender_addr = Address::from_str(spender)?;

    let call = allowanceCall {
        owner,
        spender: spender_addr,
    };
    let tx = TransactionRequest::default()
        .to(token_addr)
        .input(call.abi_encode().into());
    let result = provider.call(tx).await?;
    let allowance = allowanceCall::abi_decode_returns(&result)?;

    Ok(allowance)
}

/// Get full wallet info including native MON balance
pub async fn get_full_balances<P: Provider>(
    provider: &P,
    wallet: Address,
    wmon: &str,
    usdc: &str,
) -> Result<FullWalletInfo> {
    let wmon_addr = Address::from_str(wmon)?;
    let usdc_addr = Address::from_str(usdc)?;

    // Get native MON balance
    let native_mon = provider.get_balance(wallet).await?;

    // Get WMON balance
    let call = balanceOfCall { account: wallet };
    let tx = TransactionRequest::default()
        .to(wmon_addr)
        .input(call.abi_encode().into());
    let result = provider.call(tx).await?;
    let wmon_balance = balanceOfCall::abi_decode_returns(&result)?;

    // Get USDC balance
    let call = balanceOfCall { account: wallet };
    let tx = TransactionRequest::default()
        .to(usdc_addr)
        .input(call.abi_encode().into());
    let result = provider.call(tx).await?;
    let usdc_balance = balanceOfCall::abi_decode_returns(&result)?;

    Ok(FullWalletInfo {
        address: wallet,
        native_mon,
        wmon_balance,
        usdc_balance,
    })
}
