//! Trade Execution - 0x swaps and Uniswap V3 swaps

use alloy::network::EthereumWallet;
use alloy::primitives::{Address, Bytes, U160, U256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;
use std::str::FromStr;

/// Estimate gas cost in MON for a parallel arbitrage trade
/// Returns (estimated_gas_mon, is_profitable)
pub fn estimate_trade_profitability(
    spread_pct: f64,
    trade_value_usdc: f64,
    mon_price_usdc: f64,
) -> (f64, bool) {
    // Estimated gas usage (conservative safe limits)
    const UNISWAP_GAS: u64 = 400_000;  // exactOutput needs more gas
    const ZRX_GAS: u64 = 200_000;      // Typical 0x swap (varies by routing)
    const TOTAL_GAS: u64 = UNISWAP_GAS + ZRX_GAS;

    // Monad gas price is typically ~52 gwei (0.000000052 MON per gas)
    const GAS_PRICE_GWEI: f64 = 52.0;
    const GWEI_TO_MON: f64 = 0.000000001;

    let gas_cost_mon = (TOTAL_GAS as f64) * GAS_PRICE_GWEI * GWEI_TO_MON;
    let gas_cost_usdc = gas_cost_mon * mon_price_usdc;

    // Gross profit from spread
    let gross_profit_usdc = trade_value_usdc * (spread_pct / 100.0);

    // Account for DEX fees (~0.05% Uniswap + ~0.1% 0x routing)
    let fee_cost_usdc = trade_value_usdc * 0.0015;  // ~0.15% total

    let net_profit_usdc = gross_profit_usdc - gas_cost_usdc - fee_cost_usdc;

    (gas_cost_mon, net_profit_usdc > 0.0)
}

/// Estimate trade profitability using ACTUAL gas from 0x quote
/// This should be called AFTER fetching the 0x quote to get real gas costs
/// Returns (gas_cost_mon, net_profit_usdc, is_profitable)
pub fn estimate_trade_profitability_with_quote(
    spread_pct: f64,
    trade_value_usdc: f64,
    mon_price_usdc: f64,
    actual_0x_gas: u64,
    uniswap_gas: u64,
) -> (f64, f64, bool) {
    let total_gas = actual_0x_gas + uniswap_gas;

    // Monad gas price is typically ~52 gwei (0.000000052 MON per gas)
    const GAS_PRICE_GWEI: f64 = 52.0;
    const GWEI_TO_MON: f64 = 0.000000001;

    let gas_cost_mon = (total_gas as f64) * GAS_PRICE_GWEI * GWEI_TO_MON;
    let gas_cost_usdc = gas_cost_mon * mon_price_usdc;

    // Gross profit from spread
    let gross_profit_usdc = trade_value_usdc * (spread_pct / 100.0);

    // Account for DEX fees (~0.05% Uniswap + ~0.1% 0x routing)
    let fee_cost_usdc = trade_value_usdc * 0.0015;  // ~0.15% total

    let net_profit_usdc = gross_profit_usdc - gas_cost_usdc - fee_cost_usdc;

    (gas_cost_mon, net_profit_usdc, net_profit_usdc > 0.0)
}

/// Validate 0x quoted gas against safety limits
/// Returns Ok(()) if gas is acceptable, Err with reason if rejected
pub fn validate_0x_gas(quoted_gas: u64) -> Result<(), String> {
    if quoted_gas > crate::config::MAX_0X_GAS {
        return Err(format!(
            "0x gas too high: {} > {} limit",
            quoted_gas,
            crate::config::MAX_0X_GAS
        ));
    }

    // Also check total gas (0x + Uniswap)
    const UNISWAP_GAS: u64 = 400_000;
    let total_gas = quoted_gas + UNISWAP_GAS;
    if total_gas > crate::config::MAX_TOTAL_GAS {
        return Err(format!(
            "Total gas too high: {} > {} limit (0x: {}, Uniswap: {})",
            total_gas,
            crate::config::MAX_TOTAL_GAS,
            quoted_gas,
            UNISWAP_GAS
        ));
    }

    Ok(())
}

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

    // Uniswap V3 SwapRouter02 exactOutputSingle
    // Used to buy an EXACT amount of tokenOut, spending up to amountInMaximum of tokenIn
    struct ExactOutputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 amountOut;        // The exact amount we want to receive
        uint256 amountInMaximum;  // Max we're willing to spend
        uint160 sqrtPriceLimitX96;
    }

    function exactOutputSingle(ExactOutputSingleParams calldata params)
        external payable returns (uint256 amountIn);
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

    // CRITICAL: Monad charges full gas_limit - safe reduction saves real money
    let gas_limit = 400_000u64;  // exactOutput needs more gas than exactInput
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
    // Price stats
    pub uniswap_price: f64,      // Price paid on Uniswap (USDC per MON)
    pub zrx_price: f64,          // Price received via 0x (USDC per MON)
    pub spread_pct: f64,         // Spread percentage
    pub usdc_input: f64,         // USDC spent
    pub usdc_output: f64,        // USDC received from 0x
    pub total_gas_cost: u128,    // Total gas used
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
        println!(" PRICE ANALYSIS                                      ");
        println!("   Uniswap Price: ${:.6} per MON", self.uniswap_price);
        println!("   0x Price:      ${:.6} per MON", self.zrx_price);
        println!("   Spread:        {:+.4}%", self.spread_pct);
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
        println!("   USDC In:  {:.6}", self.usdc_input);
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
        println!("   USDC Out: {:.6}", self.usdc_output);
        println!("=====================================================");
        println!(" SUMMARY                                             ");
        println!("   MON Traded:    {:.6}", mon_traded);
        println!("   USDC Before:   {:.6}", usdc_before);
        println!("   USDC After:    {:.6}", usdc_after);
        println!("   Total Gas:     {}", self.total_gas_cost);
        println!("   Profit/Loss:   {:+.6} USDC", self.profit_loss_usdc);
        println!("=====================================================");
    }
}

// ========== PARALLEL EXECUTION STRUCTS ==========

#[derive(Debug)]
pub struct PendingLegResult {
    pub tx_hash: alloy::primitives::TxHash,
    pub success: bool,
    pub gas_used: u128,
    pub gas_limit: u64,
    pub submit_latency_ms: u64,      // Time to submit tx
    pub confirmation_time_ms: u64,   // Time until confirmed
    pub leg_name: String,
}

impl PendingLegResult {
    pub fn print(&self) {
        println!("  {} | Tx: {:?}", self.leg_name, self.tx_hash);
        println!("    Status: {}", if self.success { "SUCCESS" } else { "FAILED" });
        println!("    Submit Latency: {}ms", self.submit_latency_ms);
        println!("    Confirmation: {}ms", self.confirmation_time_ms);
        println!("    Gas: {} / {}", self.gas_used, self.gas_limit);
    }
}

#[derive(Debug)]
pub struct ParallelArbReport {
    pub leg_a_result: PendingLegResult,  // Uniswap BUY
    pub leg_b_result: PendingLegResult,  // 0x SELL
    pub balances_before: crate::wallet::FullWalletInfo,
    pub balances_after: crate::wallet::FullWalletInfo,
    pub usdc_input: f64,                 // USDC spent on Uniswap
    pub wmon_input: f64,                 // WMON sold via 0x
    pub usdc_change: f64,                // Net USDC change (profit/loss)
    pub wmon_change: f64,                // Net WMON change (should be ~0)
    pub total_execution_time_ms: u64,    // Wall clock time for entire operation
    pub expected_usdc_from_0x: f64,
}

impl ParallelArbReport {
    pub fn print(&self) {
        println!("\n=====================================================");
        println!("       PARALLEL ARBITRAGE EXECUTION REPORT           ");
        println!("=====================================================");

        println!("\n TIMING");
        println!("  Total Execution Time: {}ms", self.total_execution_time_ms);
        println!("  Leg A Submit: {}ms | Confirm: {}ms",
            self.leg_a_result.submit_latency_ms,
            self.leg_a_result.confirmation_time_ms);
        println!("  Leg B Submit: {}ms | Confirm: {}ms",
            self.leg_b_result.submit_latency_ms,
            self.leg_b_result.confirmation_time_ms);

        println!("\n LEG A: UNISWAP BUY (USDC -> WMON)");
        self.leg_a_result.print();

        println!("\n LEG B: 0x SELL (WMON -> USDC)");
        self.leg_b_result.print();

        println!("\n BALANCES BEFORE");
        self.balances_before.print();

        println!("\n BALANCES AFTER");
        self.balances_after.print();

        println!("\n TRADE AMOUNTS (exactOutput ensures match)");
        println!("  WMON Bought (Uniswap): {:.6}", self.wmon_input);
        println!("  WMON Sold (0x):        {:.6}", self.wmon_input);
        println!("  USDC Spent (Uniswap):  {:.6}", self.usdc_input);
        println!("  Expected USDC (0x):    {:.6}", self.expected_usdc_from_0x);

        println!("\n NET RESULT");
        println!("  WMON Change: {:+.6} (should be ~0)", self.wmon_change);
        println!("  USDC Change: {:+.6} (this is the profit/loss)", self.usdc_change);

        let both_success = self.leg_a_result.success && self.leg_b_result.success;
        println!("\n STATUS: {}", if both_success { "BOTH LEGS SUCCESS" } else { "ONE OR MORE LEGS FAILED" });

        // Calculate effective spread if both succeeded
        if both_success && self.usdc_input > 0.0 {
            // Spread = net USDC change / USDC spent
            let spread_pct = (self.usdc_change / self.usdc_input) * 100.0;
            println!("  Effective Spread: {:+.4}%", spread_pct);
        }

        println!("\n TOTAL GAS COST");
        let total_gas = self.leg_a_result.gas_used + self.leg_b_result.gas_used;
        println!("  Total Gas Used: {}", total_gas);

        println!("=====================================================\n");
    }
}

// ========== PARALLEL EXECUTION FUNCTIONS ==========

/// Submit Uniswap BUY transaction and wait for confirmation
/// Used as a spawned task in parallel execution
async fn execute_uniswap_buy_no_wait<P: Provider>(
    provider: &P,
    wallet: &EthereumWallet,
    usdc_amount: U256,
    min_mon_out: U256,
    pool_fee: u32,
    nonce: u64,  // Pre-fetched nonce for faster submission
) -> Result<PendingLegResult> {
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
        sqrtPriceLimitX96: U160::ZERO,
    };

    let call = exactInputSingleCall { params };
    let gas_limit = 400_000u64;  // exactOutput needs more gas than exactInput

    let tx = TransactionRequest::default()
        .to(router)
        .input(call.abi_encode().into())
        .from(from)
        .gas_limit(gas_limit)
        .nonce(nonce);

    let submit_time = std::time::Instant::now();

    let pending = provider.send_transaction(tx).await?;
    let tx_hash = *pending.tx_hash();

    let submit_latency = submit_time.elapsed();

    tracing::info!("Leg A (Uniswap BUY) submitted: {:?} in {}ms", tx_hash, submit_latency.as_millis());

    // Now wait for receipt
    let receipt = pending.get_receipt().await?;
    let total_time = submit_time.elapsed();

    Ok(PendingLegResult {
        tx_hash,
        success: receipt.status(),
        gas_used: receipt.gas_used as u128,
        gas_limit,
        submit_latency_ms: submit_latency.as_millis() as u64,
        confirmation_time_ms: total_time.as_millis() as u64,
        leg_name: "Uniswap BUY".to_string(),
    })
}

/// Submit Uniswap BUY transaction using exactOutputSingle (buy EXACT amount of WMON)
/// This ensures WMON amounts match between buy and sell legs
/// Used as a spawned task in parallel execution
async fn execute_uniswap_buy_exact_output_no_wait<P: Provider>(
    provider: &P,
    wallet: &EthereumWallet,
    wmon_amount_out: U256,    // Exact WMON we want to receive
    max_usdc_in: U256,        // Maximum USDC we're willing to spend
    pool_fee: u32,
    nonce: u64,
) -> Result<PendingLegResult> {
    let from = wallet.default_signer().address();
    let router = Address::from_str(crate::config::UNISWAP_SWAP_ROUTER)?;
    let usdc = Address::from_str(crate::config::USDC)?;
    let wmon = Address::from_str(crate::config::WMON)?;

    let params = ExactOutputSingleParams {
        tokenIn: usdc,
        tokenOut: wmon,
        fee: pool_fee.try_into()?,
        recipient: from,
        amountOut: wmon_amount_out,      // Exact WMON we want
        amountInMaximum: max_usdc_in,    // Max USDC to spend (with slippage buffer)
        sqrtPriceLimitX96: U160::ZERO,
    };

    let call = exactOutputSingleCall { params };
    let gas_limit = 400_000u64;  // exactOutput needs more gas than exactInput

    let tx = TransactionRequest::default()
        .to(router)
        .input(call.abi_encode().into())
        .from(from)
        .gas_limit(gas_limit)
        .nonce(nonce);

    let submit_time = std::time::Instant::now();

    let pending = provider.send_transaction(tx).await?;
    let tx_hash = *pending.tx_hash();

    let submit_latency = submit_time.elapsed();

    tracing::info!(
        "Leg A (Uniswap BUY exactOutput) submitted: {:?} in {}ms",
        tx_hash,
        submit_latency.as_millis()
    );

    let receipt = pending.get_receipt().await?;
    let total_time = submit_time.elapsed();

    Ok(PendingLegResult {
        tx_hash,
        success: receipt.status(),
        gas_used: receipt.gas_used as u128,
        gas_limit,
        submit_latency_ms: submit_latency.as_millis() as u64,
        confirmation_time_ms: total_time.as_millis() as u64,
        leg_name: "Uniswap BUY (exactOutput)".to_string(),
    })
}

/// Submit 0x SELL transaction and wait for confirmation
/// Used as a spawned task in parallel execution
async fn execute_0x_swap_no_wait<P: Provider>(
    provider: &P,
    wallet: &EthereumWallet,
    quote: &crate::zrx::QuoteResponse,
    nonce: u64,  // Pre-fetched nonce for faster submission
) -> Result<PendingLegResult> {
    let from = wallet.default_signer().address();

    let to = Address::from_str(&quote.transaction.to)?;
    let data: Bytes = quote.transaction.data.parse()?;
    let gas_limit: u64 = quote.transaction.gas.parse()?;
    let gas_price: u128 = quote.transaction.gas_price.parse()?;
    let value: U256 = quote.transaction.value.parse()?;

    let adjusted_gas_limit = gas_limit + crate::config::GAS_BUFFER;
    let adjusted_gas_price = gas_price * crate::config::GAS_PRICE_BUMP_PCT as u128 / 100;

    let tx = TransactionRequest::default()
        .to(to)
        .input(data.into())
        .value(value)
        .gas_limit(adjusted_gas_limit)
        .gas_price(adjusted_gas_price)
        .from(from)
        .nonce(nonce);

    let submit_time = std::time::Instant::now();

    let pending = provider.send_transaction(tx).await?;
    let tx_hash = *pending.tx_hash();

    let submit_latency = submit_time.elapsed();

    tracing::info!("Leg B (0x SELL) submitted: {:?} in {}ms", tx_hash, submit_latency.as_millis());

    // Now wait for receipt
    let receipt = pending.get_receipt().await?;
    let total_time = submit_time.elapsed();

    Ok(PendingLegResult {
        tx_hash,
        success: receipt.status(),
        gas_used: receipt.gas_used as u128,
        gas_limit: adjusted_gas_limit,
        submit_latency_ms: submit_latency.as_millis() as u64,
        confirmation_time_ms: total_time.as_millis() as u64,
        leg_name: "0x SELL".to_string(),
    })
}

/// Execute parallel arbitrage: BUY on Uniswap AND SELL via 0x simultaneously
/// Requires inventory of BOTH WMON and USDC
pub async fn execute_parallel_arbitrage<P: Provider + Clone + 'static>(
    provider: &P,
    wallet: &EthereumWallet,
    zrx: &crate::zrx::ZrxClient,
    usdc_amount: U256,      // Amount of USDC to spend on Uniswap BUY
    wmon_amount: U256,      // Amount of WMON to sell via 0x
    pool_fee: u32,          // Uniswap pool fee tier
    slippage_bps: u32,      // Slippage for both legs
) -> Result<ParallelArbReport> {
    let wallet_addr = wallet.default_signer().address();

    // PRE-FLIGHT: Get 0x quote BEFORE firing transactions
    // This is needed to build the 0x transaction
    let sell_quote = zrx.get_quote(
        crate::config::WMON,
        crate::config::USDC,
        &wmon_amount.to_string(),
        &format!("{:?}", wallet_addr),
        slippage_bps,
    ).await?;

    // Delegate to the version that accepts a pre-fetched quote
    execute_parallel_arbitrage_with_quote(
        provider,
        wallet,
        usdc_amount,
        wmon_amount,
        pool_fee,
        sell_quote,
    ).await
}

/// Execute parallel arbitrage with a pre-fetched 0x quote
/// This avoids double-fetching the quote when gas validation is done first
///
/// CRITICAL FIX: Uses exactOutputSingle to ensure WMON amounts match between legs
/// - Leg A: Buy EXACTLY wmon_amount on Uniswap (spend variable USDC)
/// - Leg B: Sell EXACTLY wmon_amount via 0x (receive USDC)
/// Result: WMON inventory unchanged, profit/loss is purely in USDC
pub async fn execute_parallel_arbitrage_with_quote<P: Provider + Clone + 'static>(
    provider: &P,
    wallet: &EthereumWallet,
    _usdc_amount: U256,     // Deprecated: now calculated from quote with buffer
    wmon_amount: U256,      // Amount of WMON to buy/sell (MUST MATCH on both legs)
    pool_fee: u32,          // Uniswap pool fee tier
    sell_quote: crate::zrx::QuoteResponse,  // Pre-fetched 0x quote
) -> Result<ParallelArbReport> {
    let wallet_addr = wallet.default_signer().address();
    let start_time = std::time::Instant::now();

    // PRE-FETCH: Get current nonce before any operations
    let current_nonce = provider.get_transaction_count(wallet_addr).await?;
    let leg_a_nonce = current_nonce;      // Uniswap BUY
    let leg_b_nonce = current_nonce + 1;  // 0x SELL

    tracing::info!("Pre-fetched nonces: Leg A = {}, Leg B = {}", leg_a_nonce, leg_b_nonce);

    // Get balances BEFORE
    let balances_before = crate::wallet::get_full_balances(
        provider,
        wallet_addr,
        crate::config::WMON,
        crate::config::USDC,
    ).await?;

    // Calculate expected outputs for reporting
    let expected_usdc_from_0x: f64 = sell_quote.buy_amount.parse::<f64>().unwrap_or(0.0) / 1e6;

    // CRITICAL: Calculate max USDC to spend with slippage buffer (2%)
    // This is used for exactOutputSingle - we specify the exact WMON we want,
    // and set a maximum USDC we're willing to spend
    let usdc_from_quote: u128 = sell_quote.buy_amount.parse().unwrap_or(0);
    let max_usdc_with_buffer = U256::from((usdc_from_quote as f64 * 1.02) as u128);

    println!("\n========== PARALLEL ARBITRAGE (EXACT OUTPUT) ==========");
    println!("Strategy: BUY EXACT WMON on Uniswap + SELL EXACT WMON via 0x");
    println!("WMON Amount (both legs): {}", wmon_amount);
    println!("Max USDC (Uniswap):      {} (with 2% buffer)", max_usdc_with_buffer);
    println!("Expected USDC (0x):      {:.6}", expected_usdc_from_0x);
    balances_before.print();

    // ========== FIRE BOTH LEGS SIMULTANEOUSLY ==========
    let provider_a = provider.clone();
    let wallet_a = wallet.clone();
    let wmon_out = wmon_amount;           // Exact WMON we want from Uniswap
    let max_usdc = max_usdc_with_buffer;  // Max USDC to spend
    let fee = pool_fee;
    let nonce_a = leg_a_nonce;

    let provider_b = provider.clone();
    let wallet_b = wallet.clone();
    let quote = sell_quote;
    let nonce_b = leg_b_nonce;

    // Spawn Leg A: Uniswap BUY using exactOutputSingle
    // This buys EXACTLY wmon_amount, spending up to max_usdc
    let leg_a_handle = tokio::spawn(async move {
        execute_uniswap_buy_exact_output_no_wait(
            &provider_a,
            &wallet_a,
            wmon_out,      // Exact WMON we want to receive
            max_usdc,      // Max USDC we're willing to spend
            fee,
            nonce_a,
        ).await
    });

    // Spawn Leg B: 0x SELL
    let leg_b_handle = tokio::spawn(async move {
        execute_0x_swap_no_wait(&provider_b, &wallet_b, &quote, nonce_b).await  // Pass pre-fetched nonce
    });

    // Wait for BOTH to complete
    let (leg_a_result, leg_b_result) = tokio::join!(leg_a_handle, leg_b_handle);

    let leg_a = leg_a_result??;
    let leg_b = leg_b_result??;

    let execution_time = start_time.elapsed();

    // Get balances AFTER
    let balances_after = crate::wallet::get_full_balances(
        provider,
        wallet_addr,
        crate::config::WMON,
        crate::config::USDC,
    ).await?;

    // Calculate results
    let usdc_before_f64 = balances_before.usdc_balance.to_string().parse::<f64>().unwrap_or(0.0) / 1e6;
    let usdc_after_f64 = balances_after.usdc_balance.to_string().parse::<f64>().unwrap_or(0.0) / 1e6;
    let wmon_before_f64 = balances_before.wmon_balance.to_string().parse::<f64>().unwrap_or(0.0) / 1e18;
    let wmon_after_f64 = balances_after.wmon_balance.to_string().parse::<f64>().unwrap_or(0.0) / 1e18;

    let usdc_change = usdc_after_f64 - usdc_before_f64;
    let wmon_change = wmon_after_f64 - wmon_before_f64;

    // Calculate actual USDC spent on Uniswap by looking at the balance difference
    // This is more accurate than using max_usdc_with_buffer since exactOutputSingle
    // only spends what's needed
    let usdc_spent_on_uniswap = usdc_before_f64 - usdc_after_f64 + expected_usdc_from_0x;

    Ok(ParallelArbReport {
        leg_a_result: leg_a,
        leg_b_result: leg_b,
        balances_before,
        balances_after,
        usdc_input: usdc_spent_on_uniswap.max(0.0),  // Actual USDC spent (not max)
        wmon_input: wmon_amount.to_string().parse::<f64>().unwrap_or(0.0) / 1e18,
        usdc_change,
        wmon_change,
        total_execution_time_ms: execution_time.as_millis() as u64,
        expected_usdc_from_0x,
    })
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

    // Convert to human-readable values
    let usdc_input_f64 = usdc_amount.to_string().parse::<f64>().unwrap_or(0.0) / 1e6;
    let mon_received_f64 = mon_received.to_string().parse::<f64>().unwrap_or(0.0) / 1e18;

    // Calculate USDC received from 0x sell
    let usdc_output_f64 = balances_after
        .usdc_balance
        .saturating_sub(balances_before.usdc_balance.saturating_sub(usdc_amount))
        .to_string()
        .parse::<f64>()
        .unwrap_or(0.0)
        / 1e6;

    // Calculate prices
    let uniswap_price = if mon_received_f64 > 0.0 {
        usdc_input_f64 / mon_received_f64
    } else {
        0.0
    };

    let zrx_price = if mon_received_f64 > 0.0 {
        usdc_output_f64 / mon_received_f64
    } else {
        0.0
    };

    // Calculate spread: (0x price - Uniswap price) / Uniswap price * 100
    // Positive spread = 0x pays more than Uniswap charged (profitable)
    // Negative spread = 0x pays less than Uniswap charged (loss)
    let spread_pct = if uniswap_price > 0.0 {
        ((zrx_price - uniswap_price) / uniswap_price) * 100.0
    } else {
        0.0
    };

    let usdc_before_f64 = balances_before
        .usdc_balance
        .to_string()
        .parse::<f64>()
        .unwrap_or(0.0)
        / 1e6;
    let usdc_after_f64 = balances_after
        .usdc_balance
        .to_string()
        .parse::<f64>()
        .unwrap_or(0.0)
        / 1e6;
    let profit_loss = usdc_after_f64 - usdc_before_f64;

    let total_gas_cost = buy_result.gas_used + sell_result.gas_used;

    Ok(ArbTradeReport {
        buy_result,
        sell_result,
        usdc_before: balances_before.usdc_balance,
        usdc_after: balances_after.usdc_balance,
        mon_traded: mon_received,
        profit_loss_usdc: profit_loss,
        uniswap_price,
        zrx_price,
        spread_pct,
        usdc_input: usdc_input_f64,
        usdc_output: usdc_output_f64,
        total_gas_cost,
    })
}
