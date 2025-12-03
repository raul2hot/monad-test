# Priority 1 & 3 Implementation Instructions

## Project Context

Rust-based Monad arbitrage bot. Codebase is in working state with:
- Parallel execution implemented (`execute_parallel_arbitrage` in `src/execution.rs`)
- `--test-parallel` flag working
- Detection loop running in default mode (no flags)

Wallet address: `0xad0e53732ae6ac04edc36f9f81c4bfc9aa344b2b`

---

## Priority 1: Auto-Execute Mode

### Objective

Add `--spread-threshold` flag to the existing monitoring mode. When spread exceeds threshold, automatically trigger `execute_parallel_arbitrage`.

### CLI Changes (`src/main.rs`)

Add this argument to the `Args` struct:

```rust
/// Spread threshold (%) to trigger auto-execution. 0 = monitoring only (default)
#[arg(long, default_value = "0.0")]
spread_threshold: f64,
```

### Monitoring Loop Changes (`src/main.rs`)

Locate the existing monitoring loop (starts around line 270 with `loop { poll_interval.tick().await;`).

**Current behavior:**
- Prints prices every 2 seconds
- Calls `check_arbitrage()` which prints opportunity if spread > 0.5%
- Does NOT execute

**New behavior when `spread_threshold > 0`:**

1. Before the loop starts, ensure approvals are done once:

```rust
// Before loop, if auto-execute enabled
if args.spread_threshold > 0.0 {
    println!("AUTO-EXECUTE ENABLED");
    println!("Spread Threshold: {}%", args.spread_threshold);
    
    // Pre-approve USDC to SwapRouter (for Uniswap BUY leg)
    let usdc_approval_amount = U256::from(100_000_000u128); // 100 USDC
    execution::ensure_approval(
        &provider,
        &eth_wallet,
        config::USDC,
        config::UNISWAP_SWAP_ROUTER,
        usdc_approval_amount,
    ).await?;
    
    // Pre-approve WMON to AllowanceHolder (for 0x SELL leg)
    let wmon_approval_amount = U256::from(100_000_000_000_000_000_000u128); // 100 WMON
    execution::ensure_approval(
        &provider,
        &eth_wallet,
        config::WMON,
        config::ALLOWANCE_HOLDER,
        wmon_approval_amount,
    ).await?;
    
    println!("Approvals confirmed. Starting monitoring...\n");
}
```

2. Inside the loop, after printing current prices:

```rust
// Calculate spread
let spread_pct = ((zrx_price - uniswap_price) / uniswap_price) * 100.0;

// Print current spread on every tick
println!(
    "[{}] MON/USDC | 0x: ${:.6} | Uniswap: ${:.6} | Spread: {:+.3}%",
    chrono::Local::now().format("%H:%M:%S"),
    zrx_price,
    uniswap_price,
    spread_pct
);

// Auto-execute if enabled and spread exceeds threshold
if args.spread_threshold > 0.0 && spread_pct > args.spread_threshold {
    println!("\n========== SPREAD THRESHOLD TRIGGERED ==========");
    println!("Detected: {:+.3}% > Threshold: {}%", spread_pct, args.spread_threshold);
    
    // Calculate trade amounts
    // Use fixed WMON amount for sell leg, auto-calculate USDC from 0x quote
    let wmon_amount = U256::from((args.wmon_amount * 1e18) as u128);
    
    // Get 0x price to determine USDC equivalent
    let price_quote = zrx.get_price(
        config::WMON,
        config::USDC,
        &wmon_amount.to_string(),
    ).await;
    
    match price_quote {
        Ok(quote) => {
            let usdc_from_quote: u128 = quote.buy_amount.parse().unwrap_or(0);
            let usdc_amount = U256::from(usdc_from_quote);
            
            println!("Trade Size: {} WMON / ${:.2} USDC", args.wmon_amount, usdc_from_quote as f64 / 1e6);
            println!("Executing parallel arbitrage...\n");
            
            let start = std::time::Instant::now();
            
            match execution::execute_parallel_arbitrage(
                &provider,
                &eth_wallet,
                &zrx,
                usdc_amount,
                wmon_amount,
                args.pool_fee,
                args.slippage_bps,
            ).await {
                Ok(report) => {
                    report.print();
                    
                    // Print post-execution stats
                    println!("========== POST-EXECUTION STATS ==========");
                    println!("Total Time: {}ms", start.elapsed().as_millis());
                    println!("Triggered Spread: {:+.3}%", spread_pct);
                    println!("USDC P/L: {:+.6}", report.usdc_change);
                    println!("==========================================\n");
                }
                Err(e) => {
                    eprintln!("Execution failed: {}", e);
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to get 0x quote: {}", e);
        }
    }
}
```

### Remove Duplicate Detection Output

The current `check_arbitrage()` function prints its own "ARBITRAGE DETECTED" box. When auto-execute is enabled, this is redundant. Modify the call:

```rust
// Only print detection box if NOT auto-executing
if args.spread_threshold == 0.0 {
    if let Some(arb) = check_arbitrage(zrx_price, uniswap_price, "Uniswap") {
        arb.print();
    }
}
```

---

## Priority 3: Speed Improvements - Pre-fetch Nonces

### Problem

Current parallel execution has ~500-700ms submit latency per leg. Part of this is RPC round-trips to fetch nonces.

### Solution

Pre-fetch nonces before spawning parallel tasks, then manually set them on transactions.

### Changes to `src/execution.rs`

#### 1. Add nonce parameter to no-wait functions

Modify `execute_uniswap_buy_no_wait`:

```rust
async fn execute_uniswap_buy_no_wait<P: Provider>(
    provider: &P,
    wallet: &EthereumWallet,
    usdc_amount: U256,
    min_mon_out: U256,
    pool_fee: u32,
    nonce: u64,  // ADD THIS PARAMETER
) -> Result<PendingLegResult> {
    // ... existing setup code ...
    
    let tx = TransactionRequest::default()
        .to(router)
        .input(call.abi_encode().into())
        .from(from)
        .gas_limit(gas_limit)
        .nonce(nonce);  // ADD THIS LINE
    
    // ... rest unchanged ...
}
```

Modify `execute_0x_swap_no_wait`:

```rust
async fn execute_0x_swap_no_wait<P: Provider>(
    provider: &P,
    wallet: &EthereumWallet,
    quote: &crate::zrx::QuoteResponse,
    nonce: u64,  // ADD THIS PARAMETER
) -> Result<PendingLegResult> {
    // ... existing setup code ...
    
    let tx = TransactionRequest::default()
        .to(to)
        .input(data.into())
        .value(value)
        .gas_limit(adjusted_gas_limit)
        .gas_price(adjusted_gas_price)
        .from(from)
        .nonce(nonce);  // ADD THIS LINE
    
    // ... rest unchanged ...
}
```

#### 2. Update `execute_parallel_arbitrage` to pre-fetch nonces

Add nonce fetching before spawning tasks:

```rust
pub async fn execute_parallel_arbitrage<P: Provider + Clone + 'static>(
    provider: &P,
    wallet: &EthereumWallet,
    zrx: &crate::zrx::ZrxClient,
    usdc_amount: U256,
    wmon_amount: U256,
    pool_fee: u32,
    slippage_bps: u32,
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

    // ... existing println statements ...

    // PRE-FLIGHT: Get 0x quote BEFORE firing transactions
    let sell_quote = zrx.get_quote(
        crate::config::WMON,
        crate::config::USDC,
        &wmon_amount.to_string(),
        &format!("{:?}", wallet_addr),
        slippage_bps,
    ).await?;

    let expected_usdc_from_0x: f64 = sell_quote.buy_amount.parse::<f64>().unwrap_or(0.0) / 1e6;
    println!("\nExpected USDC from 0x: {:.6}", expected_usdc_from_0x);

    // ========== FIRE BOTH LEGS SIMULTANEOUSLY ==========
    let provider_a = provider.clone();
    let wallet_a = wallet.clone();
    let usdc_amt = usdc_amount;
    let fee = pool_fee;
    let nonce_a = leg_a_nonce;  // Capture for move

    let provider_b = provider.clone();
    let wallet_b = wallet.clone();
    let quote = sell_quote;
    let nonce_b = leg_b_nonce;  // Capture for move

    // Spawn Leg A: Uniswap BUY
    let leg_a_handle = tokio::spawn(async move {
        execute_uniswap_buy_no_wait(
            &provider_a,
            &wallet_a,
            usdc_amt,
            U256::ZERO,
            fee,
            nonce_a,  // PASS NONCE
        ).await
    });

    // Spawn Leg B: 0x SELL
    let leg_b_handle = tokio::spawn(async move {
        execute_0x_swap_no_wait(&provider_b, &wallet_b, &quote, nonce_b).await  // PASS NONCE
    });

    // ... rest of function unchanged ...
}
```

---

## Files to Modify

| File | Changes |
|------|---------|
| `src/main.rs` | Add `spread_threshold` arg, add auto-execute logic in monitoring loop, add pre-loop approvals |
| `src/execution.rs` | Add `nonce` parameter to both no-wait functions, pre-fetch nonces in `execute_parallel_arbitrage` |

---

## Test Commands

### Test auto-execute mode (will trigger on spreads > 1%):
```bash
cargo run -- --spread-threshold 1.0 --wmon-amount 5.0 --slippage-bps 100
```

### Test with lower threshold for faster triggering:
```bash
cargo run -- --spread-threshold 0.5 --wmon-amount 3.0 --slippage-bps 100
```

### Test parallel execution directly (verify nonce optimization):
```bash
cargo run -- --test-parallel --wmon-amount 3.0 --slippage-bps 100
```

---

## Expected Output

### Monitoring mode with auto-execute:
```
==========================================
  Monad Arbitrage Bot
  Wallet: 0xad0e53732ae6ac04edc36f9f81c4bfc9aa344b2b
==========================================

AUTO-EXECUTE ENABLED
Spread Threshold: 1%
Approvals confirmed. Starting monitoring...

[14:32:01] MON/USDC | 0x: $0.032100 | Uniswap: $0.031800 | Spread: +0.943%
[14:32:03] MON/USDC | 0x: $0.032150 | Uniswap: $0.031750 | Spread: +1.260%

========== SPREAD THRESHOLD TRIGGERED ==========
Detected: +1.260% > Threshold: 1%
Trade Size: 5.0 WMON / $0.16 USDC
Executing parallel arbitrage...

[execution report prints here]

========== POST-EXECUTION STATS ==========
Total Time: 850ms
Triggered Spread: +1.260%
USDC P/L: +0.001234
==========================================

[14:32:05] MON/USDC | 0x: $0.032050 | Uniswap: $0.031850 | Spread: +0.628%
```

---

## Success Criteria

1. `cargo run -- --spread-threshold 1.0 --wmon-amount 3.0` compiles and runs
2. When spread exceeds threshold, parallel arbitrage executes automatically
3. Submit latency for both legs should be <200ms (down from 500-700ms)
4. Nonces are pre-fetched (visible in logs: "Pre-fetched nonces: Leg A = X, Leg B = Y")
5. Post-execution stats print after each triggered trade
6. Monitoring continues after execution (loop doesn't exit)

---

## Do NOT

- Do not add cooldown/rate limiting (we will handle separately)
- Do not add profitability checks (we are testing execution speed)
- Do not modify existing `--test-parallel` behavior
- Do not change the monitoring interval (stays at 2 seconds)
- Do not add Telegram/Discord alerts
