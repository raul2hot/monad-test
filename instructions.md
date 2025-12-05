# Fix: DEX-to-DEX Arbitrage Second Leg Revert Bug

## Problem Summary
The second swap in `run_test_arb` consistently fails with "Transaction reverted" because it tries to spend an **estimated** USDC amount rather than the **actual** USDC received from swap 1.

## Root Cause

### Location: `src/execution/swap.rs` lines 209-218

When `skip_balance_check=true`, the `execute_swap` function returns an **estimated** output amount based on `expected_price`, not the actual tokens received:

```rust
let amount_out = if skip_balance_check {
    // Estimate output based on expected price (no RPC call)
    let expected_out = match params.direction {
        SwapDirection::Sell => params.amount_in * params.expected_price,
        SwapDirection::Buy => params.amount_in / params.expected_price,
    };
    to_wei(expected_out, decimals_out)
} // ...
```

### Impact in `src/main.rs` `run_test_arb` function:

```rust
// Swap 1 is called with skip_balance_check=true
let sell_result = execute_swap(..., true).await?;  // skip_balance_check=true

let usdc_received = sell_result.amount_out_human;  // ← THIS IS AN ESTIMATE, NOT ACTUAL!

// Later...
let usdc_for_swap2 = usdc_received;  // Uses estimate, not actual balance!
```

If the actual USDC received differs from the estimate by even 1 wei (due to price movement, slippage, or rounding), swap 2 reverts with insufficient balance.

---

## The Fix

Query the actual USDC balance **in parallel** with the block wait. This adds **zero additional latency** since both operations run concurrently.

---

## Step 1: Add Helper Function

Add this function in `src/main.rs` (place it near the other helper functions like `to_wei` and `build_swap_calldata_only`, around line 540):

```rust
/// Query actual USDC balance for a wallet
async fn query_usdc_balance<P: Provider>(provider: &P, wallet_address: Address) -> Result<f64> {
    use alloy::sol;
    use alloy::sol_types::SolCall;
    
    sol! {
        #[derive(Debug)]
        function balanceOf(address account) external view returns (uint256);
    }
    
    let balance_call = balanceOfCall { account: wallet_address };
    let balance_tx = alloy::rpc::types::TransactionRequest::default()
        .to(USDC_ADDRESS)
        .input(alloy::rpc::types::TransactionInput::new(
            alloy::primitives::Bytes::from(balance_call.abi_encode())
        ));
    let result = provider.call(balance_tx).await?;
    let balance_wei = alloy::primitives::U256::from_be_slice(&result);
    let balance_human = (balance_wei.to::<u128>() as f64) / 1_000_000.0; // USDC has 6 decimals
    Ok(balance_human)
}
```

---

## Step 2: Modify `run_test_arb` Function

Find this section in `run_test_arb` (around lines 600-635) - the block after swap 1 succeeds:

```rust
    let usdc_received = sell_result.amount_out_human;
    println!("  ✓ Received: {:.6} USDC", usdc_received);

    // ═══════════════════════════════════════════════════════════════════
    // MONAD STATE COMMITMENT - WebSocket block subscription
    // ...
    // ═══════════════════════════════════════════════════════════════════
    let ws_url = std::env::var("MONAD_WS_URL")
        .unwrap_or_else(|_| rpc_url.replace("https://", "wss://").replace("http://", "ws://"));
    println!("  ⏳ Waiting for next block (WebSocket subscription)...");
    let t_block = std::time::Instant::now();
    match wait_for_next_block(&ws_url).await {
        Ok(block_num) => {
            println!("  ✓ Block {} confirmed in {:?}", block_num, t_block.elapsed());
        }
        Err(e) => {
            println!("  ⚠ WebSocket failed ({}), falling back to 500ms delay", e);
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }

    // Use the USDC received from swap 1 (not entire wallet balance!)
    let usdc_for_swap2 = usdc_received;
```

**Replace the entire section above with:**

```rust
    let usdc_estimated = sell_result.amount_out_human;
    println!("  ✓ Estimated received: {:.6} USDC", usdc_estimated);

    // ═══════════════════════════════════════════════════════════════════
    // MONAD STATE COMMITMENT + ACTUAL BALANCE QUERY (PARALLEL)
    // Query actual USDC balance while waiting for block confirmation.
    // This ensures we use the real balance for swap 2, not an estimate.
    // Running in parallel adds ZERO latency.
    // ═══════════════════════════════════════════════════════════════════
    let ws_url = std::env::var("MONAD_WS_URL")
        .unwrap_or_else(|_| rpc_url.replace("https://", "wss://").replace("http://", "ws://"));
    println!("  ⏳ Waiting for block + querying actual USDC balance (parallel)...");
    let t_block = std::time::Instant::now();
    
    // Run block wait and balance query in parallel
    let block_future = wait_for_next_block(&ws_url);
    let balance_future = query_usdc_balance(&provider, signer_address);
    
    let (block_result, balance_result) = tokio::join!(block_future, balance_future);
    
    // Handle block wait result
    match block_result {
        Ok(block_num) => {
            println!("  ✓ Block {} confirmed in {:?}", block_num, t_block.elapsed());
        }
        Err(e) => {
            println!("  ⚠ WebSocket failed ({}), falling back to 500ms delay", e);
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
    
    // Get actual USDC balance for swap 2
    let usdc_for_swap2 = match balance_result {
        Ok(actual_balance) => {
            if (actual_balance - usdc_estimated).abs() > 0.000001 {
                println!("  ⚠ Estimate vs Actual: {:.6} vs {:.6} (diff: {:+.6})",
                    usdc_estimated, actual_balance, actual_balance - usdc_estimated);
            }
            println!("  ✓ Using actual USDC balance: {:.6}", actual_balance);
            actual_balance
        }
        Err(e) => {
            println!("  ⚠ Balance query failed ({}), using estimate with 0.5% buffer", e);
            usdc_estimated * 0.995  // Safety buffer if query fails
        }
    };
    
    if usdc_for_swap2 < 0.000001 {
        return Err(eyre::eyre!("No USDC balance available for swap 2. Swap 1 may have failed silently."));
    }
```

---

## Step 3: Add Required Import

At the top of `src/main.rs`, ensure `tokio::join!` is available. Add this import if not present:

```rust
use tokio::join;
```

Or use the fully qualified `tokio::join!` macro as shown in the code above (which doesn't require an import).

---

## Files to Modify

| File | Change |
|------|--------|
| `src/main.rs` | Add `query_usdc_balance` helper function around line 540 |
| `src/main.rs` | Replace block wait section in `run_test_arb` with parallel version (lines 600-635) |

---

## Testing

After applying the fix, run:

```bash
cargo run -- test-arb --sell-dex pancakeswap1 --buy-dex pancakeswap2 --amount 1.0 --slippage 300
```

Expected output:
```
  ✓ Estimated received: 0.026516 USDC
  ⏳ Waiting for block + querying actual USDC balance (parallel)...
  ✓ Block 40049253 confirmed in 662.1234ms
  ✓ Using actual USDC balance: 0.026516
  
  ...
  
  ✓ Swap completed in 769.483ms
```

Both swaps should now succeed.

---

## Performance Impact

**Zero additional latency.** The balance query runs concurrently with the block wait:

```
BEFORE (broken):
  Swap 1 (1.4s) → Block Wait (662ms) → Swap 2 (850ms) = ~3s total, FAILS

AFTER (fixed):  
  Swap 1 (1.4s) → [Block Wait + Balance Query] (662ms) → Swap 2 (850ms) = ~3s total, WORKS
```

The balance query (~100ms) completes well before the block wait (~662ms), so it's effectively free.
