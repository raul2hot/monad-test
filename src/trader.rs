//! Trade orchestration - combines detection with execution

use alloy::network::EthereumWallet;
use alloy::primitives::U256;
use alloy::providers::Provider;
use eyre::Result;

use crate::{config, execution, wallet, zrx};

#[derive(Debug)]
pub struct TradeParams {
    pub amount_mon: f64,     // Amount of MON to trade (e.g., 10.0)
    pub slippage_bps: u32,   // Slippage in basis points (e.g., 100 = 1%)
    pub min_profit_bps: u32, // Minimum profit to proceed (e.g., 30 = 0.3%)
}

#[derive(Debug)]
pub struct TradeReport {
    pub direction: String,
    pub amount_in: String,
    pub expected_out: String,
    pub actual_out: String,
    pub slippage_realized: f64,
    pub mon_balance_before: U256,
    pub mon_balance_after: U256,
    pub usdc_balance_before: U256,
    pub usdc_balance_after: U256,
    pub profit_loss: f64,
    pub execution_result: execution::ExecutionResult,
}

impl TradeReport {
    pub fn print(&self) {
        let mon_before = self
            .mon_balance_before
            .to_string()
            .parse::<f64>()
            .unwrap_or(0.0)
            / 1e18;
        let mon_after = self
            .mon_balance_after
            .to_string()
            .parse::<f64>()
            .unwrap_or(0.0)
            / 1e18;
        let usdc_before = self
            .usdc_balance_before
            .to_string()
            .parse::<f64>()
            .unwrap_or(0.0)
            / 1e6;
        let usdc_after = self
            .usdc_balance_after
            .to_string()
            .parse::<f64>()
            .unwrap_or(0.0)
            / 1e6;

        println!("\n=====================================================");
        println!("            TRADE EXECUTION REPORT                   ");
        println!("=====================================================");
        println!(" Direction:        {}", self.direction);
        println!(" Amount In:        {}", self.amount_in);
        println!(" Expected Out:     {}", self.expected_out);
        println!(" Actual Out:       {}", self.actual_out);
        println!(" Slippage:         {:.4}%", self.slippage_realized);
        println!("=====================================================");
        println!(" BALANCE CHANGES                                     ");
        println!(" MON Before:       {:.6}", mon_before);
        println!(" MON After:        {:.6}", mon_after);
        println!(" USDC Before:      {:.6}", usdc_before);
        println!(" USDC After:       {:.6}", usdc_after);
        println!("=====================================================");
        println!(" P/L:              {:.6}", self.profit_loss);
        println!(" Tx Hash:          {:?}", self.execution_result.tx_hash);
        println!(
            " Status:           {}",
            if self.execution_result.success {
                "SUCCESS"
            } else {
                "FAILED"
            }
        );
        println!(
            " Execution Time:   {}ms",
            self.execution_result.execution_time_ms
        );
        println!("=====================================================\n");
    }
}

/// Execute a SELL via 0x (MON -> USDC)
/// This is the simplest test - just sell MON for USDC through 0x
pub async fn execute_0x_sell<P: Provider>(
    provider: &P,
    wallet: &EthereumWallet,
    zrx: &zrx::ZrxClient,
    params: &TradeParams,
) -> Result<TradeReport> {
    let wallet_addr = wallet.default_signer().address();

    // Get balances before
    let balances_before =
        wallet::get_balances(provider, wallet_addr, config::WMON, config::USDC).await?;

    println!("\n Starting 0x SELL trade...");
    println!("Selling {} MON for USDC via 0x", params.amount_mon);
    balances_before.print();

    // Convert MON amount to wei (18 decimals)
    let sell_amount = (params.amount_mon * 1e18) as u128;
    let sell_amount_str = sell_amount.to_string();

    // Ensure approval to AllowanceHolder
    execution::ensure_approval(
        provider,
        wallet,
        config::WMON,
        config::ALLOWANCE_HOLDER,
        U256::from(sell_amount),
    )
    .await?;

    // Get quote from 0x
    println!("Fetching 0x quote...");
    let quote = zrx
        .get_quote(
            config::WMON,
            config::USDC,
            &sell_amount_str,
            &format!("{:?}", wallet_addr),
            params.slippage_bps,
        )
        .await?;

    let expected_usdc = quote.buy_amount.parse::<f64>().unwrap_or(0.0) / 1e6;
    let min_usdc = quote.min_buy_amount.parse::<f64>().unwrap_or(0.0) / 1e6;

    println!("Quote received:");
    println!("  Expected USDC: {:.6}", expected_usdc);
    println!("  Min USDC:      {:.6}", min_usdc);

    // Execute the swap
    println!("Executing swap...");
    let exec_result = execution::execute_0x_swap(provider, wallet, &quote).await?;
    exec_result.print();

    // Get balances after
    let balances_after =
        wallet::get_balances(provider, wallet_addr, config::WMON, config::USDC).await?;

    // Calculate actual output
    let usdc_received = balances_after
        .usdc_balance
        .checked_sub(balances_before.usdc_balance)
        .unwrap_or(U256::ZERO);
    let actual_usdc = usdc_received.to_string().parse::<f64>().unwrap_or(0.0) / 1e6;

    // Calculate slippage
    let slippage = if expected_usdc > 0.0 {
        ((expected_usdc - actual_usdc) / expected_usdc) * 100.0
    } else {
        0.0
    };

    // Calculate profit/loss (rough estimate based on current price)
    // For a more accurate P/L, we'd need to track the entry price
    let mon_price_estimate = expected_usdc / params.amount_mon;
    let _ = mon_price_estimate; // Suppress unused warning

    Ok(TradeReport {
        direction: "SELL MON -> USDC via 0x".to_string(),
        amount_in: format!("{} MON", params.amount_mon),
        expected_out: format!("{:.6} USDC", expected_usdc),
        actual_out: format!("{:.6} USDC", actual_usdc),
        slippage_realized: slippage,
        mon_balance_before: balances_before.mon_balance,
        mon_balance_after: balances_after.mon_balance,
        usdc_balance_before: balances_before.usdc_balance,
        usdc_balance_after: balances_after.usdc_balance,
        profit_loss: actual_usdc, // For a sell, profit is just what we received
        execution_result: exec_result,
    })
}
