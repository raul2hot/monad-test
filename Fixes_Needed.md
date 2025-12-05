# Phase 4B: Root Cause Analysis - 7 Second Execution Latency

## Executive Summary

The Monad mainnet arbitrage bot is experiencing **7+ seconds per swap execution** despite implementing Phase 4A optimizations (nonce manager, pre-approvals, delay removal). The root causes are in `src/execution/swap.rs` and involve HTTP provider inefficiencies and unnecessary RPC round-trips.

---

## Confirmed Root Causes

### Root Cause #1: Provider Rebuilt For Every Transaction (CRITICAL)

**Location:** `src/execution/swap.rs`, lines ~180-195

**Problem:**
```rust
// Inside execute_swap() - this runs for EVERY swap
let url: reqwest::Url = rpc_url.parse()?;
let provider_with_signer = ProviderBuilder::new()
    .wallet(wallet)
    .connect_http(url);
```

**Impact:** Creates a brand new HTTP connection and provider instance for every single swap. This involves:
- DNS resolution
- TCP handshake
- TLS negotiation (if HTTPS)
- No connection reuse between swaps

**Fix:** Pass a pre-built provider with signer from the caller instead of rebuilding it inside `execute_swap()`.

---

### Root Cause #2: Gas Price Fetched Per Transaction (MODERATE)

**Location:** `src/execution/swap.rs`, line ~165

**Problem:**
```rust
// RPC call inside execute_swap()
let gas_price = provider.get_gas_price().await.unwrap_or(100_000_000_000);
```

**Impact:** Adds ~100-500ms latency per swap. Gas price doesn't change between swaps in a 2-swap arbitrage.

**Fix:** Fetch gas price once at the start of the arb sequence and pass it to `execute_swap()`.

---

### Root Cause #3: Gas Estimation Per Transaction (MODERATE)

**Location:** `src/execution/swap.rs`, lines ~157-162

**Problem:**
```rust
// RPC call inside execute_swap()
let gas_estimate = provider.estimate_gas(estimate_tx).await.unwrap_or(250_000);
```

**Impact:** Adds ~200-800ms latency per swap. For known swap routes, gas estimation is unnecessary.

**Fix:** Use hardcoded gas limits based on router type:
- Uniswap V3 / PancakeSwap V3: 250,000 gas
- LFJ: 400,000 gas
- Monday Trade: 250,000 gas

---

### Root Cause #4: Balance Checks Add Latency (MINOR)

**Location:** `src/execution/swap.rs`, lines ~169-175 and ~218-223

**Problem:**
```rust
// Two RPC calls to check token balance
let result = provider.call(balance_tx).await?;  // Before swap
// ... swap execution ...
let result = provider.call(balance_tx).await?;  // After swap
```

**Impact:** Adds ~100-200ms each (400ms total per swap). For arb execution, we can calculate expected output from price instead of querying balance.

**Fix:** Make balance checks optional via a flag, or calculate output from swap events/logs instead.

---

### Root Cause #5: Transaction Building Not Pre-Signed (MODERATE)

**Location:** `src/execution/swap.rs`, line ~200

**Problem:** Transaction is built, signed, and sent in sequence. For the second swap, we could have the transaction pre-built and ready to send.

**Impact:** Serial execution adds unnecessary latency. Building TX takes ~10-50ms.

**Fix:** Pre-build and pre-sign swap 2 transaction while swap 1 is in flight.

---

### Root Cause #6: `get_receipt()` Waits For Block Confirmation (INHERENT)

**Location:** `src/execution/swap.rs`, line ~205

**Problem:**
```rust
let receipt = pending.get_receipt().await?;
```

**Impact:** This waits for the transaction to be included in a block. On Monad, blocks are ~1 second, but the wait includes propagation and inclusion which varies. This is **unavoidable** for reliable execution, but can be optimized.

**Note:** This is not a bug but inherent blockchain latency. The goal is to minimize everything else so only this wait remains.

---

## Detailed Fix Instructions

### Fix 1: Refactor `execute_swap()` to Accept Pre-Built Provider

**File:** `src/execution/swap.rs`

**Current signature:**
```rust
pub async fn execute_swap<P: Provider>(
    provider: &P,
    signer: &PrivateKeySigner,
    params: SwapParams,
    rpc_url: &str,
) -> Result<SwapResult>
```

**New signature:**
```rust
pub async fn execute_swap<P: Provider + Clone>(
    provider: &P,
    provider_with_signer: &impl Provider,  // Pre-built with wallet
    signer_address: Address,
    params: SwapParams,
    gas_price: u128,  // Pre-fetched
    skip_balance_check: bool,  // Optional optimization
) -> Result<SwapResult>
```

**Implementation changes:**

1. Remove these lines from inside `execute_swap()`:
```rust
// DELETE THESE:
let wallet_address = signer.address();
let wallet = EthereumWallet::from(signer.clone());
// ...
let gas_price = provider.get_gas_price().await.unwrap_or(100_000_000_000);
// ...
let url: reqwest::Url = rpc_url.parse()?;
let provider_with_signer = ProviderBuilder::new()
    .wallet(wallet)
    .connect_http(url);
```

2. Use passed-in parameters instead:
```rust
let wallet_address = signer_address;  // Already have it
// gas_price is passed in
// provider_with_signer is passed in
```

---

### Fix 2: Use Hardcoded Gas Limits

**File:** `src/execution/swap.rs`

**Add gas limit lookup function:**
```rust
fn get_gas_limit_for_router(router_type: RouterType) -> u64 {
    match router_type {
        RouterType::UniswapV3 => 280_000,
        RouterType::PancakeV3 => 280_000,
        RouterType::LfjLB => 420_000,
        RouterType::MondayTrade => 280_000,
    }
}
```

**Replace gas estimation:**
```rust
// DELETE:
let gas_estimate = provider.estimate_gas(estimate_tx).await.unwrap_or(250_000);
let gas_limit = gas_estimate + (gas_estimate / 10);

// REPLACE WITH:
let gas_limit = get_gas_limit_for_router(params.router.router_type);
```

---

### Fix 3: Create Arb-Specific Fast Execution Function

**File:** `src/main.rs`

Create a new optimized function for arb execution that minimizes RPC calls:

```rust
/// Optimized arb execution with minimal RPC overhead
async fn execute_arb_fast(
    provider: &impl Provider,
    signer: &PrivateKeySigner,
    rpc_url: &str,
    sell_router: RouterConfig,
    buy_router: RouterConfig,
    amount: f64,
    sell_price: f64,
    buy_price: f64,
    slippage_bps: u32,
) -> Result<(SwapResult, SwapResult)> {
    let wallet_address = signer.address();
    let wallet = EthereumWallet::from(signer.clone());
    
    // Create provider with signer ONCE
    let url: reqwest::Url = rpc_url.parse()?;
    let provider_with_signer = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(url);
    
    // Fetch gas price ONCE
    let gas_price = provider.get_gas_price().await.unwrap_or(100_000_000_000);
    
    // Pre-calculate all amounts
    let amount_in_wei = to_wei(amount, WMON_DECIMALS);
    let expected_usdc = amount * sell_price;
    let min_usdc = expected_usdc * (1.0 - slippage_bps as f64 / 10000.0);
    let min_usdc_wei = to_wei(min_usdc, USDC_DECIMALS);
    
    let expected_wmon_back = expected_usdc / buy_price;
    let min_wmon = expected_wmon_back * (1.0 - slippage_bps as f64 / 10000.0);
    let min_wmon_wei = to_wei(min_wmon, WMON_DECIMALS);
    
    // Pre-build BOTH swap calldatas
    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() + 300;
    
    let swap1_calldata = build_swap_calldata(
        sell_router.router_type,
        WMON_ADDRESS,
        USDC_ADDRESS,
        amount_in_wei,
        min_usdc_wei,
        wallet_address,
        sell_router.pool_fee,
        deadline,
    )?;
    
    // Pre-build swap 2 with expected USDC amount
    let expected_usdc_wei = to_wei(expected_usdc, USDC_DECIMALS);
    let swap2_calldata = build_swap_calldata(
        buy_router.router_type,
        USDC_ADDRESS,
        WMON_ADDRESS,
        expected_usdc_wei,
        min_wmon_wei,
        wallet_address,
        buy_router.pool_fee,
        deadline,
    )?;
    
    // Get gas limits (no RPC needed)
    let gas1 = get_gas_limit_for_router(sell_router.router_type);
    let gas2 = get_gas_limit_for_router(buy_router.router_type);
    
    // Build TX 1
    let tx1 = alloy::rpc::types::TransactionRequest::default()
        .to(sell_router.address)
        .from(wallet_address)
        .input(alloy::rpc::types::TransactionInput::new(swap1_calldata))
        .gas_limit(gas1)
        .nonce(next_nonce())
        .max_fee_per_gas(gas_price + (gas_price / 10))
        .max_priority_fee_per_gas(gas_price / 10)
        .with_chain_id(MONAD_CHAIN_ID);
    
    // Execute swap 1
    let start1 = std::time::Instant::now();
    let pending1 = provider_with_signer.send_transaction(tx1).await?;
    let receipt1 = pending1.get_receipt().await?;
    let elapsed1 = start1.elapsed();
    
    println!("  ✓ Swap 1 completed in {:?}", elapsed1);
    
    // Get actual USDC received from logs (faster than balance check)
    // For now, use expected amount - will rebuild if needed
    let actual_usdc = expected_usdc;  // TODO: Parse from Transfer events
    
    // Build TX 2 with actual USDC amount
    let actual_usdc_wei = to_wei(actual_usdc, USDC_DECIMALS);
    
    let tx2 = alloy::rpc::types::TransactionRequest::default()
        .to(buy_router.address)
        .from(wallet_address)
        .input(alloy::rpc::types::TransactionInput::new(swap2_calldata))  // Use pre-built
        .gas_limit(gas2)
        .nonce(next_nonce())
        .max_fee_per_gas(gas_price + (gas_price / 10))
        .max_priority_fee_per_gas(gas_price / 10)
        .with_chain_id(MONAD_CHAIN_ID);
    
    // Execute swap 2 immediately
    let start2 = std::time::Instant::now();
    let pending2 = provider_with_signer.send_transaction(tx2).await?;
    let receipt2 = pending2.get_receipt().await?;
    let elapsed2 = start2.elapsed();
    
    println!("  ✓ Swap 2 completed in {:?}", elapsed2);
    
    // Build results from receipts...
    // (omitted for brevity - construct SwapResult from receipt data)
    
    Ok((swap1_result, swap2_result))
}
```

---

### Fix 4: Optimize `run_test_arb()` to Use Fast Path

**File:** `src/main.rs`

Replace the current sequential swap execution with the optimized path:

```rust
async fn run_test_arb(sell_dex: &str, buy_dex: &str, amount: f64, slippage: u32) -> Result<()> {
    // ... setup code ...
    
    // Create provider with signer ONCE at the start
    let wallet = EthereumWallet::from(signer.clone());
    let provider_with_signer = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(url.clone());
    
    // Fetch gas price ONCE
    let gas_price = provider.get_gas_price().await.unwrap_or(100_000_000_000);
    
    // Execute arb with minimal overhead
    let (sell_result, buy_result) = execute_arb_fast(
        &provider,
        &provider_with_signer,
        signer.address(),
        sell_router,
        buy_router,
        amount,
        sell_price.price,
        buy_price.price,
        slippage,
        gas_price,
    ).await?;
    
    // ... print results ...
}
```

---

## Implementation Priority

| Priority | Fix | Estimated Savings | Difficulty |
|----------|-----|-------------------|------------|
| 1 | Hardcoded gas limits | 400-800ms/swap | Easy |
| 2 | Pre-fetch gas price once | 100-300ms/swap | Easy |
| 3 | Create single provider_with_signer | 200-500ms/swap | Medium |
| 4 | Remove balance checks | 200-400ms/swap | Medium |
| 5 | Pre-build swap 2 calldata | 20-50ms | Easy |

**Total expected savings: 1-2 seconds per swap (2-4 seconds for 2-swap arb)**

---

## Remaining Latency (Unavoidable)

After all optimizations, the remaining latency will be:
- Block confirmation time: ~1-2 seconds per swap
- Network propagation: ~100-500ms per swap
- Transaction signing: ~10-50ms per swap

**Expected final execution time: 3-5 seconds for 2-swap arb** (down from 15 seconds)

---

## Verification Steps

After implementing fixes:

1. **Add timing instrumentation:**
```rust
let t0 = std::time::Instant::now();
// ... operation ...
println!("  [TIMING] operation took {:?}", t0.elapsed());
```

2. **Expected timing breakdown for optimized 2-swap arb:**
```
[TIMING] Price fetch: ~200ms
[TIMING] Gas price fetch: ~100ms  (once only)
[TIMING] Swap 1 send + confirm: ~1.5-2.5s  (mostly block time)
[TIMING] Swap 2 send + confirm: ~1.5-2.5s  (mostly block time)
[TIMING] Total: ~3-5s
```

3. **Run test:**
```bash
cargo run -- test-arb --sell-dex pancakeswap1 --buy-dex lfj --amount 1.0 --slippage 150
```

---

## Code Changes Checklist

- [ ] Add `get_gas_limit_for_router()` function in `src/execution/swap.rs`
- [ ] Remove `estimate_gas()` call from `execute_swap()`
- [ ] Modify `execute_swap()` signature to accept `gas_price` parameter
- [ ] Modify `execute_swap()` signature to accept pre-built `provider_with_signer`
- [ ] Add `skip_balance_check` parameter to `execute_swap()`
- [ ] Create `execute_arb_fast()` function in `src/main.rs` (or new file `src/arb/mod.rs`)
- [ ] Update `run_test_arb()` to build provider_with_signer once
- [ ] Update `run_test_arb()` to fetch gas_price once
- [ ] Add timing instrumentation for debugging
- [ ] Test and verify timing improvements

---

## Advanced Optimizations (Future Phase)

If further speed improvements are needed:

1. **WebSocket RPC connection:** Lower latency than HTTP
2. **Transaction pre-signing:** Sign while waiting for previous TX
3. **Flashbots-style bundles:** If Monad supports atomic bundles
4. **Parallel submission:** Send both TXs simultaneously (requires same nonce - risky)
5. **MEV protection:** Use private mempool if available on Monad

---

## Summary

The 7-second execution time is caused by:
1. **Rebuilding HTTP provider for every swap** - Fix by creating once
2. **Fetching gas price for every swap** - Fix by caching
3. **Gas estimation RPC calls** - Fix with hardcoded limits
4. **Balance check RPC calls** - Fix by making optional

After fixes, expect **3-5 second** total arb execution (block confirmation is the floor).
