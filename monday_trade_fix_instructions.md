# Monday Trade Router Fix Instructions

## Root Cause Analysis

The Monday Trade swap was reverting because:

1. **Wrong struct definition**: The current code uses SwapRouter02 style (WITHOUT deadline in struct), but Monday Trade uses the **original ISwapRouter** interface (WITH deadline in struct)
2. **Wrong fee tier**: Config uses `3000` (0.30%) but the pool uses `500` (0.05%)

## Confirmed Working Configuration (from Node.js test)

```
Fee tier: 500 (0.05%)
Deadline: INSIDE the struct (between recipient and amountIn)
Router: 0xFE951b693A2FE54BE5148614B109E316B567632F
```

## Files to Modify

### 1. `src/execution/routers/monday.rs` - COMPLETE REWRITE

Replace the entire file with the correct ISwapRouter interface:

```rust
use alloy::primitives::{Address, Bytes, U256, U160, Uint};
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;

// Monday Trade SwapRouter Interface
// IMPORTANT: Monday Trade uses the ORIGINAL ISwapRouter (from v3-periphery),
// NOT SwapRouter02. The key difference is that deadline is INSIDE the struct.
sol! {
    /// ExactInputSingleParams for original ISwapRouter
    /// NOTE: deadline IS in the struct (unlike SwapRouter02)
    #[derive(Debug)]
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 deadline;           // <-- CRITICAL: deadline is HERE
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    /// Swaps exact input for maximum output
    #[derive(Debug)]
    function exactInputSingle(ExactInputSingleParams calldata params)
        external
        payable
        returns (uint256 amountOut);
}

/// Build swap calldata for Monday Trade router (original ISwapRouter style - deadline IN struct)
pub fn build_exact_input_single(
    token_in: Address,
    token_out: Address,
    fee: u32,
    recipient: Address,
    amount_in: U256,
    amount_out_min: U256,
    deadline: u64,
) -> Result<Bytes> {
    let fee_uint24: Uint<24, 1> = Uint::from(fee);

    let params = ExactInputSingleParams {
        tokenIn: token_in,
        tokenOut: token_out,
        fee: fee_uint24,
        recipient,
        deadline: U256::from(deadline),
        amountIn: amount_in,
        amountOutMinimum: amount_out_min,
        sqrtPriceLimitX96: U160::ZERO,  // No price limit
    };

    let calldata = exactInputSingleCall { params }.abi_encode();
    Ok(Bytes::from(calldata))
}
```

### 2. `src/execution/routers/mod.rs` - Update Monday Trade call

Change the `RouterType::MondayTrade` match arm to pass `deadline`:

```rust
RouterType::MondayTrade => {
    monday::build_exact_input_single(
        token_in, token_out, pool_fee, recipient, amount_in, amount_out_min, deadline
    )
}
```

### 3. `src/config.rs` - Fix fee tier

Change the MondayTrade router config from `pool_fee: 3000` to `pool_fee: 500`:

```rust
RouterConfig {
    name: "MondayTrade",
    address: MONDAY_SWAP_ROUTER,
    router_type: RouterType::MondayTrade,
    pool_address: alloy::primitives::address!("8f889ba499c0a176fb8f233d9d35b1c132eb868c"),
    pool_fee: 500,  // 0.05% fee tier (NOT 3000!)
},
```

Also update the PoolConfig for MondayTrade pool:

```rust
pub fn get_monday_trade_pool() -> PoolConfig {
    PoolConfig {
        name: "MondayTrade",
        address: alloy::primitives::address!("8f889ba499c0a176fb8f233d9d35b1c132eb868c"),
        pool_type: PoolType::MondayTrade,
        fee_bps: 5, // 0.05% fee (NOT 30!)
    }
}
```

## Summary of Changes

| File | Change |
|------|--------|
| `src/execution/routers/monday.rs` | Add `deadline` field to struct, update function signature |
| `src/execution/routers/mod.rs` | Pass `deadline` parameter to Monday router |
| `src/config.rs` | Change `pool_fee: 3000` → `500`, change `fee_bps: 30` → `5` |

## Verification

After changes, test with:
```bash
cargo run --release -- test-swap --dex MondayTrade --amount 0.01 --direction sell --slippage 100
```

Expected: Swap should succeed without reverting.
