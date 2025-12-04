# FIX: Stale Price Protection

## For: Claude Code Opus
## Priority: HIGH

---

## Problem

Bot detects +6.9% spread, executes trade, but spread collapses to +0.009% by confirmation time (7 seconds later). Result: **-$0.0016 loss** on a trade that looked profitable.

**Evidence:**
```
03:29:17 - Detected: +6.941% spread
03:29:19 - Spread now: +0.009% (collapsed in 2 seconds)
03:29:25 - Trade confirms: USDC P/L: -0.001599
```

---

## Required Changes

### 1. Add Price Staleness Check Before Execution

**File:** `src/main.rs`

**Location:** After getting the 0x quote, before calling `execute_parallel_arbitrage`

**Add this check:**

```rust
// Get fresh Uniswap price right before execution
let fresh_uniswap_price = get_uniswap_price(&provider, &pool_address).await?;
let fresh_spread = (zrx_price - fresh_uniswap_price) / fresh_uniswap_price * 100.0;

// Abort if spread has degraded significantly
const MIN_EXECUTION_SPREAD: f64 = 0.5; // Minimum 0.5% spread at execution time
if fresh_spread < MIN_EXECUTION_SPREAD {
    println!("  [ABORTED] Spread collapsed: {:.3}% -> {:.3}%", original_spread, fresh_spread);
    continue;
}
```

### 2. Add Config Constant

**File:** `src/config.rs`

```rust
/// Minimum spread required at execution time (after quote fetch)
pub const MIN_EXECUTION_SPREAD_PCT: f64 = 0.5;
```

### 3. Increase Default Spread Threshold

**File:** `src/config.rs`

Change default spread threshold from `0.9` to `2.0`:

```rust
/// Default spread threshold for triggering trades
pub const DEFAULT_SPREAD_THRESHOLD: f64 = 2.0;
```

**Reasoning:** With 7-second confirmation times, small spreads will be arbed away before your transaction lands.

---

## Updated Execution Flow

```
1. Detect spread (e.g., +6.9%)
2. Check: spread > threshold (2.0%)? ✓
3. Fetch 0x quote
4. Check: 0x gas < 400k? ✓
5. ** NEW: Re-check spread immediately before execution **
6. Check: fresh_spread > 0.5%? If no → ABORT
7. Execute parallel arbitrage
```

---

## Optional: Add Spread Decay Logging

**File:** `src/main.rs`

Add logging to track how fast spreads decay:

```rust
println!("  [SPREAD CHECK] Original: {:.3}% | Fresh: {:.3}% | Decay: {:.3}%", 
    original_spread, fresh_spread, original_spread - fresh_spread);
```

This helps tune the `MIN_EXECUTION_SPREAD` value over time.

---

## Test Command

```bash
cargo run --release -- --spread-threshold 2.0 --wmon-amount 50.0 --slippage-bps 100
```

**Expected behavior:** Bot should print `[ABORTED] Spread collapsed` when spreads disappear between detection and execution.

---

## Summary

| Change | File | Purpose |
|--------|------|---------|
| Add fresh spread check | `src/main.rs` | Abort if spread collapsed |
| Add `MIN_EXECUTION_SPREAD_PCT` | `src/config.rs` | Configurable abort threshold |
| Increase default threshold | `src/config.rs` | Require larger spreads (2%+) |
