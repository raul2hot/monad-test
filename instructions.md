# MEV Bot Optimization Instructions for Claude Code Opus

## Project Overview
Monad mainnet arbitrage bot in Rust. We execute atomic DEX-to-DEX arbitrage via a smart contract in a single transaction.

## Critical Goal
**Reduce `atomic-arb` execution time from ~600ms to <300ms**

Command being optimized:
```bash
cargo run -- atomic-arb --sell-dex uniswap --buy-dex pancakeswap1 --amount 0.1 --force
```

## Current Architecture

### Key Files (all provided in context)
- `src/execution/atomic_arb.rs` - **PRIMARY TARGET** - Atomic arb execution
- `src/main.rs` - CLI entry point, `run_atomic_arb()` function
- `src/config.rs` - Router/pool addresses, contract address
- `src/nonce.rs` - Nonce management (already optimized)
- `contracts/src/MonadAtomicArb.sol` - On-chain contract

### Current Execution Flow (atomic-arb)
```
1. Parse CLI args
2. Connect to RPC
3. PARALLEL: gas_price + nonce_init + price_fetch  (~100ms) ✓ Already optimized
4. Create provider_with_signer
5. Get router configs
6. Get prices from fetched data
7. execute_atomic_arb():
   a. Query contract balances BEFORE        (~50-100ms) ❌ BOTTLENECK
   b. Calculate amounts
   c. Build sell calldata
   d. Build executeArb call
   e. eth_estimateGas                       (~50-100ms) ❌ BOTTLENECK  
   f. Build transaction
   g. Send transaction                      (~20-50ms)
   h. Wait for receipt (20ms polling)       (~100-200ms) ❌ BOTTLENECK
   i. Query contract balances AFTER         (~50-100ms) ❌ BOTTLENECK
   j. Calculate profit
8. Print result
```

## Identified Bottlenecks & Solutions

### 1. Pre-TX Balance Query (~50-100ms)
**Current:** `query_contract_balances()` before execution
**Solution:** Skip in TURBO mode - we only need balances for profit calculation, not execution

### 2. Gas Estimation (~50-100ms)
**Current:** `provider.estimate_gas()` call
**Solution:** Hardcode gas limit for atomic arb (it's predictable ~350-400k gas)
- Uniswap + PancakeSwap: ~380k
- With LFJ: ~420k
- Safe limit: 450k + 50k buffer = 500k

### 3. Receipt Polling (20ms interval)
**Current:** 20ms polling in `wait_for_receipt_fast()`
**Solution:** 2ms polling for TURBO mode (Monad blocks are fast)

### 4. Post-TX Balance Query (~50-100ms)
**Current:** `query_contract_balances()` after execution
**Solution:** Skip in TURBO mode - derive profit from transaction receipt/events or defer

### 5. Provider Creation
**Current:** Created fresh in `run_atomic_arb()`
**Solution:** Already at function start, no issue

## Implementation Plan

### Option A: Add `--turbo` flag to atomic-arb command
```rust
// In Commands::AtomicArb
#[arg(long, default_value = "false")]
turbo: bool,  // Skip non-essential operations for speed
```

### Option B: Create new `execute_atomic_arb_turbo()` function
Separate function that:
1. Skips pre-balance query
2. Uses hardcoded gas limit
3. Uses 2ms receipt polling
4. Skips post-balance query (returns estimated profit)

### Option C: Conditional logic in existing function
Add `turbo: bool` parameter to `execute_atomic_arb()` and branch internally.

**Recommended: Option B** - Cleaner separation, no risk of breaking existing flow

## Code Changes Required

### 1. `src/execution/atomic_arb.rs`

Add new function:
```rust
/// TURBO MODE: Execute atomic arb with minimal latency
/// Skips: pre-balance query, gas estimation, post-balance query
/// Uses: hardcoded gas limit, 2ms receipt polling
pub async fn execute_atomic_arb_turbo<P: Provider>(
    provider_with_signer: &P,
    signer_address: Address,
    sell_router: &RouterConfig,
    buy_router: &RouterConfig,
    amount: f64,
    sell_price: f64,
    buy_price: f64,
    slippage_bps: u32,
    gas_price: u128,
) -> Result<AtomicArbResult> {
    let start = std::time::Instant::now();
    
    // NO pre-balance query - save ~80ms
    
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
        sell_router, SwapDirection::Sell, wmon_in_wei, min_usdc_out_wei,
    )?;
    
    let buy_pool_fee_u24: Uint<24, 1> = Uint::from(buy_router.pool_fee);
    
    // Use unchecked version (no on-chain profit check = less gas)
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
    
    // HARDCODED gas limit - save ~80ms from estimation
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
    
    let pending = provider_with_signer.send_transaction(tx).await?;
    let tx_hash = *pending.tx_hash();
    
    // TURBO receipt polling - 2ms instead of 20ms
    let receipt = wait_for_receipt_turbo(provider_with_signer, tx_hash).await?;
    
    let exec_time = start.elapsed().as_millis();
    
    // NO post-balance query - return estimated profit
    let estimated_profit = expected_wmon_back - amount;
    let estimated_profit_bps = (estimated_profit / amount * 10000.0) as i32;
    
    let gas_cost_mon = (gas_limit as f64 * gas_price as f64) / 1e18;
    
    Ok(AtomicArbResult {
        tx_hash: format!("{:?}", tx_hash),
        success: receipt.status(),
        profit_wmon: estimated_profit,  // Estimated, not actual
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

/// TURBO: 2ms polling for receipt
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
```

### 2. `src/main.rs`

Update `Commands::AtomicArb`:
```rust
Some(Commands::AtomicArb { sell_dex, buy_dex, amount, slippage, min_profit_bps, force, turbo }) => {
    if turbo {
        run_atomic_arb_turbo(&sell_dex, &buy_dex, amount, slippage).await
    } else {
        run_atomic_arb(&sell_dex, &buy_dex, amount, slippage, min_profit_bps, force).await
    }
}
```

Add new function:
```rust
async fn run_atomic_arb_turbo(sell_dex: &str, buy_dex: &str, amount: f64, slippage: u32) -> Result<()> {
    let total_start = std::time::Instant::now();
    
    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");
    
    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url.clone());
    
    let signer = PrivateKeySigner::from_str(&private_key)?;
    let signer_address = signer.address();
    
    // PARALLEL init - already optimized
    let (gas_result, nonce_result, prices_result) = tokio::join!(
        provider.get_gas_price(),
        init_nonce(&provider, signer_address),
        get_current_prices(&provider)
    );
    
    let gas_price = gas_result.unwrap_or(50_000_000_000); // 50 gwei default
    nonce_result?;
    let prices = prices_result?;
    
    // Create provider with signer
    let wallet = EthereumWallet::from(signer);
    let provider_with_signer = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(url);
    
    // Get routers
    let sell_router = get_router_by_name(sell_dex)
        .ok_or_else(|| eyre::eyre!("Unknown sell DEX: {}", sell_dex))?;
    let buy_router = get_router_by_name(buy_dex)
        .ok_or_else(|| eyre::eyre!("Unknown buy DEX: {}", buy_dex))?;
    
    // Get prices
    let sell_price = prices.iter()
        .find(|p| p.pool_name.to_lowercase() == sell_dex.to_lowercase())
        .ok_or_else(|| eyre::eyre!("No price for {}", sell_dex))?.price;
    let buy_price = prices.iter()
        .find(|p| p.pool_name.to_lowercase() == buy_dex.to_lowercase())
        .ok_or_else(|| eyre::eyre!("No price for {}", buy_dex))?.price;
    
    println!("TURBO MODE | {} -> {}", sell_dex, buy_dex);
    
    let result = execute_atomic_arb_turbo(
        &provider_with_signer,
        signer_address,
        &sell_router,
        &buy_router,
        amount,
        sell_price,
        buy_price,
        slippage,
        gas_price,
    ).await?;
    
    println!("TX: {}", result.tx_hash);
    println!("Success: {}", result.success);
    println!("Estimated Profit: {:.6} WMON ({} bps)", result.profit_wmon, result.profit_bps);
    println!("TOTAL TIME: {:?}", total_start.elapsed());
    
    Ok(())
}
```

### 3. Update `src/execution/mod.rs`
```rust
pub use atomic_arb::{
    execute_atomic_arb, 
    execute_atomic_arb_turbo,  // Add this
    AtomicArbResult, 
    print_atomic_arb_result, 
    query_contract_balances
};
```

## Expected Timing Breakdown (TURBO)

| Step | Current | TURBO | Saved |
|------|---------|-------|-------|
| Parallel init | 100ms | 100ms | 0ms |
| Pre-balance query | 80ms | 0ms | 80ms |
| Gas estimation | 80ms | 0ms | 80ms |
| Build TX | 5ms | 5ms | 0ms |
| Send TX | 30ms | 30ms | 0ms |
| Wait receipt | 150ms | 80ms | 70ms (2ms poll) |
| Post-balance query | 80ms | 0ms | 80ms |
| **TOTAL** | **~600ms** | **~215ms** | **~310ms** |

## Testing Commands

```bash
# Standard mode (existing)
cargo run -- atomic-arb --sell-dex uniswap --buy-dex pancakeswap1 --amount 0.1 --force

# TURBO mode (new)
cargo run -- atomic-arb --sell-dex uniswap --buy-dex pancakeswap1 --amount 0.1 --turbo
```

## Important Notes

1. **TURBO mode trades accuracy for speed** - profit is estimated, not measured
2. **Gas limit is hardcoded** - if a new router uses more gas, update `TURBO_GAS_LIMIT`
3. **No profit verification** - use `--force` behavior implicitly
4. **For production**, may want to verify profit after-the-fact with balance query

## Files to Modify
1. `src/execution/atomic_arb.rs` - Add `execute_atomic_arb_turbo()` and `wait_for_receipt_turbo()`
2. `src/main.rs` - Add `--turbo` flag and `run_atomic_arb_turbo()`
3. `src/execution/mod.rs` - Export new function

## Success Criteria
- `--turbo` mode completes in <300ms consistently
- Transaction still succeeds on-chain
- No regression in standard mode