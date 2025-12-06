# Fast Arb Improvement Instructions

## Current Performance: 1.7s
## Target Performance: <1.2s (before own node)

---

## Issue 1: Gas Display Shows Limit, Not Actual Usage

**Problem:** Output shows `Gas Used: 800000` which is the hardcoded limit, not actual consumption.

**File:** `src/execution/fast_arb.rs`

**Find this code in `execute_fast_arb()`:**
```rust
println!("    Swap 1 confirmed: {} (gas: {})", 
         if receipt1.status() { "SUCCESS" } else { "FAILED" },
         swap1.gas_limit);  // WRONG: shows limit
```

**Replace with:**
```rust
println!("    Swap 1 confirmed: {} (gas: {})", 
         if receipt1.status() { "SUCCESS" } else { "FAILED" },
         receipt1.gas_used);  // CORRECT: shows actual
```

**Do the same for swap 2.**

---

## Issue 2: Dynamic Safety Buffer Based on Slippage

**Problem:** Fixed 99% buffer is too conservative for tight slippage, too loose for wide slippage.

**File:** `src/execution/fast_arb.rs`

**Find:**
```rust
let usdc_for_buy = expected_usdc * 0.99;
```

**Replace with:**
```rust
// Dynamic buffer: slippage 200bps → 99%, slippage 100bps → 99.5%, slippage 50bps → 99.75%
let safety_factor = 1.0 - (slippage_bps as f64 / 20000.0);
let usdc_for_buy = expected_usdc * safety_factor;
```

---

## Issue 3: Reduce Receipt Polling Interval

**Problem:** 50ms polling may be too slow for Monad's fast blocks.

**File:** `src/execution/fast_arb.rs`

**Find:**
```rust
let mut poll_interval = interval(Duration::from_millis(50));
```

**Replace with:**
```rust
let mut poll_interval = interval(Duration::from_millis(20));  // 20ms polling
```

**Expected savings:** ~50-100ms

---

## Issue 4: Add Timeout to Parallel Receipt Wait

**Problem:** If one receipt hangs, both hang forever.

**File:** `src/execution/fast_arb.rs`

**Find:**
```rust
let (receipt1, receipt2) = tokio::join!(
    wait_for_receipt_fast(provider_with_signer, tx1_hash),
    wait_for_receipt_fast(provider_with_signer, tx2_hash)
);
```

**Replace with:**
```rust
let receipt_timeout = Duration::from_secs(15);
let (receipt1, receipt2) = tokio::join!(
    timeout(receipt_timeout, wait_for_receipt_fast(provider_with_signer, tx1_hash)),
    timeout(receipt_timeout, wait_for_receipt_fast(provider_with_signer, tx2_hash))
);

let receipt1 = receipt1.map_err(|_| eyre!("Swap 1 receipt timeout"))??;
let receipt2 = receipt2.map_err(|_| eyre!("Swap 2 receipt timeout"))??;
```

---

## Issue 5: Pre-validate Profitability Before Execution

**Problem:** Arb executes even when spread is negative (wastes gas).

**File:** `src/main.rs` in `run_fast_arb()`

**Add after getting prices:**
```rust
let spread_bps = ((sell_price - buy_price) / buy_price * 10000.0) as i32;
let total_fee_bps = (sell_router.pool_fee / 100 + buy_router.pool_fee / 100) as i32;
let net_spread_bps = spread_bps - total_fee_bps;

println!("  Spread: {} bps | Fees: {} bps | Net: {} bps", 
         spread_bps, total_fee_bps, net_spread_bps);

if net_spread_bps < 0 {
    println!("\n  ⚠️  Negative net spread. Arb will be unprofitable.");
    println!("  Continue anyway? [y/N]");
    
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    if !input.trim().eq_ignore_ascii_case("y") {
        println!("  Aborted.");
        return Ok(());
    }
}
```

---

## Issue 6: Track Actual vs Estimated Amounts

**Problem:** Can't verify if estimates are accurate without checking final balances.

**File:** `src/execution/fast_arb.rs`

**Add to `FastArbResult`:**
```rust
pub struct FastArbResult {
    // ... existing fields ...
    pub wmon_out_actual: Option<f64>,  // NEW: filled after balance check
    pub estimation_error_bps: Option<i32>,  // NEW: (actual - estimated) / estimated
}
```

**Add optional balance check at end of `execute_fast_arb()`:**
```rust
// Optional: verify actual output (adds ~200ms but useful for debugging)
#[cfg(debug_assertions)]
{
    use crate::wallet::get_balances;
    let final_balances = get_balances(provider_with_signer, wallet_address).await?;
    // Compare with starting balance to get actual WMON received
}
```

---

## Issue 7: Connection Reuse for HTTP Provider

**Problem:** Each RPC call may open new TCP connection.

**File:** `src/main.rs`

**Add at top of file:**
```rust
use reqwest::Client;
use std::sync::OnceLock;

static HTTP_CLIENT: OnceLock<Client> = OnceLock::new();

fn get_http_client() -> &'static Client {
    HTTP_CLIENT.get_or_init(|| {
        Client::builder()
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(Duration::from_secs(30))
            .tcp_keepalive(Duration::from_secs(60))
            .build()
            .expect("Failed to create HTTP client")
    })
}
```

**Note:** This requires modifying how alloy provider is created. Check if alloy supports custom reqwest client.

---

## Issue 8: Batch Nonce Reservation

**Problem:** `next_nonce()` called twice sequentially.

**File:** `src/nonce.rs`

**Add new function:**
```rust
/// Reserve multiple nonces atomically
pub fn reserve_nonces(count: u64) -> Vec<u64> {
    let start = NONCE
        .get()
        .expect("Nonce manager not initialized")
        .fetch_add(count, Ordering::SeqCst);
    
    (0..count).map(|i| start + i).collect()
}
```

**Usage in fast_arb.rs:**
```rust
let nonces = crate::nonce::reserve_nonces(2);
let nonce1 = nonces[0];
let nonce2 = nonces[1];
```

---

## Issue 9: Log Arb Opportunities to File

**Problem:** No persistent record of executed arbs for analysis.

**File:** `src/execution/fast_arb.rs`

**Add at end of `execute_fast_arb()`:**
```rust
// Log to arb execution history
use std::fs::OpenOptions;
use std::io::Write;

let log_line = format!(
    "{},{},{},{},{},{},{},{},{}\n",
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
    sell_dex,
    buy_dex,
    result.wmon_in,
    result.usdc_intermediate,
    result.wmon_out,
    result.total_gas_used,
    result.execution_time_ms,
    if result.error.is_some() { "FAILED" } else { "SUCCESS" }
);

if let Ok(mut file) = OpenOptions::new()
    .create(true)
    .append(true)
    .open("arb_executions.csv") 
{
    let _ = file.write_all(log_line.as_bytes());
}
```

---

## Priority Order

1. **Issue 1** - Fix gas display (quick win, better debugging)
2. **Issue 3** - 20ms polling (easy, saves ~50-100ms)
3. **Issue 5** - Profitability check (prevents wasted gas)
4. **Issue 2** - Dynamic buffer (better capital efficiency)
5. **Issue 4** - Receipt timeout (prevents hangs)
6. **Issue 8** - Batch nonces (cleaner code)
7. **Issue 9** - Execution logging (analytics)
8. **Issue 6** - Actual amount tracking (debugging)
9. **Issue 7** - Connection reuse (may require alloy changes)

---

## Testing Commands

```bash
# Test with small amount first
cargo run -- fast-arb --sell-dex pancakeswap2 --buy-dex mondaytrade --amount 0.01 --slippage 200

# Compare old vs new timing
cargo run -- test-arb --sell-dex pancakeswap2 --buy-dex mondaytrade --amount 0.01 --slippage 200
```

---

## Expected Results After All Fixes

| Metric | Before | After |
|--------|--------|-------|
| Total time | 1.7s | ~1.4s |
| Gas display | Limit (800k) | Actual (~250k) |
| Failed arbs | Possible | Warned |
| Execution log | None | CSV file |