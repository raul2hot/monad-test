# Phase 4B: CRITICAL FIX - Alloy Receipt Polling Interval

## Root Cause Identified

The **7-second delay** is caused by **alloy's default receipt polling interval** for HTTP providers.

When you call:
```rust
let receipt = pending.get_receipt().await?;
```

Alloy uses a default polling interval of **7 seconds** to check if the transaction has been mined. This is why each swap takes exactly ~7 seconds regardless of other optimizations.

---

## FIX 1: Custom Fast Receipt Polling (CRITICAL)

**File:** `src/execution/swap.rs`

### Step 1: Add Required Imports

At the top of the file, add:
```rust
use std::time::Duration;
use tokio::time::{interval, timeout};
use alloy::primitives::TxHash;
use alloy::rpc::types::TransactionReceipt;
```

### Step 2: Add Fast Polling Helper Function

Add this function before `execute_swap()`:

```rust
/// Wait for transaction receipt with fast polling (100ms interval)
/// Times out after 30 seconds
async fn wait_for_receipt_fast<P: Provider>(
    provider: &P,
    tx_hash: TxHash,
) -> Result<TransactionReceipt> {
    let mut poll_interval = interval(Duration::from_millis(100));
    let deadline = Duration::from_secs(30);
    
    timeout(deadline, async {
        loop {
            poll_interval.tick().await;
            if let Some(receipt) = provider.get_transaction_receipt(tx_hash).await? {
                return Ok::<_, eyre::Report>(receipt);
            }
        }
    })
    .await
    .map_err(|_| eyre::eyre!("Transaction confirmation timeout after 30s"))?
}
```

### Step 3: Replace Receipt Waiting in `execute_swap()`

Find this code (around line 200-210):
```rust
let send_result = provider_with_signer.send_transaction(tx).await;

match send_result {
    Ok(pending) => {
        let receipt = pending.get_receipt().await?;
        let elapsed = start.elapsed();
```

**Replace with:**
```rust
let send_result = provider_with_signer.send_transaction(tx).await;

match send_result {
    Ok(pending) => {
        let tx_hash = *pending.tx_hash();
        
        // CRITICAL: Use fast 100ms polling instead of default 7-second interval!
        let receipt = wait_for_receipt_fast(provider, tx_hash).await?;
        let elapsed = start.elapsed();
```

---

## FIX 2: LFJ Swap Reverting

The LFJ swap is reverting separately from the timing issue. There are several possible causes:

### Cause 2A: Wrong Bin Step

**File:** `src/config.rs`, line ~86

Current config:
```rust
pub fn get_lfj_pool() -> PoolConfig {
    PoolConfig {
        name: "LFJ",
        address: alloy::primitives::address!("5e60bc3f7a7303bc4dfe4dc2220bdc90bc04fe22"),
        pool_type: PoolType::LiquidityBook,
        fee_bps: 10,  // <-- This is used as bin_step!
    }
}
```

And in `get_routers()`:
```rust
RouterConfig {
    name: "LFJ",
    address: LFJ_LB_ROUTER,
    router_type: RouterType::LfjLB,
    pool_address: alloy::primitives::address!("5e60bc3f7a7303bc4dfe4dc2220bdc90bc04fe22"),
    pool_fee: 10,  // <-- Used as bin_step in LFJ router
},
```

**Verification needed:** Call the pool contract to verify actual bin step:
- Pool address: `0x5e60bc3f7a7303bc4dfe4dc2220bdc90bc04fe22`
- Call `getBinStep()` - should return the actual bin step

**The bin step might be 15, 20, or 25 instead of 10!**

### Cause 2B: Wrong LFJ Version

**File:** `src/execution/routers/lfj.rs`, line ~82

Current code uses version 3:
```rust
let path = Path {
    pairBinSteps: vec![U256::from(bin_step)],
    versions: vec![3],  // V2_2 = 3
    tokenPath: vec![token_in, token_out],
};
```

Verify that the pool is actually V2_2 (version 3). If it's V2_1, use version 2.

### Cause 2C: Insufficient USDC Approval

Even though `prepare-arb` ran, verify USDC is approved for LFJ router.

Add debug output before the swap:
```rust
// In run_test_arb() before swap 2
println!("DEBUG: Checking USDC approval for LFJ...");
let allowance_call = allowanceCall {
    owner: signer.address(),
    spender: LFJ_LB_ROUTER,
};
let tx = alloy::rpc::types::TransactionRequest::default()
    .to(USDC_ADDRESS)
    .input(alloy::rpc::types::TransactionInput::new(Bytes::from(allowance_call.abi_encode())));
let result = provider.call(tx).await?;
let allowance = U256::from_be_slice(&result);
println!("DEBUG: USDC allowance for LFJ: {}", allowance);
```

### Cause 2D: Token Order Issue

**File:** `src/execution/routers/lfj.rs`

For LFJ's `swapExactTokensForTokens`, the `tokenPath` must be in correct order.

For USDC → WMON (buy direction):
```rust
tokenPath: vec![USDC_ADDRESS, WMON_ADDRESS]  // token_in, token_out
```

Verify this is happening correctly in `build_swap_exact_tokens_for_tokens()`.

---

## Quick Test Commands

### Test 1: Verify the fix works with a different DEX pair (no LFJ)
```bash
cargo run -- test-arb --sell-dex pancakeswap1 --buy-dex uniswap --amount 1.0 --slippage 150
```

If this completes in ~2-4 seconds instead of 15s, the polling fix worked!

### Test 2: Test LFJ alone
```bash
cargo run -- test-swap --dex lfj --amount 1.0 --direction buy
```

This will help isolate if LFJ is the specific problem.

### Test 3: Check actual bin step
Add this to your code to query the bin step:
```rust
// In run_test_arb() or a new debug command
let bin_step_call = getBinStepCall {};
let tx = alloy::rpc::types::TransactionRequest::default()
    .to(alloy::primitives::address!("5e60bc3f7a7303bc4dfe4dc2220bdc90bc04fe22"))
    .input(alloy::rpc::types::TransactionInput::new(Bytes::from(bin_step_call.abi_encode())));
let result = provider.call(tx).await?;
let bin_step = u16::from_be_bytes([result[30], result[31]]);
println!("Actual LFJ bin step: {}", bin_step);
```

---

## Expected Results After Fix

### Timing:
```
[TIMING] Gas price fetch: ~60ms
[TIMING] Price fetch: ~60ms  
[TIMING] Swap 1 execution: ~1-2s  (was 7.4s!)
[TIMING] Swap 2 execution: ~1-2s  (was 7.2s!)
[TIMING] Total: ~2-4s  (was 15s!)
```

### Success criteria:
- ✅ Each swap completes in 1-2 seconds (not 7 seconds)
- ✅ Total arb execution under 5 seconds
- ✅ LFJ swaps don't revert (after bin step fix)

---

## Summary

| Issue | Root Cause | Fix |
|-------|------------|-----|
| **7s per swap** | Alloy's default 7s polling interval in `get_receipt()` | Use custom `wait_for_receipt_fast()` with 100ms polling |
| **LFJ revert** | Wrong bin step (10 vs actual), or approval issue | Verify bin step from contract, check approvals |

**The 7-second delay has NOTHING to do with gas estimation, provider creation, or nonce management** - it's purely alloy's default HTTP polling interval for transaction receipts!

---

## Code Changes Checklist

- [ ] Add `use std::time::Duration;` to swap.rs imports
- [ ] Add `use tokio::time::{interval, timeout};` to swap.rs imports  
- [ ] Add `wait_for_receipt_fast()` helper function
- [ ] Replace `pending.get_receipt().await` with `wait_for_receipt_fast(provider, tx_hash).await`
- [ ] Test with non-LFJ pair first to verify timing fix
- [ ] Query actual LFJ bin step from contract
- [ ] Update config if bin step is wrong
- [ ] Test LFJ swaps after config fix
