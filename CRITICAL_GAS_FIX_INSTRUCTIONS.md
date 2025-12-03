# CRITICAL GAS FIX INSTRUCTIONS

## For: Claude Code Opus
## Priority: URGENT - Bot is losing money

---

## Problem Statement

The bot executed a trade where the 0x API returned a gas estimate of **1,514,394** but the profitability check used a hardcoded estimate of **200,000**. This caused the bot to approve an unprofitable trade.

**Evidence from logs:**
```
Gas: 1514394 / 1514394  (0x leg)
Gas: 250000 / 250000    (Uniswap leg)
Total Gas Used: 1764394
USDC P/L: -0.001604
```

---

## Root Cause

The profitability check in `main.rs` calls `estimate_trade_profitability()` which uses hardcoded gas values BEFORE the actual 0x quote is fetched. The real 0x gas was 7.5x higher than estimated.

**Current broken flow:**
1. Spread detected
2. Profitability estimated with hardcoded 200k gas for 0x ← WRONG
3. Trade approved
4. 0x quote fetched (reveals 1.5M gas) ← TOO LATE
5. Trade executes at a loss

---

## Required Fixes

### Fix 1: Add Gas Guard Constants

**File:** `src/config.rs`

**Action:** Add these constants after the existing profitability thresholds section:

```
MAX_0X_GAS: u64 = 400_000
MAX_TOTAL_GAS: u64 = 700_000
```

**Purpose:** Hard limits to reject 0x routes that are too expensive.

---

### Fix 2: Create New Profitability Function

**File:** `src/execution.rs`

**Action:** Add a new function `estimate_trade_profitability_with_quote` that accepts the actual gas value from a 0x quote instead of using hardcoded estimates.

**Parameters needed:**
- `spread_pct: f64`
- `trade_value_usdc: f64`
- `mon_price_usdc: f64`
- `actual_0x_gas: u64` ← From the quote.transaction.gas field
- `uniswap_gas: u64` ← Use 250_000

**Returns:** `(gas_cost_mon: f64, net_profit_usdc: f64, is_profitable: bool)`

---

### Fix 3: Add Gas Validation Function

**File:** `src/execution.rs`

**Action:** Add a function `validate_0x_gas(quoted_gas: u64) -> Result<(), String>` that:
- Rejects if `quoted_gas > config::MAX_0X_GAS`
- Returns error message explaining why rejected

---

### Fix 4: Restructure Auto-Execute Flow in main.rs

**File:** `src/main.rs`

**Location:** The auto-execute block starting around line 375 (inside `if args.spread_threshold > 0.0 && spread_pct > args.spread_threshold`)

**Required change to the flow:**

**BEFORE (current order - broken):**
1. Check basic guards (trade size, spread for small trades)
2. Call `estimate_trade_profitability()` with hardcoded gas
3. If profitable, get `zrx.get_price()` 
4. Execute parallel arbitrage

**AFTER (new order - fixed):**
1. Check basic guards (trade size, spread for small trades)
2. Get **full 0x quote** using `zrx.get_quote()` (not `get_price`)
3. Parse `quote.transaction.gas` as u64
4. Call `validate_0x_gas()` - skip if gas too high
5. Call `estimate_trade_profitability_with_quote()` using actual gas
6. Skip if not profitable
7. Execute parallel arbitrage (pass the already-fetched quote to avoid double-fetch)

---

### Fix 5: Modify execute_parallel_arbitrage to Accept Pre-fetched Quote

**File:** `src/execution.rs`

**Action:** Add an optional parameter or create an overloaded version of `execute_parallel_arbitrage` that accepts an already-fetched `QuoteResponse` instead of fetching it internally.

**Reason:** The quote is now fetched in main.rs for gas validation. Passing it to the execution function avoids a redundant API call and ensures the same quote is used.

---

## Validation Criteria

After implementing these fixes, the bot must:

1. **Log the actual 0x gas** before deciding to trade
2. **Skip trades** where 0x gas > 400,000 with a clear log message
3. **Use actual gas** in profitability calculation, not hardcoded 200k
4. **Not double-fetch** the 0x quote (fetch once, use for both validation and execution)

---

## Test Command

After implementation, test with:
```bash
cargo run --release -- --spread-threshold 0.5 --wmon-amount 50.0 --slippage-bps 100
```

**Expected behavior:** When a high-gas 0x route is quoted (>400k), the bot should print a skip message like:
```
[SKIPPED] 0x gas too high: 1514394 > 400000 limit
```

---

## Files to Modify Summary

| File | Changes |
|------|---------|
| `src/config.rs` | Add MAX_0X_GAS and MAX_TOTAL_GAS constants |
| `src/execution.rs` | Add `estimate_trade_profitability_with_quote()` and `validate_0x_gas()` |
| `src/main.rs` | Restructure auto-execute block to fetch quote first, validate gas, then decide |

---

## Do NOT

- Do not remove the existing `estimate_trade_profitability()` function (it may be used elsewhere)
- Do not change the 0x API endpoints or authentication
- Do not modify the Uniswap execution logic
- Do not change gas limits on the actual transactions (250k for Uniswap is correct)
