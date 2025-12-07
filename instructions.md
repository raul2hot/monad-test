# Monad Arb Bot - Troubleshooting: Negative Profit on "Successful" Atomic Arb

## Summary of the Issue

**Log Analysis:**
```
Pre-execution:  WMON balance = 1000.627
Post-execution: WMON balance = 1000.280
Delta:          -0.347 WMON (LOSS of ~14 bps)
Expected:       +0.754 WMON (PROFIT of ~30 bps)
Total Deviation: ~44 bps worse than expected
```

The arb "succeeded" (didn't revert) but lost money because `--force` flag was used.

---

## Root Cause Analysis

### 1. The `--force` Flag Bypasses Profit Checks

In `atomic_arb.rs` (lines ~150-170):
```rust
let calldata = if force {
    println!("  Using UNCHECKED mode (force=true) - no profit check");
    let execute_call = executeArbUncheckedCall {  // <-- NO minProfit check!
        ...
        minWmonOut: min_wmon_out_wei,  // Only checks slippage, not profit
    };
```

**Problem:** `executeArbUnchecked` only verifies `minWmonOut` (slippage), NOT profitability. The arb can succeed with 200 bps slippage (getting back 245.7+ WMON) while still being unprofitable.

### 2. Price Moved During Execution (~940ms)

The log shows:
- `total_execution_ms: 940`
- Price fetch was done BEFORE execution
- By the time TX confirmed, prices likely shifted

With volatile markets, 940ms is enough for the spread to evaporate.

### 3. Trade Size vs Liquidity (250 WMON)

250 WMON is a significant trade that causes:
- **Price impact on sell**: Pushed PancakeSwap1 price down
- **Price impact on buy**: Pushed MondayTrade price up
- Combined impact likely exceeded the 30 bps gross spread

### 4. Post-Execution Logging Bug

In `run_auto_arb` (main.rs ~1640), the `PostExecutionSnapshot` for atomic arb shows:
```rust
actual_usdc_received: result.usdc_intermediate,  // This is 0 for atomic arb!
actual_wmon_back: result.wmon_out,              // This uses ESTIMATED value!
```

The atomic result's `wmon_out` is calculated as:
```rust
wmon_out: result.wmon_in + result.profit_wmon,  // Uses estimated profit!
```

---

## Fixes Required

### Fix 1: Remove `--force` Flag for Testing (Immediate)

**Never use `--force` with real funds.** It's only for debugging contract execution paths.

Run without `--force`:
```bash
cargo run -- auto-arb --min-spread-bps 50 --amount 10 --slippage 150
```

### Fix 2: Add Real Profit Verification in Atomic Arb

In `src/execution/atomic_arb.rs`, after receipt confirmation, query actual balance change:

```rust
// After line ~280 (after receipt check)
if receipt.status() {
    // Query ACTUAL balances instead of using estimates
    let (wmon_after, _) = query_contract_balances(provider_with_signer).await?;
    let actual_wmon_back = wmon_after - wmon_before + amount; // Account for initial spend
    let actual_profit = actual_wmon_back - amount;
    let actual_profit_bps = if amount > 0.0 {
        (actual_profit / amount * 10000.0) as i32
    } else {
        0
    };
    
    // Use actual values, not estimates
    return Ok(AtomicArbResult {
        ...
        profit_wmon: actual_profit,  // ACTUAL, not estimated
        profit_bps: actual_profit_bps,
        ...
    });
}
```

### Fix 3: Add Pre-TX Balance Snapshot

In `execute_atomic_arb`, add balance query BEFORE execution:

```rust
// Add at start of function (after validations)
let (wmon_before, usdc_before) = query_contract_balances(provider_with_signer).await?;
println!("  Contract balances before: {:.6} WMON, {:.6} USDC", wmon_before, usdc_before);
```

### Fix 4: Reduce Trade Size for Testing

In your test command:
```bash
# Use small amounts until confident
cargo run -- auto-arb --amount 1.0 --min-spread-bps 50 --slippage 150
```

### Fix 5: Tighten Slippage Tolerance

200 bps slippage is too loose. A "profitable" 30 bps spread can easily become a loss.

```bash
# Use 50-100 bps slippage for real trading
cargo run -- auto-arb --slippage 75 --min-spread-bps 30
```

### Fix 6: Add Real-Time Price Recheck Before Execution

In `run_auto_arb`, before executing, re-fetch prices:

```rust
// Before execute_atomic_arb call
println!("  Re-checking prices before execution...");
let fresh_prices = get_current_prices(&provider).await?;
let fresh_sell = fresh_prices.iter().find(|p| p.pool_name == spread.sell_pool);
let fresh_buy = fresh_prices.iter().find(|p| p.pool_name == spread.buy_pool);

if let (Some(sell), Some(buy)) = (fresh_sell, fresh_buy) {
    let fresh_spread_bps = ((sell.price - buy.price) / buy.price * 10000.0) as i32;
    if fresh_spread_bps < min_spread_bps {
        println!("  Spread evaporated! Was {} bps, now {} bps. Skipping.", 
            net_spread_bps, fresh_spread_bps);
        continue;
    }
}
```

---

## Recommended Testing Workflow

### Step 1: Verify Contract Logic (Dry Run)
```bash
cargo run -- auto-arb --dry-run --min-spread-bps 20 --amount 10
```

### Step 2: Small Amount Test (NO --force)
```bash
cargo run -- auto-arb --amount 0.1 --min-spread-bps 50 --slippage 100 --max-executions 1
```

### Step 3: Monitor Results
Check `arb_stats_*.jsonl` for:
- `post.wmon_delta` - actual balance change
- `post.net_profit_bps` - should match expectation
- Compare `pre.net_spread_bps` vs `post.net_profit_bps`

### Step 4: Scale Up Gradually
Only increase `--amount` after confirming profitability at lower sizes.

---

## Key Metrics to Monitor

| Metric | Expected | Your Log | Issue |
|--------|----------|----------|-------|
| `swap2_tx_hash` | empty (atomic) | empty | ✓ OK |
| `actual_usdc_received` | >0 | 0.0 | Bug: not tracked for atomic |
| `wmon_delta` | >0 | -0.347 | ✗ LOSS |
| `net_profit_bps` | ~20 | -13 | ✗ 33 bps worse |
| `total_execution_ms` | <500 | 940 | ⚠ Slow, allows price drift |

---

## Summary Checklist

- [ ] Remove `--force` flag immediately
- [ ] Reduce `--amount` to 1-10 WMON for testing
- [ ] Tighten `--slippage` to 75-100 bps
- [ ] Increase `--min-spread-bps` to 50+ for safety margin
- [ ] Apply Fix 2 (actual balance verification)
- [ ] Apply Fix 6 (price recheck before execution)
- [ ] Monitor `wmon_delta` in logs, not `success` flag

---

## Why The Arb "Succeeded" But Lost Money

```
minWmonOut = 250.754 * (1 - 0.02) = 245.74 WMON
Actual received ≈ 249.65 WMON (estimated from delta)

245.74 < 249.65  →  Slippage check PASSED
249.65 < 250.00  →  But LOST 0.35 WMON profit!
```

The slippage protection (`minWmonOut`) prevented catastrophic loss but allowed small loss. With `--force`, there's no `minProfit` check, so any loss within slippage tolerance executes.

**Solution:** Never use `--force` for production, and implement Fix 2 to catch this in logging.