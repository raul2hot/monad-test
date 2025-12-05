# LFJ Swap Fix - Root Cause Analysis & Solution

## Problem Summary

Your LFJ swap was failing with "Transaction reverted" because the **bin step used in the swap calldata did not match the pool's actual bin step**.

## Root Cause

The LFJ Liquidity Book router uses a `Path` struct to route swaps:

```solidity
struct Path {
    uint256[] pairBinSteps;  // ← MUST match pool's bin step exactly!
    uint8[] versions;
    address[] tokenPath;
}
```

Internally, the router calls `_getPairs(path.pairBinSteps, path.versions, path.tokenPath)` to look up the trading pair. **If the bin step doesn't match exactly, the router won't find the pair and the transaction reverts.**

### Code Flow Analysis

1. **config.rs** sets `pool_fee: 15` for LFJ (intended as bin step)
2. **swap.rs** calls `build_swap_calldata(..., pool_fee, deadline)`
3. **routers/mod.rs** passes `deadline` to LFJ but **ignores `pool_fee`**:
   ```rust
   RouterType::LfjLB => {
       lfj::build_swap_exact_tokens_for_tokens(
           token_in, token_out, amount_in, amount_out_min, recipient, deadline
           // ⚠️ pool_fee is NOT passed!
       )
   }
   ```
4. **routers/lfj.rs** uses `DEFAULT_BIN_STEP: u64 = 20` instead of the actual pool's bin step

**Result**: Swap uses bin step 20, but pool uses bin step 15 (or another value) → Pair not found → Revert

## Fix

### 1. Update `src/execution/routers/lfj.rs`

Change the function signature to accept `bin_step` as a parameter:

```rust
pub fn build_swap_exact_tokens_for_tokens(
    token_in: Address,
    token_out: Address,
    amount_in: U256,
    amount_out_min: U256,
    recipient: Address,
    deadline: u64,
    bin_step: u32,  // ← NEW parameter!
) -> Result<Bytes> {
    let path = Path {
        pairBinSteps: vec![U256::from(bin_step)],  // ← Use actual bin step!
        versions: vec![3],
        tokenPath: vec![token_in, token_out],
    };
    // ... rest unchanged
}
```

### 2. Update `src/execution/routers/mod.rs`

Pass `pool_fee` as the bin step:

```rust
RouterType::LfjLB => {
    lfj::build_swap_exact_tokens_for_tokens(
        token_in, token_out, amount_in, amount_out_min, recipient, deadline,
        pool_fee,  // ← Pass as bin_step!
    )
}
```

### 3. Verify and Update Pool Configuration

Run the verification script to get the actual bin step:

```bash
chmod +x verify_lfj_bin_step.sh
./verify_lfj_bin_step.sh "$MONAD_RPC_URL" "0x5e60bc3f7a7303bc4dfe4dc2220bdc90bc04fe22"
```

Then update `config.rs` with the correct bin step:

```rust
RouterConfig {
    name: "LFJ",
    address: LFJ_LB_ROUTER,
    router_type: RouterType::LfjLB,
    pool_address: alloy::primitives::address!("5e60bc3f7a7303bc4dfe4dc2220bdc90bc04fe22"),
    pool_fee: 15,  // ← Use the actual bin step from verification!
}
```

## LFJ Bin Step Reference

| Bin Step | Base Fee | Typical Use Case        |
|----------|----------|-------------------------|
| 1        | ~0.01%   | Stablecoins (USDC/USDT) |
| 5        | ~0.05%   | Correlated assets       |
| 10       | ~0.10%   | Low volatility          |
| **15**   | ~0.15%   | **Medium volatility (likely WMON/USDC)** |
| 20       | ~0.20%   | Higher volatility       |
| 25       | ~0.25%   | High volatility         |
| 50       | ~0.50%   | Very high volatility    |
| 100      | ~1.00%   | Exotic pairs            |

## Testing After Fix

```bash
# Test the LFJ swap after applying fixes
cargo run --release -- test-swap --dex lfj --amount 0.1 --direction sell --slippage 100
```

## Additional Notes

### Version Field

The `versions` field should be set to `3` for V2.2 (current LFJ version):
- V1 = 0 (legacy)
- V2 = 1 (legacy) 
- V2.1 = 2
- **V2.2 = 3** ← Use this

### Token Order

For LFJ swaps, token order in `tokenPath` should be:
- `[tokenIn, tokenOut]` for exact input swaps
- Example: Selling WMON for USDC → `[WMON, USDC]`

This matches what you're already doing, so token order is correct.

### Common Issues

1. **Wrong bin step**: Most common cause of reverts
2. **Wrong version**: Use 3 for V2.2
3. **Pool doesn't exist**: Verify pool address on block explorer
4. **Insufficient liquidity**: Check pool reserves
