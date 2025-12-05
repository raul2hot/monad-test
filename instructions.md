# Fix Instructions for Monad Arbitrage Bot - BUY Direction Failure

## Root Cause Analysis

**Pattern observed:**
- SELL direction (WMON → USDC): Works on all DEXes
- BUY direction (USDC → WMON): Fails on all DEXes (LFJ, PancakeSwap)

This is a **direction-specific bug**, not a gas limit issue.

---

## BUG #1: PancakeSwap SmartRouter Uses Different Interface

### File
`src/execution/routers/pancake_v3.rs`

### Problem
PancakeSwap SmartRouter V3 (`0x21114915Ac6d5A2e156931e20B20b038dEd0Be7C`) uses **IV3SwapRouter** interface which is DIFFERENT from Uniswap's SwapRouter02.

The PancakeSwap SmartRouter's `exactInputSingle` requires **deadline** in the struct (like Monday Trade), but the current code uses the Uniswap SwapRouter02 style (no deadline).

### Current (Wrong)
```rust
struct ExactInputSingleParams {
    address tokenIn;
    address tokenOut;
    uint24 fee;
    address recipient;
    uint256 amountIn;
    uint256 amountOutMinimum;
    uint160 sqrtPriceLimitX96;
}
```

### Fix
Replace the entire `src/execution/routers/pancake_v3.rs` with:

```rust
use alloy::primitives::{Address, Bytes, U256, U160, Uint};
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;

// PancakeSwap V3 SmartRouter Interface
// IMPORTANT: PancakeSwap SmartRouter uses IV3SwapRouter which has deadline OUTSIDE the struct
// but wrapped in a multicall with deadline. For direct calls, we need to use the
// exactInputSingle that matches their deployed bytecode.
//
// After checking PancakeSwap's SmartRouter, it actually uses the SAME interface as SwapRouter02
// BUT the function selector might be different, OR we need to go through multicall.
//
// Alternative: Use the V3Pool directly or use exactInputSingleHop
sol! {
    // Standard exactInputSingle (SwapRouter02 style - no deadline in struct)
    #[derive(Debug)]
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    #[derive(Debug)]
    function exactInputSingle(ExactInputSingleParams calldata params)
        external
        payable
        returns (uint256 amountOut);
}

pub fn build_exact_input_single(
    token_in: Address,
    token_out: Address,
    fee: u32,
    recipient: Address,
    amount_in: U256,
    amount_out_min: U256,
) -> Result<Bytes> {
    let fee_uint24: Uint<24, 1> = Uint::from(fee);

    let params = ExactInputSingleParams {
        tokenIn: token_in,
        tokenOut: token_out,
        fee: fee_uint24,
        recipient,
        amountIn: amount_in,
        amountOutMinimum: amount_out_min,
        sqrtPriceLimitX96: U160::ZERO,
    };

    let calldata = exactInputSingleCall { params }.abi_encode();
    Ok(Bytes::from(calldata))
}
```

**NOTE:** If this doesn't work, PancakeSwap might require going through their `multicall` with deadline. Check their docs.

---

## BUG #2: LFJ Token Order Issue

### File
`src/execution/routers/lfj.rs`

### Problem
LFJ Liquidity Book requires tokens in a specific order in the path. The token order may need to be sorted (lower address first).

Check the token addresses:
- WMON: `0x3bd359C1119dA7Da1D913D1C4D2B7c461115433A`
- USDC: `0x754704Bc059F8C67012fEd69BC8A327a5aafb603`

WMON < USDC by address, so WMON is token0.

### Verification
For BUY direction (USDC → WMON):
- `token_in = USDC`
- `token_out = WMON`
- Path should be `[USDC, WMON]`

This looks correct, but LFJ might have issues with the direction. 

### Fix
Add debug logging to verify the path being sent:

```rust
pub fn build_swap_with_bin_step(
    token_in: Address,
    token_out: Address,
    amount_in: U256,
    amount_out_min: U256,
    recipient: Address,
    deadline: u64,
    bin_step: u64,
) -> Result<Bytes> {
    println!("  [LFJ DEBUG] token_in: {:?}", token_in);
    println!("  [LFJ DEBUG] token_out: {:?}", token_out);
    println!("  [LFJ DEBUG] amount_in: {}", amount_in);
    println!("  [LFJ DEBUG] amount_out_min: {}", amount_out_min);
    println!("  [LFJ DEBUG] bin_step: {}", bin_step);
    
    let path = Path {
        pairBinSteps: vec![U256::from(bin_step)],
        versions: vec![3],
        tokenPath: vec![token_in, token_out],
    };

    let calldata = swapExactTokensForTokensCall {
        amountIn: amount_in,
        amountOutMin: amount_out_min,
        path,
        to: recipient,
        deadline: U256::from(deadline),
    }.abi_encode();

    Ok(Bytes::from(calldata))
}
```

---

## BUG #3: Amount Calculation Issue for Small USDC Amounts

### File
`src/execution/swap.rs`

### Problem
For BUY direction with 0.026725 USDC, the expected WMON output is calculated as:
```
expected_out = amount_in / expected_price = 0.026725 / 0.026880 = 0.9942 WMON
```

But this expects to get ~1 WMON from ~0.027 USDC, which means the slippage-adjusted minimum is:
```
min_out = 0.9942 * 0.97 = 0.964 WMON
```

**This minimum is unrealistic!** The pool cannot give 0.964 WMON for 0.027 USDC with any reasonable slippage.

### Root Cause
The price `0.026880` means "0.026880 USDC per WMON". So for BUY:
- You spend USDC
- You receive WMON
- Expected WMON = USDC_amount / price = 0.027 / 0.027 ≈ 1 WMON

Wait - this IS correct if WMON costs $0.027!

But the **amountOutMinimum might be too high** due to price impact on small trades.

### Fix - Set amountOutMinimum to 0 for Testing

In `src/execution/swap.rs`, temporarily set min output to 0 to test:

```rust
// Calculate minimum output based on expected price and slippage
let expected_out = match params.direction {
    SwapDirection::Sell => params.amount_in * params.expected_price,
    SwapDirection::Buy => params.amount_in / params.expected_price,
};

let slippage_multiplier = 1.0 - (params.slippage_bps as f64 / 10000.0);
let min_out = expected_out * slippage_multiplier;

// TEMPORARY DEBUG: Set to 0 to see if swap works without slippage protection
let amount_out_min = U256::ZERO;  // CHANGE THIS BACK AFTER TESTING!
// let amount_out_min = to_wei(min_out, decimals_out);

println!("  [DEBUG] expected_out: {}", expected_out);
println!("  [DEBUG] min_out (before zero): {}", min_out);
println!("  [DEBUG] amount_out_min: {}", amount_out_min);
```

---

## Immediate Debugging Steps

1. **Add debug logging** to see exact calldata being sent
2. **Set amountOutMinimum to 0** temporarily to isolate slippage vs encoding issues
3. **Test BUY direction with test-swap** first (not test-arb):

```bash
# Test BUY direction directly on each DEX
cargo run -- test-swap --dex pancakeswap1 --amount 0.03 --direction buy --slippage 500
cargo run -- test-swap --dex lfj --amount 0.03 --direction buy --slippage 500
cargo run -- test-swap --dex uniswap --amount 0.03 --direction buy --slippage 500
```

If test-swap BUY fails on all DEXes, the issue is in `execute_swap` or router encoding.
If test-swap BUY works but test-arb fails, the issue is in test-arb's amount calculation.

---

## Most Likely Root Cause

After deeper analysis, the most likely issue is:

**The `amountOutMinimum` is too high relative to what the pool can actually provide.**

For a swap of 0.027 USDC expecting 0.96 WMON minimum, the pool needs extreme liquidity. With low liquidity, even 3% slippage isn't enough.

### Quick Fix
Increase slippage dramatically for testing:

```bash
cargo run -- test-arb --sell-dex uniswap --buy-dex pancakeswap1 --amount 1.0 --slippage 1000
```

If this works with 10% slippage, the issue is slippage tolerance, not encoding.

---

## Gas Limits are NOT the Issue

The current gas limits (280k for V3, 420k for LFJ) are fine. The error is "Transaction reverted" not "out of gas". The contract is rejecting the swap parameters.