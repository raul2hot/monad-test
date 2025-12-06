# Monad Arb Bot - Slippage Fix Instructions

## Command Being Used
```bash
cargo run -- auto-arb --min-spread-bps 40 --amount 200.0 --slippage 100 --max-executions 50 --cooldown-secs 2
```

---

## ROOT CAUSE ANALYSIS

### Evidence from Stats Files

| Metric | Run #1 (50 WMON) | Run #2 (200 WMON) |
|--------|------------------|-------------------|
| Expected Net Spread | +12 bps | +11 bps |
| **Actual Result** | **-18 bps** | **-37 bps** |
| USDC Dust Left | 0.002 | 0.022 |

**Pattern**: Larger trades = worse slippage. USDC dust left in wallet proves swap 2 isn't using actual swap 1 output.

---

## ROOT CAUSE #1: Swap 2 Uses Pre-Calculated Amount (CRITICAL)

**File**: `src/execution/fast_arb.rs` (~line 70-85)

**Problem Code**:
```rust
let safety_factor = 1.0 - (slippage_bps as f64 / 20000.0);
let usdc_for_swap2 = expected_usdc * safety_factor;  // ❌ ESTIMATED before swap 1!
```

**What's Wrong**: Swap 2's input is calculated BEFORE swap 1 executes. The bot doesn't use actual USDC received.

**Fix**: After swap 1 confirms, query actual USDC balance and use that (minus small buffer) for swap 2.

---

## ROOT CAUSE #2: "Actual" Fields Are Estimates

**File**: `src/execution/fast_arb.rs` (~line 290)

**Problem Code**:
```rust
FastArbResult {
    usdc_intermediate: usdc_for_swap2,           // ❌ This is ESTIMATED
    wmon_out: if both_success { expected_wmon_back } else { 0.0 }, // ❌ ESTIMATED
}
```

**What's Wrong**: The result struct logs estimates as "actual" values. No visibility into real execution.

**Fix**: Query WMON/USDC balances after each swap and calculate actual deltas.

---

## ROOT CAUSE #3: Swap 2 Built Before Swap 1 Executes

**File**: `src/execution/fast_arb.rs` (~line 100-130)

**Problem Code**:
```rust
// Both calldatas built BEFORE any execution
let swap1_calldata = build_fast_swap_tx(...);
let swap2_calldata = build_fast_swap_tx(  // ❌ Built with estimated USDC
    buy_router,
    SwapDirection::Buy,
    usdc_for_swap2_wei,  // ❌ This is a guess!
    ...
);
```

**What's Wrong**: Swap 2 calldata is pre-built with estimated USDC amount before knowing swap 1's actual output.

**Fix**: Build swap 2 calldata AFTER swap 1 confirms, using actual USDC balance.

---

## ROOT CAUSE #4: auto-arb Uses fast_arb Internally

**File**: `src/main.rs` (~line 870 in `run_auto_arb`)

The `auto-arb` command calls `execute_fast_arb()`, which has all the above issues.

---

## REQUIRED CHANGES

### Change 1: Query USDC Balance After Swap 1

In `src/execution/fast_arb.rs`, after swap 1 receipt is confirmed:

```rust
// AFTER swap 1 confirms, query actual USDC balance
let usdc_after_swap1 = query_usdc_balance(provider_with_signer, signer_address).await?;
let usdc_received = usdc_after_swap1 - usdc_before;

// Use actual USDC for swap 2 (with small safety buffer for rounding)
let usdc_for_swap2 = usdc_received * 0.999;  // 0.1% buffer for dust
```

### Change 2: Build Swap 2 AFTER Swap 1 Confirms

Move swap 2 calldata building to AFTER swap 1 receipt:

```rust
// Wait for swap 1 receipt FIRST
let swap1_receipt = wait_for_receipt_fast(provider_with_signer, swap1_hash).await?;

if !swap1_receipt.status() {
    // Handle swap 1 failure
    return Ok(FastArbResult { ... });
}

// NOW query actual balance and build swap 2
let usdc_after_swap1 = query_usdc_balance(provider_with_signer, signer_address).await?;
let usdc_for_swap2 = (usdc_after_swap1 - usdc_before) * 0.999;
let usdc_for_swap2_wei = to_wei(usdc_for_swap2, USDC_DECIMALS);

// NOW build swap 2 with actual amount
let min_wmon_out = (usdc_for_swap2 / buy_price) * slippage_multiplier;
let swap2_calldata = build_fast_swap_tx(
    buy_router,
    SwapDirection::Buy,
    usdc_for_swap2_wei,
    to_wei(min_wmon_out, WMON_DECIMALS),
    signer_address,
)?;
```

### Change 3: Add Balance Query Helper

Add this helper function to `src/execution/fast_arb.rs`:

```rust
/// Query USDC balance for a wallet
async fn query_usdc_balance<P: Provider>(provider: &P, wallet: Address) -> Result<f64> {
    let call = balanceOfCall { account: wallet };
    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(USDC_ADDRESS)
        .input(alloy::rpc::types::TransactionInput::new(Bytes::from(call.abi_encode())));
    let result = provider.call(tx).await?;
    let balance = U256::from_be_slice(&result);
    Ok(from_wei(balance, USDC_DECIMALS))
}

fn from_wei(amount: U256, decimals: u8) -> f64 {
    let divisor = 10u64.pow(decimals as u32) as f64;
    let amount_u128: u128 = amount.try_into().unwrap_or(0);
    amount_u128 as f64 / divisor
}
```

### Change 4: Track Actual Values in Result

Update `FastArbResult` to track actual values:

```rust
pub struct FastArbResult {
    // ... existing fields ...
    
    // Add these for actual tracking
    pub usdc_before: f64,
    pub usdc_after_swap1: f64,
    pub wmon_after_swap2: f64,
    pub actual_usdc_received: f64,  // usdc_after_swap1 - usdc_before
    pub actual_wmon_received: f64,  // wmon_after_swap2 - wmon_before
    pub swap1_slippage_bps: i32,    // (expected - actual) / expected * 10000
    pub swap2_slippage_bps: i32,
}
```

### Change 5: Re-estimate Gas for Swap 2

Since swap 2 amount changes, re-estimate gas after building new calldata:

```rust
let swap2_gas_limit = estimate_gas_with_buffer(
    provider_with_signer,
    buy_router.address,
    signer_address,
    &swap2_calldata,  // New calldata with actual USDC
    buy_router.router_type,
).await;
```

---

## EXECUTION FLOW (FIXED)

```
1. Get USDC balance BEFORE
2. Build swap 1 calldata
3. Estimate gas for swap 1
4. Send swap 1
5. Wait for swap 1 receipt
6. ✅ Query ACTUAL USDC balance
7. ✅ Calculate actual USDC received
8. ✅ Build swap 2 calldata with ACTUAL USDC
9. ✅ Estimate gas for swap 2
10. Send swap 2
11. Wait for swap 2 receipt
12. Query final WMON balance
13. Calculate actual P&L from balance deltas
```

---

## TRADEOFF NOTE

This fix adds ~100-200ms latency (balance query + gas estimate after swap 1). This is acceptable because:
- Current approach loses 30-50 bps per trade to slippage
- 200ms latency is worth it to avoid leaving USDC dust and getting wrong amounts

---

## FILES TO MODIFY

1. `src/execution/fast_arb.rs` - Main changes (all 5 changes above)
2. `src/stats.rs` - Update `PostExecutionSnapshot` to use actual values instead of estimates

---

## TESTING

After fix, run:
```bash
cargo run -- auto-arb --min-spread-bps 40 --amount 50.0 --slippage 100 --max-executions 1 --cooldown-secs 10
```

Verify in stats file:
- `usdc_delta` should be ~0 (not positive dust)
- `actual_usdc_received` should match real balance change
- Slippage should be significantly reduced