# Fix Instructions for Monad Arbitrage Bot

## 3 Critical Bugs to Fix

Use `test-arb` for the arbitrage bot (not `test-swap`).

---

## BUG #1: `to_wei` Function Precision Overflow

### Files to Modify
- `src/execution/swap.rs` (lines ~65-69)
- `src/wallet/wrap.rs` (lines ~35-39)  
- `src/main.rs` (lines ~455-459)

### Problem
```rust
fn to_wei(amount: f64, decimals: u8) -> U256 {
    let multiplier = 10u64.pow(decimals as u32);
    let wei_amount = (amount * multiplier as f64) as u64;  // OVERFLOW: u64 max is ~18.4 for 18 decimals
    U256::from(wei_amount)
}
```

Any WMON amount > 18.4 silently overflows, causing wrong swap amounts.

### Fix
Replace ALL THREE `to_wei` functions with:

```rust
fn to_wei(amount: f64, decimals: u8) -> U256 {
    let multiplier = U256::from(10u64).pow(U256::from(decimals));
    let amount_scaled = (amount * 1e18) as u128;
    U256::from(amount_scaled) * multiplier / U256::from(10u64).pow(U256::from(18u8))
}
```

---

## BUG #3: Monad State Commitment Race Condition

### File to Modify
- `src/main.rs` (lines ~620-660 in `run_test_arb`)

### Problem
Monad has delayed state commitment. Current code waits for 1 block then queries balance, but state may not be committed yet. The 500ms fallback is also insufficient.

### Fix
Replace the block wait + balance query section in `run_test_arb` (after swap 1 completes):

```rust
// MONAD STATE COMMITMENT - Wait for block then retry balance query
let ws_url = std::env::var("MONAD_WS_URL")
    .unwrap_or_else(|_| rpc_url.replace("https://", "wss://").replace("http://", "ws://"));
println!("  ⏳ Waiting for Monad state commitment...");
let t_block = std::time::Instant::now();

match wait_for_next_block(&ws_url).await {
    Ok(block_num) => {
        println!("  ✓ Block {} confirmed in {:?}", block_num, t_block.elapsed());
    }
    Err(e) => {
        println!("  ⚠ WebSocket failed ({}), using 1s delay", e);
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
    }
}

// Retry balance query up to 3 times with 200ms gaps
let mut usdc_for_swap2 = 0.0;
for attempt in 1..=3 {
    match query_usdc_balance(&provider, signer_address).await {
        Ok(actual_balance) => {
            let usdc_received = actual_balance - usdc_before;
            if usdc_received > 0.0001 {
                let usdc_received = (usdc_received * 1_000_000.0).floor() / 1_000_000.0;
                let usdc_safe = usdc_received * 0.998;
                println!("  ✓ USDC received: {:.6} (using {:.6})", usdc_received, usdc_safe);
                usdc_for_swap2 = usdc_safe;
                break;
            }
        }
        Err(_) => {}
    }
    if attempt < 3 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

if usdc_for_swap2 < 0.000001 {
    usdc_for_swap2 = usdc_estimated * 0.99;
    println!("  ⚠ Using estimated USDC: {:.6}", usdc_for_swap2);
}
```

---

## BUG #4: Skip Balance Check Returns Estimated (Not Actual) Amount

### File to Modify
- `src/main.rs` (`run_test_arb` function, around line ~590)

### Problem
When `skip_balance_check = true`, `execute_swap` returns **estimated** output, not actual. For swap 1 in test-arb, we NEED actual USDC received to feed into swap 2.

Current code:
```rust
let sell_result = execute_swap(
    ...
    true,  // Skip balance check - RETURNS ESTIMATE, NOT ACTUAL
).await?;
```

Then `sell_result.amount_out_human` is used to calculate swap 2 input, but it's wrong.

### Fix
In `run_test_arb`, change swap 1 to NOT skip balance check:

```rust
let t_swap1 = std::time::Instant::now();
let sell_result = execute_swap(
    &provider,
    &provider_with_signer,
    signer_address,
    sell_params,
    gas_price,
    false,  // CHANGED: Must get actual USDC received for swap 2
).await?;
println!("  [TIMING] Swap 1 execution: {:?}", t_swap1.elapsed());
```

Keep swap 2 with `skip_balance_check = true` (we only care about final P/L from on-chain state).

---

## Fix Priority

1. **BUG #1** - Silent overflow corrupts amounts
2. **BUG #4** - Swap 2 uses wrong USDC input  
3. **BUG #3** - Stale balance reads

---

## Test After Fixes

```bash
cargo run -- test-arb --sell-dex uniswap --buy-dex pancakeswap1 --amount 1.0 --slippage 150
```
