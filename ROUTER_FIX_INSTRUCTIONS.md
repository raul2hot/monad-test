# Router Fix Instructions for LFJ and Monday Trade

## Executive Summary

Both LFJ and Monday Trade swaps are failing because we're using the **wrong interface**:
- **LFJ**: Uses TraderJoe Liquidity Book interface, NOT Uniswap V3 `exactInputSingle`
- **Monday Trade**: Uses SwapRouter02 interface which has NO `deadline` in the struct

---

## PART 1: LFJ (TraderJoe Liquidity Book) Router Fix

### Root Cause
LFJ is NOT a Uniswap V3 fork. It's TraderJoe's **Liquidity Book DLMM** protocol which uses a completely different swap interface based on `Path` structs.

### Current Wrong Code (Do NOT use)
```rust
// WRONG - LFJ does not use exactInputSingle
sol! {
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 deadline;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }
    function exactInputSingle(ExactInputSingleParams calldata params)
        external payable returns (uint256 amountOut);
}
```

### Correct LFJ Interface

```rust
use alloy::primitives::{Address, Bytes, U256};
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;

// LFJ Liquidity Book Router V2.2 Interface
sol! {
    /// Version enum for LFJ pairs
    /// V1 = 0 (JoeV1 - legacy AMM)
    /// V2 = 1 (Liquidity Book V2 - legacy)
    /// V2_1 = 2 (Liquidity Book V2.1)
    /// V2_2 = 3 (Liquidity Book V2.2 - CURRENT) <-- USE THIS
    
    /// Path struct for routing swaps
    /// @param pairBinSteps The bin step for each pair (determines fee tier)
    /// @param versions The version for each pair (use 3 for V2_2)
    /// @param tokenPath The tokens to swap through [tokenIn, tokenOut]
    #[derive(Debug)]
    struct Path {
        uint256[] pairBinSteps;
        uint8[] versions;      // Use uint8 for Version enum
        address[] tokenPath;
    }

    /// Swaps exact tokens for tokens using Liquidity Book
    /// @param amountIn The amount of input tokens to swap
    /// @param amountOutMin The minimum amount of output tokens to receive
    /// @param path The Path struct containing routing information
    /// @param to The recipient address
    /// @param deadline Transaction deadline timestamp
    /// @return amountOut The amount of output tokens received
    #[derive(Debug)]
    function swapExactTokensForTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        Path memory path,
        address to,
        uint256 deadline
    ) external returns (uint256 amountOut);
}

/// LFJ Router address on Monad Mainnet
pub const LFJ_ROUTER_ADDRESS: &str = "0x18556DA13313f3532c54711497A8FedAC273220E";

/// LFJ Pool address for WMON/USDC
pub const LFJ_WMON_USDC_POOL: &str = "0x5e60bc3f7a7303bc4dfe4dc2220bdc90bc04fe22";

/// Build swap calldata for LFJ router
pub fn build_lfj_swap_calldata(
    token_in: Address,
    token_out: Address,
    amount_in: U256,
    amount_out_min: U256,
    recipient: Address,
    deadline: u64,
    bin_step: u64,  // Fee tier: typically 15, 20, or 25 for volatile pairs
) -> Result<Bytes> {
    // Build the Path struct
    let path = Path {
        pairBinSteps: vec![U256::from(bin_step)],
        versions: vec![3],  // V2_2 = 3
        tokenPath: vec![token_in, token_out],
    };

    let calldata = swapExactTokensForTokensCall {
        amountIn: amount_in,
        amountOutMin: amount_out_min,
        path,
        to: recipient,
        deadline: U256::from(deadline),
    }
    .abi_encode();

    Ok(Bytes::from(calldata))
}
```

### LFJ Bin Steps (Fee Tiers)

| Bin Step | Fee | Use Case |
|----------|-----|----------|
| 1 | 0.01% | Stablecoins |
| 5 | 0.05% | Correlated assets |
| 10 | 0.10% | Low volatility |
| 15 | 0.15% | Medium volatility |
| 20 | 0.20% | **LIKELY WMON/USDC** |
| 25 | 0.25% | High volatility |
| 50 | 0.50% | Very high volatility |
| 100 | 1.00% | Exotic pairs |

**To find the correct bin step**, query the LFJ pool contract:
```rust
sol! {
    function getBinStep() external view returns (uint16 binStep);
}
```

Or query the LBFactory to find all pairs:
```rust
// LFJ Factory on Monad
pub const LFJ_FACTORY: &str = "0xb43120c4745967fa9b93E79C149E66B0f2D6Fe0c";

sol! {
    function getLBPairInformation(
        address tokenA,
        address tokenB,
        uint256 binStep
    ) external view returns (address lbPair);
}
```

---

## PART 2: Monday Trade Router Fix

### Root Cause
Monday Trade is built on **SynFutures infrastructure** and uses the **SwapRouter02** interface, which does NOT have `deadline` inside the struct (deadline is checked separately via a modifier).

### Current Wrong Code (Do NOT use)
```rust
// WRONG - Monday Trade uses SwapRouter02, not SwapRouter
sol! {
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 deadline;        // <-- WRONG: SwapRouter02 does NOT have this
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }
}
```

### Correct Monday Trade Interface (SwapRouter02)

```rust
use alloy::primitives::{Address, Bytes, U160, U256};
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;

// Monday Trade SwapRouter02 Interface (NO deadline in struct)
sol! {
    /// ExactInputSingleParams for SwapRouter02
    /// NOTE: No deadline field - deadline is handled by multicall wrapper
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

    /// Swaps exact input for maximum output
    #[derive(Debug)]
    function exactInputSingle(ExactInputSingleParams calldata params)
        external payable returns (uint256 amountOut);
    
    /// Multicall with deadline (use this to wrap calls with deadline check)
    #[derive(Debug)]
    function multicall(uint256 deadline, bytes[] calldata data)
        external payable returns (bytes[] memory results);
}

/// Monday Trade Router address on Monad Mainnet
pub const MONDAY_ROUTER_ADDRESS: &str = "0xFE951b693A2FE54BE5148614B109E316B567632F";

/// Monday Trade Pool address for WMON/USDC
pub const MONDAY_WMON_USDC_POOL: &str = "0x8f889ba499c0a176fb8f233d9d35b1c132eb868c";

/// Build swap calldata for Monday Trade router (SwapRouter02 style)
pub fn build_monday_swap_calldata(
    token_in: Address,
    token_out: Address,
    fee: u32,  // Fee in hundredths of bps (3000 = 0.3%)
    amount_in: U256,
    amount_out_min: U256,
    recipient: Address,
) -> Result<Bytes> {
    let params = ExactInputSingleParams {
        tokenIn: token_in,
        tokenOut: token_out,
        fee: fee.try_into().unwrap_or(3000),
        recipient,
        amountIn: amount_in,
        amountOutMinimum: amount_out_min,
        sqrtPriceLimitX96: U160::ZERO,  // No price limit
    };

    let calldata = exactInputSingleCall { params }.abi_encode();
    Ok(Bytes::from(calldata))
}

/// Build swap calldata wrapped with deadline using multicall
pub fn build_monday_swap_with_deadline(
    token_in: Address,
    token_out: Address,
    fee: u32,
    amount_in: U256,
    amount_out_min: U256,
    recipient: Address,
    deadline: u64,
) -> Result<Bytes> {
    // First, build the inner exactInputSingle call
    let inner_calldata = build_monday_swap_calldata(
        token_in,
        token_out,
        fee,
        amount_in,
        amount_out_min,
        recipient,
    )?;

    // Wrap it in multicall with deadline
    let calldata = multicallCall {
        deadline: U256::from(deadline),
        data: vec![inner_calldata],
    }
    .abi_encode();

    Ok(Bytes::from(calldata))
}
```

### Alternative: Try Original SwapRouter Interface First

If the SwapRouter02 approach fails, Monday Trade might use the original SwapRouter with deadline in struct. Try this:

```rust
// Alternative: Original SwapRouter interface WITH deadline in struct
sol! {
    #[derive(Debug)]
    struct ExactInputSingleParamsV1 {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 deadline;          // <-- HAS deadline
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    #[derive(Debug)]
    function exactInputSingle(ExactInputSingleParamsV1 calldata params)
        external payable returns (uint256 amountOut);
}
```

---

## PART 3: Implementation Checklist

### For LFJ Router (`src/execution/routers/lfj.rs`):

1. [ ] Remove any `exactInputSingle` code
2. [ ] Add `Path` struct definition
3. [ ] Add `swapExactTokensForTokens` function definition  
4. [ ] Use Version `3` (V2_2) in the versions array
5. [ ] Query or hardcode the correct `binStep` for WMON/USDC pool
6. [ ] Test bin steps: try 15, 20, 25 in order until one works

### For Monday Trade Router (`src/execution/routers/monday.rs`):

1. [ ] Remove `deadline` from `ExactInputSingleParams` struct
2. [ ] Keep all other fields: tokenIn, tokenOut, fee, recipient, amountIn, amountOutMinimum, sqrtPriceLimitX96
3. [ ] If direct call fails, try wrapping with `multicall(deadline, data)`
4. [ ] If SwapRouter02 fails, try original SwapRouter with deadline in struct

---

## PART 4: Testing Strategy

### Test Command Format
```bash
# Test LFJ swap
cargo run -- test-swap lfj 1.0 --slippage 2.0

# Test Monday Trade swap
cargo run -- test-swap monday 1.0 --slippage 2.0
```

### Debugging Tips

1. **Check function selector**: Ensure the 4-byte selector matches:
   - LFJ `swapExactTokensForTokens`: Calculate from signature
   - Monday `exactInputSingle`: `0x04e45aaf` (SwapRouter02) or `0x414bf389` (SwapRouter)

2. **Log the calldata**: Print the hex-encoded calldata and verify it matches expected format

3. **Check approvals**: Ensure tokens are approved to the correct router address

4. **Verify pool exists**: Query the factory to confirm the pool address is correct

---

## PART 5: Contract Addresses Summary

| Contract | Address | Notes |
|----------|---------|-------|
| **LFJ Router** | `0x18556DA13313f3532c54711497A8FedAC273220E` | Liquidity Book V2.2 |
| **LFJ Factory** | `0xb43120c4745967fa9b93E79C149E66B0f2D6Fe0c` | Query for pools |
| **LFJ WMON/USDC Pool** | `0x5e60bc3f7a7303bc4dfe4dc2220bdc90bc04fe22` | |
| **Monday Router** | `0xFE951b693A2FE54BE5148614B109E316B567632F` | SwapRouter02 style |
| **Monday Pool** | `0x8f889ba499c0a176fb8f233d9d35b1c132eb868c` | |
| **WMON** | `0x3bd359C1119dA7Da1D913D1C4D2B7c461115433A` | Wrapped MON |
| **USDC** | `0x754704Bc059F8C67012fEd69BC8A327a5aafb603` | |

---

## PART 6: Quick Reference - Key Differences

| Feature | Uniswap V3 | LFJ (Liquidity Book) | Monday Trade |
|---------|------------|---------------------|--------------|
| Function | `exactInputSingle` | `swapExactTokensForTokens` | `exactInputSingle` |
| Fee Parameter | `uint24 fee` | `binStep` in Path | `uint24 fee` |
| Deadline | In struct | Separate parameter | NOT in struct (multicall) |
| Path Format | `tokenIn,tokenOut` | `Path{binSteps,versions,tokens}` | `tokenIn,tokenOut` |
| Version | - | V2_2 = 3 | SwapRouter02 |

---

## PART 7: Expected Gas Usage

| DEX | Estimated Gas | Notes |
|-----|---------------|-------|
| Uniswap | ~220,000 | Confirmed working |
| PancakeSwap | ~275,000 | Confirmed working |
| LFJ | ~200,000-250,000 | Liquidity Book is efficient |
| Monday | ~200,000-250,000 | Similar to Uniswap V3 |

Remember: On Monad, you pay for gas limit, not gas used. Set precise limits!

---

## PART 8: Fallback Strategy

If the above interfaces still fail:

1. **Fetch contract bytecode** and decode the function selectors
2. **Use cast** to call the contract and inspect revert reasons:
   ```bash
   cast call <router> "exactInputSingle((address,address,uint24,address,uint256,uint256,uint160))" \
     "(0xWMON,0xUSDC,3000,0xRECIPIENT,1000000000000000000,0,0)" \
     --rpc-url $MONAD_RPC_URL
   ```
3. **Check block explorer** for successful swap transactions and decode their calldata
4. **Contact protocol teams** on Discord for correct interface documentation
