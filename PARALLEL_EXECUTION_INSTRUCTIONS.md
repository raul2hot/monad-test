# Parallel Execution Implementation Instructions

## Objective

Modify the Monad arbitrage bot to execute **both legs simultaneously** instead of sequentially. Current implementation takes ~15 seconds (7-8s per leg). Target is both legs in same or consecutive blocks (~400-800ms total).

---

## Current State

### Current Sequential Flow (SLOW - ~15 seconds)
```
1. Get starting balances
2. Approve USDC to SwapRouter (if needed)
3. Execute Uniswap BUY (USDC -> WMON)
4. WAIT for confirmation (~7-8 seconds)
5. Get MON received amount
6. Approve WMON to AllowanceHolder (if needed)
7. Get 0x quote for exact MON amount received
8. Execute 0x SELL (WMON -> USDC)
9. WAIT for confirmation (~7-8 seconds)
10. Calculate results
```

### Problem
- Sequential execution means price can move between legs
- ~15 seconds total is too slow for arbitrage
- Leg 2 amount depends on Leg 1 result (tight coupling)

---

## Target: Parallel Inventory-Based Execution

### New Flow (FAST - <1 second)
```
1. Get starting balances (WMON and USDC inventory)
2. Calculate trade amounts based on inventory
3. Fire BOTH legs simultaneously:
   - Leg A: Uniswap BUY (USDC -> WMON) 
   - Leg B: 0x SELL (WMON -> USDC)
4. Track both pending transactions
5. Wait for BOTH confirmations (parallel)
6. Calculate net P/L from balance changes
```

### Key Insight
With inventory of BOTH tokens (500 WMON + $15 USDC), legs are independent:
- Leg A spends USDC, receives WMON
- Leg B spends WMON, receives USDC
- Net effect: WMON inventory unchanged, USDC changes by spread amount

---

## Wallet Inventory (Pre-configured by User)

```
WMON: 500 (for selling via 0x)
USDC: $15 (for buying on Uniswap)
Native MON: ~1100 (for gas)
```

---

## Implementation Tasks

### Task 1: Create New Parallel Execution Function

Create `execute_parallel_arbitrage` in `src/execution.rs`:

```rust
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
    let start_time = std::time::Instant::now();

    // Get balances BEFORE
    let balances_before = crate::wallet::get_full_balances(
        provider,
        wallet_addr,
        crate::config::WMON,
        crate::config::USDC,
    ).await?;

    println!("\n========== PARALLEL ARBITRAGE ==========");
    println!("Strategy: BUY on Uniswap + SELL via 0x (simultaneous)");
    println!("USDC Input (Uniswap): {}", usdc_amount);
    println!("WMON Input (0x):      {}", wmon_amount);
    balances_before.print();

    // PRE-FLIGHT: Get 0x quote BEFORE firing transactions
    // This is needed to build the 0x transaction
    let sell_quote = zrx.get_quote(
        crate::config::WMON,
        crate::config::USDC,
        &wmon_amount.to_string(),
        &format!("{:?}", wallet_addr),
        slippage_bps,
    ).await?;

    // Calculate expected outputs for reporting
    let expected_wmon_from_uniswap = estimate_uniswap_output(usdc_amount, pool_fee);
    let expected_usdc_from_0x: f64 = sell_quote.buy_amount.parse::<f64>().unwrap_or(0.0) / 1e6;

    println!("\nExpected WMON from Uniswap: (estimated at runtime)");
    println!("Expected USDC from 0x: {:.6}", expected_usdc_from_0x);

    // ========== FIRE BOTH LEGS SIMULTANEOUSLY ==========
    let provider_clone = provider.clone();
    let wallet_clone = wallet.clone();
    
    // Spawn Leg A: Uniswap BUY
    let leg_a_handle = tokio::spawn({
        let provider = provider_clone.clone();
        let wallet = wallet_clone.clone();
        async move {
            execute_uniswap_buy_no_wait(
                &provider,
                &wallet,
                usdc_amount,
                U256::ZERO,  // min_out - set to 0 for now, can add slippage protection
                pool_fee,
            ).await
        }
    });

    // Spawn Leg B: 0x SELL
    let leg_b_handle = tokio::spawn({
        let provider = provider_clone;
        let wallet = wallet_clone;
        let quote = sell_quote.clone();
        async move {
            execute_0x_swap_no_wait(&provider, &wallet, &quote).await
        }
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

    Ok(ParallelArbReport {
        leg_a_result: leg_a,
        leg_b_result: leg_b,
        balances_before,
        balances_after,
        usdc_input: usdc_amount.to_string().parse::<f64>().unwrap_or(0.0) / 1e6,
        wmon_input: wmon_amount.to_string().parse::<f64>().unwrap_or(0.0) / 1e18,
        usdc_change,
        wmon_change,
        total_execution_time_ms: execution_time.as_millis() as u64,
        expected_usdc_from_0x,
    })
}
```

### Task 2: Create No-Wait Execution Functions

Add these functions to `src/execution.rs` that submit transactions without waiting for receipts:

```rust
/// Submit Uniswap BUY transaction - returns immediately after submission
/// Does NOT wait for confirmation
async fn execute_uniswap_buy_no_wait<P: Provider>(
    provider: &P,
    wallet: &EthereumWallet,
    usdc_amount: U256,
    min_mon_out: U256,
    pool_fee: u32,
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
    let gas_limit = 500_000u64;

    let tx = TransactionRequest::default()
        .to(router)
        .input(call.abi_encode().into())
        .from(from)
        .gas_limit(gas_limit);

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

/// Submit 0x SELL transaction - returns immediately after submission
/// Does NOT wait for confirmation
async fn execute_0x_swap_no_wait<P: Provider>(
    provider: &P,
    wallet: &EthereumWallet,
    quote: &crate::zrx::QuoteResponse,
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
        .from(from);

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
```

### Task 3: Create New Report Structs

Add these structs to `src/execution.rs`:

```rust
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
        
        println!("\n INPUTS");
        println!("  USDC Spent (Uniswap): {:.6}", self.usdc_input);
        println!("  WMON Sold (0x):       {:.6}", self.wmon_input);
        println!("  Expected USDC (0x):   {:.6}", self.expected_usdc_from_0x);
        
        println!("\n NET RESULT");
        println!("  USDC Change: {:+.6}", self.usdc_change);
        println!("  WMON Change: {:+.6}", self.wmon_change);
        
        let both_success = self.leg_a_result.success && self.leg_b_result.success;
        println!("\n STATUS: {}", if both_success { "BOTH LEGS SUCCESS" } else { "ONE OR MORE LEGS FAILED" });
        
        // Calculate effective spread if both succeeded
        if both_success && self.wmon_input > 0.0 {
            // Spread = (USDC received from 0x - USDC spent on Uniswap) / USDC spent
            // But since we're using different amounts, look at net USDC change
            let spread_pct = (self.usdc_change / self.usdc_input) * 100.0;
            println!("  Effective Spread: {:+.4}%", spread_pct);
        }
        
        println!("\n TOTAL GAS COST");
        let total_gas = self.leg_a_result.gas_used + self.leg_b_result.gas_used;
        println!("  Total Gas Used: {}", total_gas);
        
        println!("=====================================================\n");
    }
}
```

### Task 4: Add CLI Flag for Parallel Mode

Update `src/main.rs` - Add new argument:

```rust
#[derive(Parser, Debug)]
#[command(name = "monad-arb-bot")]
#[command(about = "Monad Arbitrage Bot - 0x vs Direct Pool Strategy")]
struct Args {
    // ... existing args ...

    /// Run parallel arbitrage test (both legs simultaneously)
    #[arg(long)]
    test_parallel: bool,

    /// Amount of WMON to sell via 0x in parallel mode
    #[arg(long, default_value = "10.0")]
    wmon_amount: f64,
}
```

Add handling in main():

```rust
// ========== PARALLEL ARBITRAGE TEST MODE ==========
if args.test_parallel {
    println!("PARALLEL ARBITRAGE TEST MODE");
    println!("Strategy: BUY on Uniswap + SELL via 0x (SIMULTANEOUS)");
    println!("USDC Amount (Uniswap BUY): ${}", args.usdc_amount);
    println!("WMON Amount (0x SELL): {}", args.wmon_amount);
    println!("Pool Fee: {}bps", args.pool_fee);
    println!("Slippage: {}bps ({}%)", args.slippage_bps, args.slippage_bps as f64 / 100.0);

    // Ensure approvals are done BEFORE parallel execution
    println!("\n Checking approvals...");
    
    // USDC approval to SwapRouter
    let usdc_amount = U256::from((args.usdc_amount * 1_000_000.0) as u128);
    execution::ensure_approval(
        &provider,
        &eth_wallet,
        config::USDC,
        config::UNISWAP_SWAP_ROUTER,
        usdc_amount,
    ).await?;
    
    // WMON approval to AllowanceHolder
    let wmon_amount = U256::from((args.wmon_amount * 1e18) as u128);
    execution::ensure_approval(
        &provider,
        &eth_wallet,
        config::WMON,
        config::ALLOWANCE_HOLDER,
        wmon_amount,
    ).await?;
    
    println!(" Approvals confirmed. Executing parallel arbitrage...\n");

    // Execute parallel arbitrage
    let report = execution::execute_parallel_arbitrage(
        &provider,
        &eth_wallet,
        &zrx,
        usdc_amount,
        wmon_amount,
        args.pool_fee,
        args.slippage_bps,
    ).await?;

    report.print();

    println!(" Parallel arbitrage test complete!");
    return Ok(());
}
```

### Task 5: Ensure Provider is Clone-able

The parallel execution spawns tasks that need owned providers. Check that the provider setup in main.rs supports cloning. The current setup should work:

```rust
let provider = ProviderBuilder::new()
    .wallet(eth_wallet.clone())
    .connect_http(rpc_url.parse()?);
```

If there are issues, wrap in Arc:

```rust
use std::sync::Arc;

let provider = Arc::new(
    ProviderBuilder::new()
        .wallet(eth_wallet.clone())
        .connect_http(rpc_url.parse()?)
);
```

Then update function signatures to accept `Arc<P>` or `&Arc<P>`.

---

## Pre-requisites Before Running

1. **Approvals must be set up BEFORE parallel execution**
   - USDC approved to SwapRouter: `0xfE31F71C1b106EAc32F1A19239c9a9A72ddfb900`
   - WMON approved to AllowanceHolder: `0x0000000000001fF3684f28c67538d4D072C22734`

2. **Inventory must be available**
   - WMON balance >= wmon_amount
   - USDC balance >= usdc_amount

3. **Native MON for gas** (both legs need gas)

---

## Test Commands

### Test with small amounts first:
```bash
cargo run -- --test-parallel --usdc-amount 1.0 --wmon-amount 30.0 --slippage-bps 100
```

### Test with larger amounts:
```bash
cargo run -- --test-parallel --usdc-amount 5.0 --wmon-amount 150.0 --slippage-bps 100
```

---

## Expected Output Format

```
PARALLEL ARBITRAGE TEST MODE
Strategy: BUY on Uniswap + SELL via 0x (SIMULTANEOUS)
USDC Amount (Uniswap BUY): $5
WMON Amount (0x SELL): 150
Pool Fee: 500bps
Slippage: 100bps (1.0%)

 Checking approvals...
 Approvals confirmed. Executing parallel arbitrage...

========== PARALLEL ARBITRAGE ==========
Strategy: BUY on Uniswap + SELL via 0x (simultaneous)
USDC Input (Uniswap): 5000000
WMON Input (0x):      150000000000000000000
Wallet: 0x...
  Native MON:   1100.000000 MON (gas token)
  WMON Balance: 500.000000 WMON (wrapped, tradeable)
  USDC Balance: 15.000000 USDC

Expected USDC from 0x: 4.523400

2024-XX-XX INFO Leg A (Uniswap BUY) submitted: 0x... in 45ms
2024-XX-XX INFO Leg B (0x SELL) submitted: 0x... in 52ms

=====================================================
       PARALLEL ARBITRAGE EXECUTION REPORT           
=====================================================

 TIMING
  Total Execution Time: 892ms
  Leg A Submit: 45ms | Confirm: 850ms
  Leg B Submit: 52ms | Confirm: 870ms

 LEG A: UNISWAP BUY (USDC -> WMON)
  Uniswap BUY | Tx: 0x...
    Status: SUCCESS
    Submit Latency: 45ms
    Confirmation: 850ms
    Gas: 180000 / 500000

 LEG B: 0x SELL (WMON -> USDC)
  0x SELL | Tx: 0x...
    Status: SUCCESS
    Submit Latency: 52ms
    Confirmation: 870ms
    Gas: 195000 / 210000

 BALANCES BEFORE
Wallet: 0x...
  Native MON:   1100.000000 MON
  WMON Balance: 500.000000 WMON
  USDC Balance: 15.000000 USDC

 BALANCES AFTER
Wallet: 0x...
  Native MON:   1099.950000 MON
  WMON Balance: 516.234567 WMON
  USDC Balance: 14.523400 USDC

 INPUTS
  USDC Spent (Uniswap): 5.000000
  WMON Sold (0x):       150.000000
  Expected USDC (0x):   4.523400

 NET RESULT
  USDC Change: -0.476600
  WMON Change: +16.234567

 STATUS: BOTH LEGS SUCCESS
  Effective Spread: -9.53%

 TOTAL GAS COST
  Total Gas Used: 375000
=====================================================

 Parallel arbitrage test complete!
```

---

## Key Metrics to Verify

1. **Total Execution Time**: Should be ~800-1500ms (not 15 seconds)
2. **Submit Latency**: Both legs should submit within ~100ms of each other
3. **Both Legs in Same Block**: Check block numbers on explorer - ideally same block
4. **Both Legs SUCCESS**: Both transactions confirmed successfully

---

## Notes for Implementation

1. **Do NOT add profitability checks** - we are still in "perfecting execution" phase
2. **Do NOT add conditional logic** - execute regardless of spread direction
3. **Clone the wallet/provider as needed** for spawned tasks
4. **Keep existing sequential `--test-arb` mode** - add parallel as separate flag
5. **Print detailed timing stats** - we need to measure improvement

---

## Files to Modify

| File | Changes |
|------|---------|
| `src/execution.rs` | Add `execute_parallel_arbitrage`, `execute_uniswap_buy_no_wait`, `execute_0x_swap_no_wait`, `PendingLegResult`, `ParallelArbReport` |
| `src/main.rs` | Add `--test-parallel` flag and `--wmon-amount` arg, add handling block |
| `src/wallet.rs` | No changes needed |
| `src/zrx.rs` | No changes needed |
| `src/config.rs` | No changes needed |

---

## Success Criteria

Implementation is complete when:

1. `cargo run -- --test-parallel --usdc-amount 1.0 --wmon-amount 30.0` executes without errors
2. Both transactions are submitted within 100ms of each other
3. Total execution time is under 2 seconds (vs previous 15 seconds)
4. Report shows both legs SUCCESS
5. Balance changes reflect both swaps occurred
6. Timing breakdown shows submit vs confirmation latencies
