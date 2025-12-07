# Atomic Arbitrage USDC Balance Bug Fix

## Overview

Fix a critical bug where swap 2 uses a PRE-CALCULATED USDC amount instead of the ACTUAL USDC received from swap 1. This causes USDC balance leakage from the contract.

---

## The Bug

**Location:** `src/execution/atomic_arb.rs` lines ~180-185

**Current broken code:**
```rust
// For swap 2, use conservative USDC estimate
let usdc_for_swap2 = expected_usdc * 0.999; // Tiny buffer for dust
let usdc_for_swap2_wei = to_wei(usdc_for_swap2, USDC_DECIMALS);
```

**Problem:** Rust pre-calculates `usdc_for_swap2` BEFORE swap 1 executes. The Solidity contract receives this pre-built calldata and uses it blindly.

**Result:** 
- Swap 1 might return 0.041097 USDC (actual)
- Swap 2 tries to spend 0.038455 USDC (pre-calculated estimate)
- Contract loses the difference: -0.002642 USDC per trade

---

## The Fix

Build swap 2 calldata ON-CHAIN in Solidity using the ACTUAL USDC balance after swap 1.

---

## Step 1: Modify Solidity Contract

**File:** `contracts/src/MonadAtomicArb.sol`

### 1.1 Change the `executeArb` function signature

**Remove:** `bytes calldata buyRouterData` parameter  
**Add:** `uint8 buyRouter`, `uint24 buyPoolFee`, `uint256 minWmonOut`

**New signature:**
```solidity
function executeArb(
    Router sellRouter,
    bytes calldata sellRouterData,
    Router buyRouter,
    uint24 buyPoolFee,
    uint256 minWmonOut,
    uint256 minProfit
) external onlyOwner returns (int256 profit)
```

### 1.2 Add helper function to build swap calldata on-chain

Add this function before `executeArb`:

```solidity
/// @notice Build exactInputSingle calldata for V3-style routers
/// @dev Works for Uniswap, PancakeSwap (wrapped in multicall), and Monday
function _buildBuyCalldata(
    Router router,
    uint256 amountIn,
    uint256 amountOutMin,
    uint24 fee
) internal view returns (bytes memory) {
    if (router == Router.Uniswap || router == Router.MondayTrade) {
        // Uniswap/Monday: exactInputSingle with deadline in struct (Monday) or not (Uniswap)
        // For simplicity, use Uniswap format (no deadline in struct)
        return abi.encodeWithSelector(
            bytes4(0x04e45aaf), // exactInputSingle selector
            USDC,              // tokenIn
            WMON,              // tokenOut
            fee,               // fee tier
            address(this),     // recipient
            amountIn,          // amountIn
            amountOutMin,      // amountOutMinimum
            uint160(0)         // sqrtPriceLimitX96 (0 = no limit)
        );
    } else if (router == Router.PancakeSwap) {
        // PancakeSwap: wrap in multicall(deadline, data[])
        bytes memory innerCall = abi.encodeWithSelector(
            bytes4(0x04e45aaf), // exactInputSingle selector
            USDC,
            WMON,
            fee,
            address(this),
            amountIn,
            amountOutMin,
            uint160(0)
        );
        bytes[] memory calls = new bytes[](1);
        calls[0] = innerCall;
        return abi.encodeWithSelector(
            bytes4(0x5ae401dc), // multicall(uint256,bytes[]) selector
            block.timestamp + 300, // deadline
            calls
        );
    } else if (router == Router.LFJ) {
        // LFJ: swapExactTokensForTokens with Path struct
        // Path: pairBinSteps[], versions[], tokenPath[]
        uint256[] memory binSteps = new uint256[](1);
        binSteps[0] = uint256(fee); // fee is binStep for LFJ
        uint8[] memory versions = new uint8[](1);
        versions[0] = 3; // V2_2
        address[] memory path = new address[](2);
        path[0] = USDC;
        path[1] = WMON;
        
        return abi.encodeWithSelector(
            bytes4(0x4b126ad4), // swapExactTokensForTokens selector
            amountIn,
            amountOutMin,
            binSteps,
            versions,
            path,
            address(this),
            block.timestamp + 300
        );
    }
    revert InvalidRouter();
}
```

### 1.3 Update executeArb implementation

Replace the current `executeArb` function body:

```solidity
function executeArb(
    Router sellRouter,
    bytes calldata sellRouterData,
    Router buyRouter,
    uint24 buyPoolFee,
    uint256 minWmonOut,
    uint256 minProfit
) external onlyOwner returns (int256 profit) {
    uint256 wmonBefore = IERC20(WMON).balanceOf(address(this));

    // Swap 1: WMON -> USDC on sellRouter (calldata pre-built by Rust)
    address sellAddr = _getRouterAddress(sellRouter);
    (bool success1,) = sellAddr.call(sellRouterData);
    if (!success1) revert SwapFailed(1);

    // Get ACTUAL USDC balance after swap 1
    uint256 usdcToSwap = IERC20(USDC).balanceOf(address(this));
    
    // Build swap 2 calldata ON-CHAIN using actual USDC
    bytes memory buyCalldata = _buildBuyCalldata(buyRouter, usdcToSwap, minWmonOut, buyPoolFee);
    
    // Swap 2: USDC -> WMON on buyRouter
    address buyAddr = _getRouterAddress(buyRouter);
    (bool success2,) = buyAddr.call(buyCalldata);
    if (!success2) revert SwapFailed(2);

    uint256 wmonAfter = IERC20(WMON).balanceOf(address(this));

    // Calculate profit
    profit = int256(wmonAfter) - int256(wmonBefore);

    // Revert if below minimum
    if (wmonAfter < wmonBefore + minProfit) {
        revert Unprofitable(wmonBefore, wmonAfter);
    }

    emit ArbExecuted(
        uint8(sellRouter),
        uint8(buyRouter),
        wmonBefore,
        wmonAfter,
        profit
    );
}
```

### 1.4 Update executeArbUnchecked similarly

Apply the same pattern to `executeArbUnchecked` - remove `buyRouterData`, add `buyRouter`, `buyPoolFee`, `minWmonOut`.

---

## Step 2: Modify Rust Code

**File:** `src/execution/atomic_arb.rs`

### 2.1 Remove usdc_for_swap2 calculation

**Delete these lines (~180-190):**
```rust
// For swap 2, use conservative USDC estimate
let usdc_for_swap2 = expected_usdc * 0.999; // Tiny buffer for dust
let usdc_for_swap2_wei = to_wei(usdc_for_swap2, USDC_DECIMALS);
let expected_wmon_back = usdc_for_swap2 / buy_price;
let min_wmon_out = expected_wmon_back * slippage_mult;
let min_wmon_out_wei = to_wei(min_wmon_out, WMON_DECIMALS);
```

### 2.2 Remove buy_calldata building

**Delete the buy_calldata construction (~200-210):**
```rust
let buy_calldata = build_router_calldata(
    buy_router,
    SwapDirection::Buy,
    usdc_for_swap2_wei,
    min_wmon_out_wei,
)?;
```

### 2.3 Update the Solidity call encoding

**Change the sol! macro definitions:**

```rust
sol! {
    #[derive(Debug)]
    function executeArb(
        uint8 sellRouter,
        bytes calldata sellRouterData,
        uint8 buyRouter,
        uint24 buyPoolFee,
        uint256 minWmonOut,
        uint256 minProfit
    ) external returns (int256 profit);

    #[derive(Debug)]
    function executeArbUnchecked(
        uint8 sellRouter,
        bytes calldata sellRouterData,
        uint8 buyRouter,
        uint24 buyPoolFee,
        uint256 minWmonOut
    ) external returns (int256 profit);
}
```

### 2.4 Update calldata encoding

**Calculate minWmonOut in Rust:**
```rust
// Calculate minimum WMON output (for slippage protection)
let expected_wmon_back = expected_usdc / buy_price;
let min_wmon_out = expected_wmon_back * slippage_mult;
let min_wmon_out_wei = to_wei(min_wmon_out, WMON_DECIMALS);

// Get pool fee for the buy router
let buy_pool_fee = buy_router.pool_fee; // This is u32, need to convert to u24
```

**Update the executeArb call:**
```rust
let calldata = if force {
    let execute_call = executeArbUncheckedCall {
        sellRouter: ContractRouter::from(sell_router.router_type) as u8,
        sellRouterData: sell_calldata,
        buyRouter: ContractRouter::from(buy_router.router_type) as u8,
        buyPoolFee: buy_router.pool_fee.try_into().unwrap_or(3000), // u24
        minWmonOut: min_wmon_out_wei,
    };
    Bytes::from(execute_call.abi_encode())
} else {
    let execute_call = executeArbCall {
        sellRouter: ContractRouter::from(sell_router.router_type) as u8,
        sellRouterData: sell_calldata,
        buyRouter: ContractRouter::from(buy_router.router_type) as u8,
        buyPoolFee: buy_router.pool_fee.try_into().unwrap_or(3000), // u24
        minWmonOut: min_wmon_out_wei,
        minProfit: min_profit_wei,
    };
    Bytes::from(execute_call.abi_encode())
};
```

---

## Step 3: Rebuild and Redeploy

### 3.1 Compile Solidity

```bash
cd contracts
forge build
```

### 3.2 Deploy new contract

```bash
forge create --rpc-url $MONAD_RPC_URL --private-key $PRIVATE_KEY src/MonadAtomicArb.sol:MonadAtomicArb
```

### 3.3 Update config.rs

**File:** `src/config.rs`

Update `ATOMIC_ARB_CONTRACT` with the new contract address:
```rust
pub const ATOMIC_ARB_CONTRACT: Address = alloy::primitives::address!("NEW_ADDRESS_HERE");
```

### 3.4 Setup approvals

```bash
cargo run -- contract-balance  # Verify new contract
cargo run -- fund-contract --amount 0.5  # Fund with test amount
```

---

## Step 4: Test

```bash
# Dry run first
cargo run -- atomic-arb --sell-dex uniswap --buy-dex pancakeswap1 --amount 0.1 --slippage 200 --force

# Check contract balances before and after
cargo run -- contract-balance
```

**Expected Result:**
- USDC balance should be ~0 after arb (all USDC used)
- No USDC leakage between trades

---

## Summary of Changes

| File | Change |
|------|--------|
| `contracts/src/MonadAtomicArb.sol` | New function signature, add `_buildBuyCalldata()`, query actual USDC balance |
| `src/execution/atomic_arb.rs` | Remove `usdc_for_swap2` calculation, remove `buy_calldata` building, update call encoding |
| `src/config.rs` | Update `ATOMIC_ARB_CONTRACT` after deployment |

---

## Key Points

1. **Swap 1 calldata** is still built in Rust (no change needed)
2. **Swap 2 calldata** is now built ON-CHAIN in Solidity after swap 1 completes
3. The contract uses `IERC20(USDC).balanceOf(address(this))` to get the ACTUAL USDC available
4. `minWmonOut` provides slippage protection for swap 2
5. The fix uses 100% of available USDC balance, not an estimate