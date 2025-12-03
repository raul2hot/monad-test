# Gas & Profitability Optimization Instructions

## Critical Problem Analysis

Current trade results show **gas costs exceeding profits**:

| Metric | Before | After | Change |
|--------|--------|-------|--------|
| Native MON | 935.523143 | 935.093719 | **-0.429 MON (gas)** |
| WMON | 100.166606 | 100.250954 | +0.084 WMON |
| USDC | 2.862226 | 2.862612 | +0.000386 USDC |

**Root Cause**: With 6 WMON trade size and ~0.9% spread:
- Gas cost: ~0.43 MON ≈ **$0.014**
- Gross profit: ~$0.003 (spread + WMON gain)
- **Net loss: ~$0.011**

**Important Monad Behavior**: Monad charges for the FULL `gas_limit`, not `gas_used`. This is why reducing gas limits saves real money.

## Three Solutions (Implement ALL)

### Solution A: Reduce Gas Limits (Primary Fix) - SAFE VALUES
### Solution B: Increase Minimum Trade Size (Secondary Fix)
### Solution C: Add Profitability Estimation (Advanced)

---

## Solution A: Gas Limit Optimization (CONSERVATIVE/SAFE VALUES)

**Background**: Previous tests showed out-of-gas issues at 200k. However, 500k is excessive for single-hop swaps. We use **250k as a safe middle ground** that:
- Provides ~40% buffer over typical usage (150-180k)
- Avoids the out-of-gas issues seen at 200k
- Saves ~250k gas compared to current 500k limit

### File: `src/execution.rs`

#### Change 1: Reduce Uniswap Gas Limit (SAFE VALUE)

**Location**: Function `execute_uniswap_buy_no_wait` (around line 370)

**Find this code:**
```rust
let gas_limit = 500_000u64;
```

**Replace with:**
```rust
let gas_limit = 250_000u64;  // Safe reduction from 500k - typical V3 single swap uses 150-180k, buffer for edge cases
```

**Also update** the non-parallel version `execute_uniswap_buy` (around line 215):

**Find:**
```rust
let gas_limit = 500_000u64;  // Increased from 200k - Uniswap V3 swaps can need more
```

**Replace with:**
```rust
let gas_limit = 250_000u64;  // Safe value - typical V3 single swap uses 150-180k, buffer for edge cases
```

#### Change 2: Keep 0x Gas Buffer (UNCHANGED - Let API decide)

The 0x API returns a gas estimate optimized for their routing. Keep the existing buffer.

**Location**: `src/config.rs`

**KEEP AS-IS:**
```rust
pub const GAS_BUFFER: u64 = 5_000;  // Keep - 0x API provides good estimates
```

#### Change 3: Keep Gas Price Bump (UNCHANGED - Helps with inclusion)

**Location**: `src/config.rs`

**KEEP AS-IS:**
```rust
pub const GAS_PRICE_BUMP_PCT: u64 = 110;    // Keep - helps with fast inclusion
```

---

## Solution B: Minimum Trade Size Enforcement

### File: `src/config.rs`

**Add these new constants after the existing execution settings:**

```rust
// Profitability thresholds
pub const MIN_WMON_TRADE_AMOUNT: f64 = 30.0;     // Minimum 30 WMON per trade
pub const MIN_SPREAD_FOR_SMALL_TRADE: f64 = 2.0; // If trade < 50 WMON, require 2%+ spread
pub const RECOMMENDED_WMON_AMOUNT: f64 = 50.0;   // Recommended trade size for profitability
```

### File: `src/main.rs`

**Location**: Inside the auto-execute block, after the spread threshold check (around line 310)

**Find this block:**
```rust
if args.spread_threshold > 0.0 && spread_pct > args.spread_threshold {
    // Check if an execution is already in flight (prevent nonce collisions)
    if execution_in_flight.load(Ordering::SeqCst) {
        println!("  [SKIPPED] Execution already in flight, waiting for confirmation...");
        continue;
    }

    println!("\n========== SPREAD THRESHOLD TRIGGERED ==========");
```

**Replace with:**
```rust
if args.spread_threshold > 0.0 && spread_pct > args.spread_threshold {
    // Check if an execution is already in flight (prevent nonce collisions)
    if execution_in_flight.load(Ordering::SeqCst) {
        println!("  [SKIPPED] Execution already in flight, waiting for confirmation...");
        continue;
    }

    // Profitability guard: Ensure trade size is sufficient for gas costs
    if args.wmon_amount < config::MIN_WMON_TRADE_AMOUNT {
        println!(
            "  [SKIPPED] Trade size {:.1} WMON too small. Minimum: {:.1} WMON for profitability",
            args.wmon_amount,
            config::MIN_WMON_TRADE_AMOUNT
        );
        continue;
    }

    // For smaller trades, require higher spread
    if args.wmon_amount < config::RECOMMENDED_WMON_AMOUNT 
       && spread_pct < config::MIN_SPREAD_FOR_SMALL_TRADE {
        println!(
            "  [SKIPPED] Spread {:.2}% too low for {:.1} WMON trade. Need {:.1}%+ or increase to {:.0} WMON",
            spread_pct,
            args.wmon_amount,
            config::MIN_SPREAD_FOR_SMALL_TRADE,
            config::RECOMMENDED_WMON_AMOUNT
        );
        continue;
    }

    println!("\n========== SPREAD THRESHOLD TRIGGERED ==========");
```

---

## Solution C: Add Gas Cost Estimation (Advanced)

### File: `src/execution.rs`

**Add this new function after the imports (around line 30):**

```rust
/// Estimate gas cost in MON for a parallel arbitrage trade
/// Returns (estimated_gas_mon, is_profitable)
pub fn estimate_trade_profitability(
    spread_pct: f64,
    trade_value_usdc: f64,
    mon_price_usdc: f64,
) -> (f64, bool) {
    // Estimated gas usage (conservative safe limits)
    const UNISWAP_GAS: u64 = 250_000;  // Safe limit for V3 single swap
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
```

### File: `src/main.rs`

**Add this check inside the auto-execute block, right after the profitability guards:**

```rust
// Estimate profitability before executing
let trade_value_usdc = args.wmon_amount * uniswap_price;  // Approximate value
let (est_gas_mon, is_profitable) = execution::estimate_trade_profitability(
    spread_pct,
    trade_value_usdc,
    uniswap_price,
);

if !is_profitable {
    println!(
        "  [SKIPPED] Estimated unprofitable. Gas: ~{:.4} MON, Trade value: ${:.2}, Spread: {:.2}%",
        est_gas_mon,
        trade_value_usdc,
        spread_pct
    );
    println!("  Tip: Increase --wmon-amount to {} or wait for higher spread", config::RECOMMENDED_WMON_AMOUNT);
    continue;
}

println!("Profitability check PASSED. Est. gas: {:.4} MON", est_gas_mon);
```

---

## Updated CLI Defaults

### File: `src/main.rs`

**Update the default wmon_amount in Args struct:**

**Find:**
```rust
/// Amount of WMON to sell via 0x in parallel mode
#[arg(long, default_value = "10.0")]
wmon_amount: f64,
```

**Replace with:**
```rust
/// Amount of WMON to sell via 0x in parallel mode (min 30 for profitability)
#[arg(long, default_value = "50.0")]
wmon_amount: f64,
```

**Also update spread_threshold default:**

**Find:**
```rust
/// Spread threshold (%) to trigger auto-execution. 0 = monitoring only (default)
#[arg(long, default_value = "0.0")]
spread_threshold: f64,
```

**Replace with:**
```rust
/// Spread threshold (%) to trigger auto-execution. 0 = monitoring only (default)
#[arg(long, default_value = "1.5")]
spread_threshold: f64,
```

---

## Summary of All Required Changes

### File: `src/config.rs`

```rust
// === KEEP THESE EXISTING LINES UNCHANGED ===
pub const GAS_BUFFER: u64 = 5_000;          // Keep as-is
pub const GAS_PRICE_BUMP_PCT: u64 = 110;    // Keep as-is

// === ADD THESE NEW LINES (after existing execution settings) ===
// Profitability thresholds
pub const MIN_WMON_TRADE_AMOUNT: f64 = 30.0;
pub const MIN_SPREAD_FOR_SMALL_TRADE: f64 = 2.0;
pub const RECOMMENDED_WMON_AMOUNT: f64 = 50.0;
```

### File: `src/execution.rs`

```rust
// === MODIFY THESE EXISTING LINES ===
// In execute_uniswap_buy_no_wait (~line 370):
let gas_limit = 250_000u64;  // Was: 500_000u64 (SAFE reduction)

// In execute_uniswap_buy (~line 215):
let gas_limit = 250_000u64;  // Was: 500_000u64 (SAFE reduction)

// === ADD THIS NEW FUNCTION (after imports) ===
pub fn estimate_trade_profitability(
    spread_pct: f64,
    trade_value_usdc: f64,
    mon_price_usdc: f64,
) -> (f64, bool) {
    const UNISWAP_GAS: u64 = 250_000;  // Safe limit
    const ZRX_GAS: u64 = 200_000;      // Typical 0x
    const TOTAL_GAS: u64 = UNISWAP_GAS + ZRX_GAS;
    const GAS_PRICE_GWEI: f64 = 52.0;
    const GWEI_TO_MON: f64 = 0.000000001;
    
    let gas_cost_mon = (TOTAL_GAS as f64) * GAS_PRICE_GWEI * GWEI_TO_MON;
    let gas_cost_usdc = gas_cost_mon * mon_price_usdc;
    let gross_profit_usdc = trade_value_usdc * (spread_pct / 100.0);
    let fee_cost_usdc = trade_value_usdc * 0.0015;
    let net_profit_usdc = gross_profit_usdc - gas_cost_usdc - fee_cost_usdc;
    
    (gas_cost_mon, net_profit_usdc > 0.0)
}
```

### File: `src/main.rs`

```rust
// === MODIFY ARG DEFAULTS ===
#[arg(long, default_value = "50.0")]  // Was: 10.0
wmon_amount: f64,

#[arg(long, default_value = "1.5")]   // Was: 0.0
spread_threshold: f64,

// === ADD PROFITABILITY GUARDS in auto-execute block (after execution_in_flight check) ===
// See "Solution B" section above for complete code block
```

---

## Test Commands

### Test with optimized settings:
```bash
cargo run --release -- --spread-threshold 1.5 --wmon-amount 50.0 --slippage-bps 100
```

### Test parallel execution directly:
```bash
cargo run --release -- --test-parallel --wmon-amount 50.0 --slippage-bps 100
```

### Conservative test (higher threshold):
```bash
cargo run --release -- --spread-threshold 2.0 --wmon-amount 30.0 --slippage-bps 100
```

---

## Expected Improvements

| Metric | Before | After (Expected) |
|--------|--------|------------------|
| Uniswap gas limit | 500,000 | 250,000 |
| Total gas per trade | ~650,000 | ~450,000 |
| Gas cost (MON) | ~0.43 | ~0.025 |
| Min profitable trade | N/A | 30 WMON |
| Break-even spread (50 WMON) | N/A | ~1.5% |

---

## Important Notes

1. **Gas limits are now SAFELY reduced** - 250k provides ~40% buffer over typical 150-180k usage while avoiding the out-of-gas issues seen at 200k.

2. **Trade size matters** - Fixed gas costs mean larger trades = higher profit margins. The 30 WMON minimum ensures gas is < 5% of trade value.

3. **Spread requirements** - With reduced gas, 1.5% spread on 50 WMON should be profitable. Lower spreads require larger trade sizes.

4. **Monitor gas_used in logs** - If transactions revert with "out of gas", increase Uniswap limit to 300k.

5. **Monad gas prices** - Currently stable around 50-52 gwei. If this changes significantly, adjust the `estimate_trade_profitability` constants.

6. **0x gas is API-controlled** - We trust the 0x API gas estimate + buffer. Do NOT reduce this.

---

## If You See Out-of-Gas Errors

If Uniswap transactions fail with out-of-gas, use these escalating values:

```rust
// Level 1 (Current - SAFE):
let gas_limit = 250_000u64;

// Level 2 (If still failing):
let gas_limit = 300_000u64;

// Level 3 (Maximum safe - original was here):
let gas_limit = 350_000u64;

// DO NOT go back to 500k - that's wasteful
```

---

## Verification Checklist

After implementing changes:

- [ ] `cargo build --release` compiles without errors
- [ ] `--test-parallel` runs successfully with 50 WMON
- [ ] Gas used in logs shows < 250k for Uniswap leg (confirms headroom)
- [ ] Profitability guard prints skip messages for small trades
- [ ] Auto-execute triggers on spreads > 1.5%
- [ ] Post-trade USDC balance shows profit (not loss)
- [ ] NO out-of-gas errors in transaction receipts