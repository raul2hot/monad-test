# CRITICAL FIX: Trade Amount Imbalance

## For: Claude Code Opus
## Priority: URGENT - Bot is leaking WMON on every trade

---

## Problem Statement

The bot is losing ~0.1 WMON per trade due to mismatched amounts between legs.

**Evidence from logs (00:08:16 trade):**
```
WMON Sold (0x):       50.000000
USDC Spent (Uniswap): 1.517701
WMON Change: -0.099959   ← LOST 0.1 WMON
USDC Change: +0.000001   ← Gained nothing
```

The bot sold 50 WMON but only bought back ~49.9 WMON worth. This happens because:
1. 0x quote says: "50 WMON = $1.517701"
2. Bot sends $1.517701 to Uniswap
3. Uniswap has a DIFFERENT price, returns only ~49.9 WMON
4. Net: Lost 0.1 WMON, gained $0.000001

---

## Root Cause

**Current (broken) logic in main.rs:**
```
1. Get 0x quote for selling X WMON → receive Y USDC
2. Use Y USDC as input to Uniswap BUY
3. Uniswap returns Z WMON (where Z < X due to price difference)
4. Result: Sold X WMON, bought Z WMON, lost (X - Z) WMON
```

**The amounts are backwards.** The bot should ensure it BUYS at least as much WMON as it SELLS.

---

## Required Fix

### Option A: Match WMON Amounts (Recommended)

Change the logic to:
1. Decide on WMON amount to trade (e.g., 50 WMON)
2. Calculate USDC needed on Uniswap to BUY 50 WMON (use `exactOutputSingle` instead of `exactInputSingle`)
3. Get 0x quote to SELL 50 WMON
4. Verify: USDC from 0x > USDC spent on Uniswap
5. Execute both legs with matching WMON amounts

### Option B: Match USDC Amounts (Alternative)

Change the logic to:
1. Decide on USDC amount to trade (e.g., $1.50)
2. Use $1.50 on Uniswap to BUY WMON → get X WMON
3. Get 0x quote to SELL X WMON → receive Y USDC
4. Verify: Y > $1.50
5. Execute

**Option A is better** because it guarantees WMON inventory stays constant.

---

## Implementation Instructions

### Step 1: Add Uniswap exactOutputSingle Support

**File:** `src/execution.rs`

Add a new Solidity interface for `exactOutputSingle`:

```rust
sol! {
    struct ExactOutputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 amountOut;        // The exact amount of WMON we want to receive
        uint256 amountInMaximum;  // Max USDC we're willing to spend
        uint160 sqrtPriceLimitX96;
    }

    function exactOutputSingle(ExactOutputSingleParams calldata params)
        external payable returns (uint256 amountIn);
}
```

### Step 2: Create New Uniswap BUY Function

**File:** `src/execution.rs`

Add function `execute_uniswap_buy_exact_output`:

**Parameters:**
- `provider`
- `wallet`
- `wmon_amount_out: U256` - Exact WMON we want to receive
- `max_usdc_in: U256` - Maximum USDC we're willing to spend
- `pool_fee: u32`
- `nonce: u64`

**Returns:** `PendingLegResult`

**Implementation notes:**
- Use `exactOutputSingle` instead of `exactInputSingle`
- Set `amountOut` to the desired WMON amount
- Set `amountInMaximum` to a value with slippage buffer (e.g., 0x quote amount * 1.02)

### Step 3: Update Parallel Execution Logic

**File:** `src/execution.rs` - function `execute_parallel_arbitrage`

Change the flow:

**Current (broken):**
```
usdc_amount = from 0x quote
wmon_amount = user specified (50)
Uniswap: spend usdc_amount, receive unknown WMON
0x: sell wmon_amount, receive USDC
```

**New (fixed):**
```
wmon_amount = user specified (50)
0x quote: sell 50 WMON → receive X USDC
Uniswap: buy EXACTLY 50 WMON, spend up to X * 1.02 USDC
0x: sell 50 WMON, receive X USDC
Net: WMON unchanged, USDC profit = X - Uniswap cost
```

### Step 4: Update Profitability Check

**File:** `src/main.rs`

The profitability calculation needs to account for the fact that we're now doing `exactOutput`:

```
Expected profit = USDC from 0x - (USDC needed for Uniswap exactOutput)
```

To get "USDC needed for Uniswap exactOutput", you can either:
1. Query Uniswap's quoter contract
2. Estimate from the pool price: `wmon_amount * uniswap_price * (1 + slippage)`

Option 2 is simpler for now.

---

## Updated Trade Flow

```
1. User wants to trade 50 WMON
2. Get 0x quote: "Sell 50 WMON → $1.52 USDC"
3. Check gas (existing logic) - skip if > 400k
4. Estimate Uniswap cost: 50 * $0.0304 = $1.52 (from pool price)
5. Calculate profit: $1.52 (0x) - $1.52 (Uniswap) - gas = ~$0
6. If profitable, execute:
   - Leg A: Uniswap exactOutputSingle - buy exactly 50 WMON, spend up to $1.55
   - Leg B: 0x sell - sell exactly 50 WMON, receive $1.52
7. Result: WMON unchanged, USDC profit/loss based on actual execution
```

---

## Files to Modify

| File | Changes |
|------|---------|
| `src/execution.rs` | Add `ExactOutputSingleParams` struct, add `execute_uniswap_buy_exact_output` function, modify `execute_parallel_arbitrage` to use exact output |
| `src/main.rs` | Update profitability calculation to compare 0x output vs estimated Uniswap input |

---

## Validation

After implementation, run a test trade. The logs should show:

```
WMON Sold (0x):       50.000000
WMON Bought (Uniswap): 50.000000  ← MUST MATCH
WMON Change: 0.000000             ← Should be zero (or tiny dust)
USDC Change: +X.XXXXXX            ← This is the actual profit
```

---

## Why This Matters

Current state: Every trade loses ~0.1 WMON (~$0.003) regardless of spread
Fixed state: WMON inventory stays constant, profit/loss is purely in USDC

At 10 trades per hour, current bot loses ~$0.03/hour in WMON leakage alone.

---

## Fallback: If exactOutputSingle is Complex

If implementing `exactOutputSingle` is complex, an alternative quick fix:

**Reduce the 0x sell amount to match expected Uniswap output:**

```rust
// In main.rs, before executing
let expected_wmon_from_uniswap = usdc_amount / uniswap_price;
let wmon_to_sell = expected_wmon_from_uniswap * 0.99; // Sell slightly less to be safe
```

This is a band-aid but would stop the bleeding immediately.
