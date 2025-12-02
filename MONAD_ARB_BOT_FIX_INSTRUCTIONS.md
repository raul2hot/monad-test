# Monad Arbitrage Bot - Root Cause Analysis & Fix Instructions

**Date:** December 2, 2025  
**Issue:** All detected arbitrage opportunities are being rejected despite graph showing "profitable" cycles

---

## Executive Summary

The bot is detecting arbitrage cycles (e.g., "WMON → USDC → WMON | 21% profit") but ALL opportunities are rejected during simulation. The root cause is **inconsistent liquidity measurement** between pool discovery and simulation phases, combined with **unrealistic profit calculations** in the graph building phase.

---

## Issue 1: Inconsistent Liquidity Measurement (CRITICAL)

### Symptom
```
Pool 0x5E60BC3F7a7303BC4dfE4dc2220bdC90bc04fE22 has insufficient liquidity: 
44323215691692120251 < 100000000000000000000
```

The pool has ~44 tokens of liquidity but the simulation requires 100 tokens minimum.

### Root Cause

**Two different liquidity measurement methods are used:**

| Phase | Location | Method | What It Measures |
|-------|----------|--------|------------------|
| Pool Discovery | `src/dex/batch_client.rs` | `getReserves()` | **Total** reserves across ALL bins |
| Simulation | `src/simulation/liquidity.rs` | `getBin()` in [-10, +10] range | **Active** liquidity near current price |

For LFJ (Liquidity Book), reserves can be spread across hundreds of bins. A pool might have:
- 2000 tokens total reserves (passes 1000-token discovery threshold)
- Only 44 tokens in active bins near current price (fails 100-token simulation threshold)

### Files to Modify

1. **`src/dex/batch_client.rs`** - Line ~180 in `lfj_to_pool()` and `get_pools()`
2. **`src/simulation/liquidity.rs`** - Line ~100 in `get_lfj_liquidity()`
3. **`src/config.rs`** - Centralize MIN_LIQUIDITY constant

### Fix Strategy

**Option A (Recommended): Use Active Liquidity Everywhere**

Modify pool discovery to use the same active-bins measurement as simulation:

```rust
// In batch_client.rs - Instead of using total reserves from getReserves(),
// fetch active bin ID and query surrounding bins to get ACTIVE liquidity
// This matches what simulation/liquidity.rs does
```

**Option B: Lower Simulation Threshold**

If discovery measures total reserves, simulation could accept lower active liquidity. But this risks executing trades against illiquid pools.

**Option C: Add Active Liquidity Field to Pool Struct**

Extend the `Pool` struct to store both total and active liquidity, then filter appropriately at each stage.

### Specific Code Changes Required

#### File: `src/dex/batch_client.rs`

Location: `BatchLfjClient::lfj_to_pool()` function (~line 140)

Current problematic code:
```rust
fn lfj_to_pool(pair_addr: Address, data: &LfjPairData) -> Pool {
    // ...
    // Calculate liquidity from reserves
    let total_liquidity = data.reserve_x + data.reserve_y;  // BUG: Uses total reserves
    // ...
}
```

Fix: Query active bins to get tradeable liquidity, matching the approach in `simulation/liquidity.rs`.

#### File: `src/simulation/liquidity.rs`

Location: `get_lfj_liquidity()` function (~line 80)

This function queries bins in range [-10, +10] which is correct for active liquidity. However, verify the bin range is appropriate (some pools may need wider range).

#### File: `src/config.rs`

Add centralized liquidity constants:
```rust
pub mod thresholds {
    /// Minimum active liquidity for pool inclusion (100 tokens with 18 decimals)
    pub const MIN_ACTIVE_LIQUIDITY: u128 = 100 * 10u128.pow(18);
    
    /// Minimum total liquidity (for reference, not primary filter)
    pub const MIN_TOTAL_LIQUIDITY: u128 = 1000 * 10u128.pow(18);
}
```

Remove duplicate `MIN_LIQUIDITY` constants from:
- `src/dex/batch_client.rs` (line ~22)
- `src/dex/lfj.rs` (line ~95)
- `src/dex/pancakeswap.rs` (line ~60)
- `src/dex/uniswap_v3.rs` (line ~60)

---

## Issue 2: Unrealistic Profit Detection (HIGH)

### Symptom
```
Unique cycle: WMON -> USDC -> WMON | 2 hops | 21.00% profit | single-dex
```

21% profit on a simple 2-hop same-DEX cycle is unrealistic. Real arbitrage opportunities are typically <1%.

### Root Cause

The LFJ price calculation in `src/dex/lfj.rs` may have precision issues with the Q128.128 fixed-point conversion.

### Files to Examine

1. **`src/dex/lfj.rs`** - `q128_to_f64()` function and `get_pool_state()`
2. **`src/dex/batch_client.rs`** - `calculate_lfj_sqrt_price()` function
3. **`src/dex/mod.rs`** - `Pool::price_0_to_1()` method

### Investigation Steps

1. **Add diagnostic logging** to compare:
   - Graph-calculated price (from `Pool::price_0_to_1()`)
   - Quoter-returned price (from `QuoteFetcher`)
   
2. **Run the diagnostic module** already present in `src/simulation/diagnostics.rs`:
   ```rust
   // Add to main.rs after finding cycles:
   for cycle in cycles.iter().take(1) {
       diagnose_cycle(provider.clone(), cycle, &all_pools).await?;
   }
   ```

3. **Check decimal handling** - LFJ may already include decimal adjustment in its price, causing double-adjustment in `Pool::price_0_to_1()`.

### Specific Areas to Review

#### File: `src/dex/lfj.rs`

Location: `get_pool_state()` function (~line 70)

```rust
// FIXED: Proper conversion from Q128.128 using dedicated function
let actual_price = q128_to_f64(price_x128);

// Note: decimal_adjustment is calculated but LFJ already handles decimals in its price
// The Pool::price_0_to_1() method will apply decimal adjustment using decimals0/decimals1
let _decimal_adjustment = 10_f64.powi(decimals0 as i32 - decimals1 as i32);
```

**Potential bug:** The comment says "LFJ already handles decimals" but then `Pool::price_0_to_1()` applies decimal adjustment again. Verify whether this causes double-adjustment.

#### File: `src/dex/batch_client.rs`

Location: `calculate_lfj_sqrt_price()` function (~line 130)

```rust
fn calculate_lfj_sqrt_price(active_id: u32, bin_step: u16) -> U256 {
    const ID_OFFSET: i64 = 8388608; // 2^23
    let exponent = (active_id as i64) - ID_OFFSET;
    let base = 1.0 + (bin_step as f64) / 10000.0;
    let price = base.powf(exponent as f64);
    let sqrt_price = price.sqrt();
    let sqrt_price_x96 = sqrt_price * 2_f64.powi(96);
    // ...
}
```

**Potential issue:** This approximation may not match what the quoter returns for actual swaps. Consider using the actual price from `getPriceFromId()` instead.

---

## Issue 3: Multiple Inconsistent Constants (MEDIUM)

### Symptom

`MIN_LIQUIDITY` is defined in 4+ different files with different values.

### Files with Duplicates

| File | Value | Used For |
|------|-------|----------|
| `src/dex/batch_client.rs` | `1000 * 10^18` | Pool discovery |
| `src/dex/lfj.rs` | `1000 * 10^18` | LFJ pool filtering |
| `src/dex/pancakeswap.rs` | `1000 * 10^18` | PancakeSwap filtering |
| `src/dex/uniswap_v3.rs` | `1000 * 10^18` | Uniswap V3 filtering |
| `src/simulation/simulator.rs` | `100 * 10^18` | **Different!** Simulation filtering |
| `src/config.rs` | `1000 * 10^18` | Config (unused?) |

### Fix

Centralize all thresholds in `src/config.rs` and import from there:

```rust
// src/config.rs
pub mod thresholds {
    pub const MIN_LIQUIDITY_NATIVE: u128 = 100 * 10u128.pow(18);  // 100 tokens
    pub const MIN_PROFIT_BPS: u32 = 10;  // 0.1%
}
```

---

## Issue 4: Graph Builds From Stale/Invalid Pools (LOW)

### Symptom

Pools with unrealistic prices (21% spread on same-DEX) are being added to the graph.

### Root Cause

The validation in `ArbitrageGraph::add_pool()` only checks `is_price_valid()` which allows prices between 1e-18 and 1e18 - a very wide range.

### File to Modify

**`src/graph/builder.rs`** - `add_pool()` function

### Fix

Add cross-validation:
```rust
pub fn add_pool(&mut self, pool: &Pool) {
    // Existing check
    if !pool.is_price_valid() {
        return;
    }
    
    // NEW: Skip pools where price_0_to_1 * price_1_to_0 deviates significantly from 1.0
    let round_trip = pool.price_0_to_1() * pool.price_1_to_0();
    if (round_trip - 1.0).abs() > 0.01 {  // 1% tolerance
        tracing::warn!(
            "Skipping pool {} - round trip price deviation: {:.4}",
            pool.address,
            round_trip
        );
        return;
    }
    
    // ... rest of function
}
```

---

## Recommended Fix Order

1. **Issue 1 (Critical)**: Fix liquidity measurement consistency
   - This is why ALL opportunities are rejected
   - Time estimate: 2-3 hours

2. **Issue 2 (High)**: Fix LFJ price calculation  
   - This causes false positives in cycle detection
   - Time estimate: 1-2 hours for investigation, 1-2 hours for fix

3. **Issue 3 (Medium)**: Centralize constants
   - Quick win for code maintainability
   - Time estimate: 30 minutes

4. **Issue 4 (Low)**: Add graph validation
   - Defense in depth
   - Time estimate: 30 minutes

---

## Verification Steps

After implementing fixes, verify with:

1. **Check pool counts match**: Discovery should find fewer pools (active liquidity filter)

2. **Check profit percentages are realistic**: Most should be <1%, maximum ~5%

3. **Check simulation accepts some opportunities**: At least some verified opportunities should pass

4. **Run diagnostics**:
   ```rust
   // Enable in main.rs
   use crate::simulation::diagnostics::diagnose_cycle;
   
   // After finding cycles
   for cycle in cycles.iter().take(3) {
       if let Err(e) = diagnose_cycle(provider.clone(), cycle, &all_pools).await {
           warn!("Diagnostic failed: {}", e);
       }
   }
   ```

---

## Quick Diagnostic Command

To quickly verify the fix is working, look for these log patterns:

**Before fix:**
```
Total candidates: X
Verified opportunities: 0
Rejected opportunities: X
```

**After fix:**
```
Total candidates: Y (Y < X because fewer pools pass active liquidity filter)
Verified opportunities: Z (Z > 0)
Rejected opportunities: Y - Z
```

---

## Additional Notes

### LFJ Liquidity Book Specifics

LFJ uses a "Liquidity Book" model where liquidity is distributed across discrete price bins. Key concepts:

- `binStep`: Price increment between bins (e.g., 10 = 0.1% per bin)
- `activeId`: The bin containing current price
- Reserves can exist far from active bin but are not tradeable without crossing many bins

When querying liquidity for trading purposes, only bins near the active price matter.

### Flash Loan Integration

The bot uses Neverland (Aave V3 fork) for flash loans with 9 bps fee. This is already correctly accounted for in `profit_calculator.rs`.

### Gas Estimation

The `MAX_REASONABLE_GAS` constant (1M gas) is appropriate. High gas estimates indicate quoter reverts.

---

*Generated by Claude for fixing Monad Arbitrage Bot issues*
