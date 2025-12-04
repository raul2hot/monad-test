# Gas Limit Fix Instructions
**For:** Claude Code Opus  
**Priority:** CRITICAL - Do this NOW before any testing

---

## The Problem

Uniswap `exactOutputSingle` swaps are failing with OUT OF GAS error:
```
Leg A (Uniswap BUY): FAILED - Gas: 250000/250000
```

The current 250k gas limit is NOT enough for exactOutput swaps.

---

## The Fix

### File: `src/execution.rs`

There are **3 functions** with hardcoded `gas_limit = 250_000u64`. Change ALL of them to `400_000u64`.

---

### Location 1: `execute_uniswap_buy_exact_output_no_wait` (THE FAILING ONE)

Around line 470-490, find:
```rust
let gas_limit = 250_000u64;
```

Change to:
```rust
let gas_limit = 400_000u64;  // exactOutput needs more gas than exactInput
```

---

### Location 2: `execute_uniswap_buy_no_wait`

Around line 430-450, find:
```rust
let gas_limit = 250_000u64;  // Safe value - typical V3 single swap uses 150-180k, buffer for edge cases
```

Change to:
```rust
let gas_limit = 400_000u64;  // exactOutput needs more gas than exactInput
```

---

### Location 3: `execute_uniswap_buy`

Around line 290-310, find:
```rust
let gas_limit = 250_000u64;  // Safe value - typical V3 single swap uses 150-180k, buffer for edge cases
```

Change to:
```rust
let gas_limit = 400_000u64;  // exactOutput needs more gas than exactInput
```

---

### Location 4: `estimate_trade_profitability` (for accurate estimates)

Around line 15-20, find:
```rust
const UNISWAP_GAS: u64 = 250_000;  // Safe limit for V3 single swap
```

Change to:
```rust
const UNISWAP_GAS: u64 = 400_000;  // exactOutput needs more gas
```

---

### Location 5: `validate_0x_gas` (update total gas limit)

Around line 75-85, find:
```rust
const UNISWAP_GAS: u64 = 250_000;
```

Change to:
```rust
const UNISWAP_GAS: u64 = 400_000;
```

---

### File: `src/main.rs`

### Location 6: Inside the auto-execute block

Around line 390, find:
```rust
const UNISWAP_GAS: u64 = 250_000;
```

Change to:
```rust
const UNISWAP_GAS: u64 = 400_000;
```

---

### File: `src/config.rs`

### Location 7: Update MAX_TOTAL_GAS

Around line 30, find:
```rust
pub const MAX_TOTAL_GAS: u64 = 700_000;          // Max total gas for both legs combined
```

Change to:
```rust
pub const MAX_TOTAL_GAS: u64 = 850_000;          // Max total gas for both legs combined (400k Uni + 400k 0x + buffer)
```

---

## Summary of Changes

| File | What to Find | Change To |
|------|--------------|-----------|
| `src/execution.rs` | `gas_limit = 250_000u64` (3 places) | `400_000u64` |
| `src/execution.rs` | `const UNISWAP_GAS: u64 = 250_000` (2 places) | `400_000` |
| `src/main.rs` | `const UNISWAP_GAS: u64 = 250_000` | `400_000` |
| `src/config.rs` | `MAX_TOTAL_GAS: u64 = 700_000` | `850_000` |

**Total: 7 changes across 3 files**

---

## Quick Find Command

```bash
grep -rn "250_000\|250000" src/
```

This will show all locations that need updating.

---

## Verification

After making changes:
```bash
cargo build --release
```

Confirm no compilation errors.

---

## Test Command

```bash
cargo run --release -- --spread-threshold 2.0 --wmon-amount 50.0 --slippage-bps 100
```

Watch for:
- Both legs should show SUCCESS
- Uniswap gas used should be < 400k (confirming headroom)

---

## Why 400k?

| Swap Type | Typical Gas | Safe Limit |
|-----------|-------------|------------|
| exactInput | 150-180k | 250k |
| **exactOutput** | **280-350k** | **400k** |

exactOutput requires more computation to calculate the input amount dynamically.
