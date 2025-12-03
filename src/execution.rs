//! Trade Execution - 0x swaps and Uniswap V3 swaps

use alloy::network::EthereumWallet;
use alloy::primitives::{Address, Bytes, U160, U256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;
use std::str::FromStr;

sol! {
    function approve(address spender, uint256 amount) external returns (bool);

    // WMON (Wrapped MON) - same interface as WETH
    function deposit() external payable;
    function withdraw(uint256 amount) external;

    // Uniswap V3 SwapRouter02 exactInputSingle
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    function exactInputSingle(ExactInputSingleParams calldata params)
        external payable returns (uint256 amountOut);
}

#[derive(Debug)]
pub struct ExecutionResult {
    pub tx_hash: alloy::primitives::TxHash,
    pub success: bool,
    pub gas_used: u128,
    pub gas_limit: u64,
    pub execution_time_ms: u64,
}

impl ExecutionResult {
    pub fn print(&self) {
        println!("\n========== EXECUTION RESULT ==========");
        println!("  Tx Hash:       {:?}", self.tx_hash);
        println!(
            "  Status:        {}",
            if self.success {
                "SUCCESS"
            } else {
                "FAILED"
            }
        );
        println!("  Gas Used:      {}", self.gas_used);
        println!("  Gas Limit:     {}", self.gas_limit);
        println!("  Exec Time:     {}ms", self.execution_time_ms);
        println!("=======================================\n");
    }
}

/// Approve token spending (one-time per token/spender)
pub async fn ensure_approval<P: Provider>(
    provider: &P,
    wallet: &EthereumWallet,
    token: &str,
    spender: &str, // AllowanceHolder or SwapRouter address
    required_amount: U256,
) -> Result<Option<alloy::primitives::TxHash>> {
    let token_addr = Address::from_str(token)?;
    let spender_addr = Address::from_str(spender)?;
    let owner = wallet.default_signer().address();

    // Check current allowance
    let current = crate::wallet::check_allowance(provider, token, owner, spender).await?;

    if current >= required_amount {
        tracing::info!("Allowance sufficient: {} >= {}", current, required_amount);
        return Ok(None);
    }

    tracing::info!("Approving {} to spend tokens...", spender);

    // Approve max amount (U256::MAX)
    let call = approveCall {
        spender: spender_addr,
        amount: U256::MAX,
    };

    let tx = TransactionRequest::default()
        .to(token_addr)
        .input(call.abi_encode().into())
        .from(owner);

    let pending = provider.send_transaction(tx).await?;
    let tx_hash = *pending.tx_hash();

    tracing::info!("Approval tx submitted: {:?}", tx_hash);

    // Wait for confirmation
    let _receipt = pending.get_receipt().await?;

    tracing::info!("Approval tx confirmed: {:?}", tx_hash);
    Ok(Some(tx_hash))
}

/// Wrap native MON to WMON (ERC-20)
/// Sends native MON to WMON contract, receives WMON tokens
pub async fn wrap_mon<P: Provider>(
    provider: &P,
    wallet: &EthereumWallet,
    amount: U256,
) -> Result<ExecutionResult> {
    let from = wallet.default_signer().address();
    let wmon_addr = Address::from_str(crate::config::WMON)?;

    // Call deposit() with native MON as value
    let call = depositCall {};

    let gas_limit = 100_000u64; // Increased gas limit for wrapping
    let tx = TransactionRequest::default()
        .to(wmon_addr)
        .input(call.abi_encode().into())
        .value(amount)
        .gas_limit(gas_limit)
        .from(from);

    let start_time = std::time::Instant::now();

    let pending = provider.send_transaction(tx).await?;
    let tx_hash = *pending.tx_hash();

    tracing::info!("Wrap tx submitted: {:?}", tx_hash);

    let receipt = pending.get_receipt().await?;
    let elapsed = start_time.elapsed();

    Ok(ExecutionResult {
        tx_hash,
        success: receipt.status(),
        gas_used: receipt.gas_used as u128,
        gas_limit,
        execution_time_ms: elapsed.as_millis() as u64,
    })
}

/// Unwrap WMON back to native MON
pub async fn unwrap_mon<P: Provider>(
    provider: &P,
    wallet: &EthereumWallet,
    amount: U256,
) -> Result<ExecutionResult> {
    let from = wallet.default_signer().address();
    let wmon_addr = Address::from_str(crate::config::WMON)?;

    let call = withdrawCall { amount };

    let gas_limit = 100_000u64; // Increased gas limit for unwrapping
    let tx = TransactionRequest::default()
        .to(wmon_addr)
        .input(call.abi_encode().into())
        .gas_limit(gas_limit)
        .from(from);

    let start_time = std::time::Instant::now();

    let pending = provider.send_transaction(tx).await?;
    let tx_hash = *pending.tx_hash();

    tracing::info!("Unwrap tx submitted: {:?}", tx_hash);

    let receipt = pending.get_receipt().await?;
    let elapsed = start_time.elapsed();

    Ok(ExecutionResult {
        tx_hash,
        success: receipt.status(),
        gas_used: receipt.gas_used as u128,
        gas_limit,
        execution_time_ms: elapsed.as_millis() as u64,
    })
}

/// Execute a swap via 0x using the quote response
pub async fn execute_0x_swap<P: Provider>(
    provider: &P,
    wallet: &EthereumWallet,
    quote: &crate::zrx::QuoteResponse,
) -> Result<ExecutionResult> {
    let from = wallet.default_signer().address();

    // Parse transaction fields from quote
    let to = Address::from_str(&quote.transaction.to)?;
    let data: Bytes = quote.transaction.data.parse()?;
    let gas_limit: u64 = quote.transaction.gas.parse()?;
    let gas_price: u128 = quote.transaction.gas_price.parse()?;
    let value: U256 = quote.transaction.value.parse()?;

    // CRITICAL: Monad charges full gas_limit, not gas_used
    // Add minimal buffer, don't over-estimate
    let adjusted_gas_limit = gas_limit + crate::config::GAS_BUFFER;

    // Add 10% priority bump for faster inclusion
    let adjusted_gas_price = gas_price * crate::config::GAS_PRICE_BUMP_PCT as u128 / 100;

    tracing::info!("Executing 0x swap:");
    tracing::info!("  To: {:?}", to);
    tracing::info!(
        "  Gas Limit: {} (buffered from {})",
        adjusted_gas_limit,
        gas_limit
    );
    tracing::info!("  Gas Price: {} (10% bump)", adjusted_gas_price);
    tracing::info!("  Value: {}", value);

    let tx = TransactionRequest::default()
        .to(to)
        .input(data.into())
        .value(value)
        .gas_limit(adjusted_gas_limit)
        .gas_price(adjusted_gas_price)
        .from(from);

    let start_time = std::time::Instant::now();

    let pending = provider.send_transaction(tx).await?;
    let tx_hash = *pending.tx_hash();

    tracing::info!("Transaction submitted: {:?}", tx_hash);

    // Wait for confirmation
    let receipt = pending.get_receipt().await?;

    let elapsed = start_time.elapsed();

    let success = receipt.status();
    let gas_used = receipt.gas_used;

    Ok(ExecutionResult {
        tx_hash,
        success,
        gas_used: gas_used as u128,
        gas_limit: adjusted_gas_limit,
        execution_time_ms: elapsed.as_millis() as u64,
    })
}

/// Execute BUY on Uniswap V3 (USDC -> MON)
pub async fn execute_uniswap_buy<P: Provider>(
    provider: &P,
    wallet: &EthereumWallet,
    usdc_amount: U256,
    min_mon_out: U256,
    pool_fee: u32, // e.g., 500 for 0.05%, 3000 for 0.3%
) -> Result<ExecutionResult> {
    let from = wallet.default_signer().address();
    let router = Address::from_str(crate::config::UNISWAP_SWAP_ROUTER)?;
    let usdc = Address::from_str(crate::config::USDC)?;
    let wmon = Address::from_str(crate::config::WMON)?;

    let params = ExactInputSingleParams {
        tokenIn: usdc,
        tokenOut: wmon,
        fee: pool_fee.try_into()?,
        recipient: from,
        amountIn: usdc_amount,
        amountOutMinimum: min_mon_out,
        sqrtPriceLimitX96: U160::ZERO, // No price limit
    };

    let call = exactInputSingleCall { params };

    // CRITICAL: Monad charges full gas_limit - but need enough for swap
    let gas_limit = 500_000u64;  // Increased from 200k - Uniswap V3 swaps can need more
    let tx = TransactionRequest::default()
        .to(router)
        .input(call.abi_encode().into())
        .from(from)
        .gas_limit(gas_limit);

    let start_time = std::time::Instant::now();

    let pending = provider.send_transaction(tx).await?;
    let tx_hash = *pending.tx_hash();

    tracing::info!("Uniswap BUY tx submitted: {:?}", tx_hash);

    let receipt = pending.get_receipt().await?;
    let elapsed = start_time.elapsed();

    if !receipt.status() {
        tracing::error!("Uniswap BUY FAILED! Tx: {:?}", tx_hash);
        tracing::error!("Gas used: {} / {} (out of gas: {})", receipt.gas_used, gas_limit, receipt.gas_used >= gas_limit as u64);
        tracing::error!("Try different pool fee: 500, 3000, or 10000");
    }

    Ok(ExecutionResult {
        tx_hash,
        success: receipt.status(),
        gas_used: receipt.gas_used as u128,
        gas_limit,
        execution_time_ms: elapsed.as_millis() as u64,
    })
}

#[derive(Debug)]
pub struct ArbTradeReport {
    pub buy_result: ExecutionResult,
    pub sell_result: ExecutionResult,
    pub usdc_before: U256,
    pub usdc_after: U256,
    pub mon_traded: U256,
    pub profit_loss_usdc: f64,
}

impl ArbTradeReport {
    pub fn print(&self) {
        let usdc_before = self.usdc_before.to_string().parse::<f64>().unwrap_or(0.0) / 1e6;
        let usdc_after = self.usdc_after.to_string().parse::<f64>().unwrap_or(0.0) / 1e6;
        let mon_traded = self.mon_traded.to_string().parse::<f64>().unwrap_or(0.0) / 1e18;

        println!("\n=====================================================");
        println!("     ARBITRAGE EXECUTION REPORT                      ");
        println!("     BUY on Uniswap -> SELL via 0x                   ");
        println!("=====================================================");
        println!(" BUY LEG (Uniswap)                                   ");
        println!("   Tx:     {:?}", self.buy_result.tx_hash);
        println!(
            "   Status: {}",
            if self.buy_result.success {
                "SUCCESS"
            } else {
                "FAILED"
            }
        );
        println!(
            "   Gas:    {} / {}",
            self.buy_result.gas_used, self.buy_result.gas_limit
        );
        println!("   Time:   {}ms", self.buy_result.execution_time_ms);
        println!("=====================================================");
        println!(" SELL LEG (0x)                                       ");
        println!("   Tx:     {:?}", self.sell_result.tx_hash);
        println!(
            "   Status: {}",
            if self.sell_result.success {
                "SUCCESS"
            } else {
                "FAILED"
            }
        );
        println!(
            "   Gas:    {} / {}",
            self.sell_result.gas_used, self.sell_result.gas_limit
        );
        println!("   Time:   {}ms", self.sell_result.execution_time_ms);
        println!("=====================================================");
        println!(" SUMMARY                                             ");
        println!("   MON Traded:    {:.6}", mon_traded);
        println!("   USDC Before:   {:.6}", usdc_before);
        println!("   USDC After:    {:.6}", usdc_after);
        println!("   Profit/Loss:   {:.6} USDC", self.profit_loss_usdc);
        println!("=====================================================");
    }
}

/// Execute full arbitrage: BUY on Uniswap, SELL via 0x
/// This is the SRS-compliant implementation
pub async fn execute_arbitrage<P: Provider>(
    provider: &P,
    wallet: &EthereumWallet,
    zrx: &crate::zrx::ZrxClient,
    usdc_amount: U256,  // Starting USDC amount
    pool_fee: u32,      // Uniswap pool fee tier (500, 3000, 10000)
    slippage_bps: u32,  // Slippage for 0x sell leg
) -> Result<ArbTradeReport> {
    let wallet_addr = wallet.default_signer().address();

    // Get balances before
    let balances_before = crate::wallet::get_balances(
        provider,
        wallet_addr,
        crate::config::WMON,
        crate::config::USDC,
    )
    .await?;

    println!("\n Executing Arbitrage: BUY on Uniswap -> SELL via 0x");
    balances_before.print();

    // ========== LEG 1: BUY MON on Uniswap ==========
    println!("\n LEG 1: Buying MON on Uniswap...");

    // Ensure USDC approval to SwapRouter02
    ensure_approval(
        provider,
        wallet,
        crate::config::USDC,
        crate::config::UNISWAP_SWAP_ROUTER,
        usdc_amount,
    )
    .await?;

    // Calculate minimum MON out (with slippage protection)
    // For now, use 0 and rely on the trade report to show actual slippage
    let min_mon_out = U256::ZERO; // TODO: Calculate from pool price

    let buy_result = execute_uniswap_buy(provider, wallet, usdc_amount, min_mon_out, pool_fee).await?;

    if !buy_result.success {
        return Err(eyre::eyre!("Uniswap BUY leg failed"));
    }
    buy_result.print();

    // Get MON balance after buy
    let mid_balances = crate::wallet::get_balances(
        provider,
        wallet_addr,
        crate::config::WMON,
        crate::config::USDC,
    )
    .await?;
    let mon_received = mid_balances
        .mon_balance
        .saturating_sub(balances_before.mon_balance);

    let mon_human = mon_received.to_string().parse::<f64>().unwrap_or(0.0) / 1e18;
    println!("  MON received: {:.6}", mon_human);

    // ========== LEG 2: SELL MON via 0x ==========
    println!("\n LEG 2: Selling MON via 0x...");

    // Ensure WMON approval to AllowanceHolder
    ensure_approval(
        provider,
        wallet,
        crate::config::WMON,
        crate::config::ALLOWANCE_HOLDER,
        mon_received,
    )
    .await?;

    // Get 0x quote for selling MON
    let sell_quote = zrx
        .get_quote(
            crate::config::WMON,
            crate::config::USDC,
            &mon_received.to_string(),
            &format!("{:?}", wallet_addr),
            slippage_bps,
        )
        .await?;

    let usdc_expected: f64 = sell_quote.buy_amount.parse::<f64>().unwrap_or(0.0) / 1e6;
    println!("  Expected USDC: {:.6}", usdc_expected);

    // Execute 0x sell
    let sell_result = execute_0x_swap(provider, wallet, &sell_quote).await?;
    sell_result.print();

    // ========== Calculate Results ==========
    let balances_after = crate::wallet::get_balances(
        provider,
        wallet_addr,
        crate::config::WMON,
        crate::config::USDC,
    )
    .await?;

    let usdc_before = balances_before
        .usdc_balance
        .to_string()
        .parse::<f64>()
        .unwrap_or(0.0)
        / 1e6;
    let usdc_after = balances_after
        .usdc_balance
        .to_string()
        .parse::<f64>()
        .unwrap_or(0.0)
        / 1e6;
    let profit_loss = usdc_after - usdc_before;

    Ok(ArbTradeReport {
        buy_result,
        sell_result,
        usdc_before: balances_before.usdc_balance,
        usdc_after: balances_after.usdc_balance,
        mon_traded: mon_received,
        profit_loss_usdc: profit_loss,
    })
}
