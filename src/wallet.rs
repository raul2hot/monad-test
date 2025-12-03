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
    pub mon_balance: U256,
    pub usdc_balance: U256,
}

impl WalletInfo {
    pub fn print(&self) {
        let mon_human = self.mon_balance.to_string().parse::<f64>().unwrap_or(0.0) / 1e18;
        let usdc_human = self.usdc_balance.to_string().parse::<f64>().unwrap_or(0.0) / 1e6;
        println!("Wallet: {:?}", self.address);
        println!("  MON Balance:  {:.6} MON", mon_human);
        println!("  USDC Balance: {:.6} USDC", usdc_human);
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
