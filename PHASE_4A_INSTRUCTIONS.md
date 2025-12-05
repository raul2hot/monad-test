# Phase 4A: Execution Speed Optimizations

## Context

This is a Monad mainnet arbitrage bot written in Rust using the `alloy` crate. Current DEX-to-DEX arbitrage execution takes **~15 seconds** for a 2-swap round trip. Target is **sub-2 seconds**.

**Root causes of slowness:**
1. Approval transactions sent during swap execution (adds ~3s per swap)
2. 500ms artificial delay between swaps
3. Nonce fetched from RPC for every transaction
4. Sequential TX building (swap 2 waits for swap 1 to complete before building)

---

## Task 1: Pre-Approval CLI Command

### Objective
Create a new CLI command `prepare-arb` that sends MAX_UINT approval transactions to all routers for both WMON and USDC tokens. This is a one-time setup operation.

### File to Modify
`src/main.rs`

### Implementation Details

**Add new CLI command to `Commands` enum:**
```rust
/// Prepare wallet for arbitrage by approving all routers (one-time setup)
PrepareArb,
```

**Add match arm in `main()`:**
```rust
Some(Commands::PrepareArb) => {
    run_prepare_arb().await
}
```

**Create new function `run_prepare_arb()`:**

This function must:
1. Load `MONAD_RPC_URL` and `PRIVATE_KEY` from environment
2. Create provider and signer
3. For each of the 4 router addresses, send 2 approval transactions:
   - Approve router to spend WMON (MAX_UINT)
   - Approve router to spend USDC (MAX_UINT)
4. Wait for each approval to confirm before proceeding to next
5. Print summary showing which approvals succeeded/failed

**Router addresses (from `src/config.rs`):**
```rust
UNISWAP_SWAP_ROUTER: 0xfE31F71C1b106EAc32F1A19239c9a9A72ddfb900
PANCAKE_SMART_ROUTER: 0x21114915Ac6d5A2e156931e20B20b038dEd0Be7C
LFJ_LB_ROUTER: 0x18556DA13313f3532c54711497A8FedAC273220E
MONDAY_SWAP_ROUTER: 0xFE951b693A2FE54BE5148614B109E316B567632F
```

**Token addresses (from `src/config.rs`):**
```rust
WMON_ADDRESS: 0x3bd359C1119dA7Da1D913D1C4D2B7c461115433A
USDC_ADDRESS: 0x754704Bc059F8C67012fEd69BC8A327a5aafb603
```

**Use existing approval logic from `src/execution/swap.rs`** - the `approveCall` sol! macro is already defined there.

**Expected output format:**
```
══════════════════════════════════════════════════════════════
  PREPARING WALLET FOR ARBITRAGE
══════════════════════════════════════════════════════════════
Wallet: 0xad0e53732ae6ac04edc36f9f81c4bfc9aa344b2b

Approving routers for WMON...
  ✓ Uniswap SwapRouter approved (tx: 0x...)
  ✓ PancakeSwap SmartRouter approved (tx: 0x...)
  ✓ LFJ LBRouter approved (tx: 0x...)
  ✓ Monday SwapRouter approved (tx: 0x...)

Approving routers for USDC...
  ✓ Uniswap SwapRouter approved (tx: 0x...)
  ✓ PancakeSwap SmartRouter approved (tx: 0x...)
  ✓ LFJ LBRouter approved (tx: 0x...)
  ✓ Monday SwapRouter approved (tx: 0x...)

══════════════════════════════════════════════════════════════
  PREPARATION COMPLETE - 8/8 approvals successful
══════════════════════════════════════════════════════════════
```

---

## Task 2: Modify `ensure_approval()` to Skip Approval TX

### Objective
Change `ensure_approval()` in `src/execution/swap.rs` to ONLY check allowance, NEVER send approval transactions. If allowance is insufficient, return an error telling the user to run `prepare-arb`.

### File to Modify
`src/execution/swap.rs`

### Current Problematic Code (lines ~70-115)
```rust
pub async fn ensure_approval<P: Provider>(
    provider: &P,
    signer: &PrivateKeySigner,
    token: Address,
    spender: Address,
    amount: U256,
    rpc_url: &str,
) -> Result<()> {
    // ... checks allowance ...
    
    if current_allowance >= amount {
        println!("  ✓ Sufficient allowance already exists");
        return Ok(());
    }

    println!("  → Approving router to spend tokens...");
    // THIS PART MUST BE REMOVED - it sends a TX during swap execution
    // ... approval transaction code ...
}
```

### New Implementation
```rust
/// Check that router has sufficient approval. Does NOT send approval TX.
/// If approval is missing, returns error instructing user to run prepare-arb.
pub async fn check_approval<P: Provider>(
    provider: &P,
    wallet_address: Address,
    token: Address,
    spender: Address,
    amount: U256,
) -> Result<()> {
    let allowance_call = allowanceCall {
        owner: wallet_address,
        spender,
    };

    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(token)
        .input(alloy::rpc::types::TransactionInput::new(Bytes::from(allowance_call.abi_encode())));

    let result = provider.call(tx).await?;
    let current_allowance = U256::from_be_slice(&result);

    if current_allowance >= amount {
        return Ok(());
    }

    Err(eyre!(
        "Insufficient allowance for router {:?}. Run 'cargo run -- prepare-arb' first.",
        spender
    ))
}
```

### Update Call Sites in `execute_swap()`
Replace the call to `ensure_approval()` with `check_approval()`:

**Before:**
```rust
ensure_approval(provider, signer, token_in, params.router.address, amount_in, rpc_url).await?;
```

**After:**
```rust
check_approval(provider, signer.address(), token_in, params.router.address, amount_in).await?;
```

Note: The new function does NOT need `signer` or `rpc_url` parameters since it never sends transactions.

---

## Task 3: Create Local Nonce Manager

### Objective
Create a nonce manager that fetches the nonce from RPC once at startup, then increments locally for each transaction. This eliminates RPC round-trips for nonce fetching.

### Files to Create/Modify
- **Create:** `src/nonce.rs`
- **Modify:** `src/main.rs` (add module)
- **Modify:** `src/execution/swap.rs` (use nonce manager)
- **Modify:** `src/wallet/wrap.rs` (use nonce manager)

### Implementation: `src/nonce.rs`

```rust
use alloy::primitives::Address;
use alloy::providers::Provider;
use eyre::Result;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// Global nonce manager - initialized once, used for all transactions
static NONCE: OnceLock<AtomicU64> = OnceLock::new();
static WALLET_ADDRESS: OnceLock<Address> = OnceLock::new();

/// Initialize the nonce manager by fetching current nonce from RPC.
/// Must be called once at startup before any transactions.
/// Safe to call multiple times - subsequent calls are no-ops.
pub async fn init_nonce<P: Provider>(provider: &P, wallet_address: Address) -> Result<u64> {
    // Store wallet address for validation
    let _ = WALLET_ADDRESS.set(wallet_address);
    
    // If already initialized, return current value
    if let Some(nonce) = NONCE.get() {
        return Ok(nonce.load(Ordering::SeqCst));
    }
    
    // Fetch from RPC
    let nonce = provider.get_transaction_count(wallet_address).await?;
    
    // Initialize atomic counter
    let _ = NONCE.set(AtomicU64::new(nonce));
    
    Ok(nonce)
}

/// Get the next nonce and increment the counter atomically.
/// Panics if init_nonce() was not called first.
pub fn next_nonce() -> u64 {
    NONCE
        .get()
        .expect("Nonce manager not initialized. Call init_nonce() first.")
        .fetch_add(1, Ordering::SeqCst)
}

/// Get current nonce without incrementing (for debugging/display).
pub fn current_nonce() -> u64 {
    NONCE
        .get()
        .expect("Nonce manager not initialized. Call init_nonce() first.")
        .load(Ordering::SeqCst)
}

/// Reset nonce by re-fetching from RPC. Use if transaction failed.
pub async fn reset_nonce<P: Provider>(provider: &P) -> Result<u64> {
    let wallet_address = *WALLET_ADDRESS
        .get()
        .expect("Nonce manager not initialized.");
    
    let nonce = provider.get_transaction_count(wallet_address).await?;
    
    if let Some(atomic_nonce) = NONCE.get() {
        atomic_nonce.store(nonce, Ordering::SeqCst);
    }
    
    Ok(nonce)
}
```

### Modify `src/main.rs`

**Add module declaration near top:**
```rust
mod nonce;
```

**Add import:**
```rust
use nonce::init_nonce;
```

**Initialize nonce in functions that use transactions:**

In `run_test_arb()`, `run_test_swap()`, `run_wrap()`, `run_unwrap()`, `run_buy_mon()`, `run_sell_mon()`, and `run_prepare_arb()` - add nonce initialization after creating provider and signer:

```rust
// After: let signer = PrivateKeySigner::from_str(&private_key)?;
// Add:
init_nonce(&provider, signer.address()).await?;
```

### Modify `src/execution/swap.rs`

**Add import at top:**
```rust
use crate::nonce::next_nonce;
```

**Update `execute_swap()` function:**

Find where the transaction is built (around line 180):
```rust
let tx = alloy::rpc::types::TransactionRequest::default()
    .to(params.router.address)
    .input(alloy::rpc::types::TransactionInput::new(calldata))
    .gas_limit(gas_limit);
```

**Change to:**
```rust
let tx = alloy::rpc::types::TransactionRequest::default()
    .to(params.router.address)
    .input(alloy::rpc::types::TransactionInput::new(calldata))
    .gas_limit(gas_limit)
    .nonce(next_nonce());
```

### Modify `src/wallet/wrap.rs`

**Add import at top:**
```rust
use crate::nonce::next_nonce;
```

**Update `wrap_mon()` function** - find the transaction build:
```rust
let tx = alloy::rpc::types::TransactionRequest::default()
    .to(WMON_ADDRESS)
    .value(amount_wei)
    .input(...)
    .gas_limit(60_000);
```

**Change to:**
```rust
let tx = alloy::rpc::types::TransactionRequest::default()
    .to(WMON_ADDRESS)
    .value(amount_wei)
    .input(...)
    .gas_limit(60_000)
    .nonce(next_nonce());
```

**Update `unwrap_wmon()` function** similarly - add `.nonce(next_nonce())` to the transaction builder.

---

## Task 4: Remove 500ms Inter-Swap Delay

### Objective
Remove the artificial 500ms delay between swaps in the `test-arb` command.

### File to Modify
`src/main.rs`

### Location
In function `run_test_arb()`, find this line (approximately line 520):
```rust
// Small delay to ensure state is updated
tokio::time::sleep(Duration::from_millis(500)).await;
```

### Action
**Delete these 2 lines entirely.** The delay is not needed - Monad's state is consistent immediately after transaction confirmation.

---

## Task 5: Optimized Arb Execution (Parallel TX Building)

### Objective
Create a new optimized arb execution function that builds swap 2's calldata WHILE swap 1 is in flight, then submits swap 2 immediately after swap 1 confirms.

### File to Modify
`src/main.rs`

### Implementation Strategy

The current flow is:
1. Build swap 1 calldata
2. Send swap 1
3. Wait for swap 1 receipt
4. Get USDC balance (to know amount for swap 2)
5. Build swap 2 calldata
6. Send swap 2
7. Wait for swap 2 receipt

The optimized flow:
1. Build swap 1 calldata
2. Build swap 2 calldata template (with estimated USDC output)
3. Send swap 1
4. While waiting for swap 1: calldata for swap 2 is already ready
5. Get swap 1 receipt
6. If actual USDC differs significantly from estimate, rebuild swap 2 calldata
7. Send swap 2 immediately
8. Wait for swap 2 receipt

### Code Changes in `run_test_arb()`

**Add helper function to pre-build swap calldata:**
```rust
fn build_swap_calldata_only(
    router: &RouterConfig,
    direction: SwapDirection,
    amount_in: U256,
    amount_out_min: U256,
    recipient: Address,
) -> Result<Bytes> {
    let (token_in, token_out) = match direction {
        SwapDirection::Sell => (WMON_ADDRESS, USDC_ADDRESS),
        SwapDirection::Buy => (USDC_ADDRESS, WMON_ADDRESS),
    };
    
    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() + 300;
    
    build_swap_calldata(
        router.router_type,
        token_in,
        token_out,
        amount_in,
        amount_out_min,
        recipient,
        router.pool_fee,
        deadline,
    )
}
```

**Modify the swap execution section to pre-build swap 2:**

After getting prices and before executing swap 1:
```rust
// Pre-calculate expected USDC output from swap 1
let expected_usdc = amount * sell_price.price;
let expected_usdc_wei = to_wei(expected_usdc, USDC_DECIMALS);

// Pre-calculate expected WMON output from swap 2 (for slippage)
let expected_wmon_back = expected_usdc / buy_price.price;
let slippage_multiplier = 1.0 - (slippage as f64 / 10000.0);
let min_wmon_out = expected_wmon_back * slippage_multiplier;
let min_wmon_out_wei = to_wei(min_wmon_out, WMON_DECIMALS);

// Pre-build swap 2 calldata (will be ready when swap 1 completes)
let swap2_calldata = build_swap_calldata_only(
    &buy_router,
    SwapDirection::Buy,
    expected_usdc_wei,  // Will adjust if actual differs by >5%
    min_wmon_out_wei,
    signer.address(),
)?;
```

---

## Testing Procedure

After implementing all changes:

### 1. Build the project
```bash
cargo build --release
```

### 2. Run prepare-arb (one-time)
```bash
cargo run -- prepare-arb
```

### 3. Test execution speed with test-arb
```bash
# Before optimization baseline (if you haven't already):
# Total time was ~15 seconds

# After optimization:
cargo run -- test-arb --sell-dex lfj --buy-dex pancakeswap1 --amount 1.0 --slippage 150
```

### Expected Results
- `prepare-arb` should complete 8 approvals in ~30-60 seconds
- `test-arb` should complete in **2-4 seconds** (down from 15 seconds)
- No approval transactions should occur during `test-arb` execution
- Console should show nonce being used (no "fetching nonce" delays)

---

## File Summary

| File | Action | Changes |
|------|--------|---------|
| `src/main.rs` | Modify | Add `PrepareArb` command, add `mod nonce`, init nonce in TX functions, remove 500ms delay |
| `src/nonce.rs` | Create | New file with nonce manager |
| `src/execution/swap.rs` | Modify | Replace `ensure_approval` with `check_approval`, add nonce to TX |
| `src/wallet/wrap.rs` | Modify | Add nonce to wrap/unwrap TXs |
| `src/execution/mod.rs` | No change | - |
| `src/config.rs` | No change | Router addresses already defined |

---

## Important Notes for Implementation

1. **Do NOT modify router addresses or token addresses** - they are already correct in `src/config.rs`

2. **The `sol!` macro for `approveCall` already exists** in `src/execution/swap.rs` - reuse it

3. **Use `U256::MAX` for approval amounts** - this is standard practice to avoid re-approvals

4. **Nonce manager uses `OnceLock` + `AtomicU64`** - this is thread-safe and allows the nonce to be initialized once and used across async contexts

5. **If a transaction fails**, the nonce manager's `reset_nonce()` function should be called to re-sync with the chain

6. **The parallel TX building in Task 5 is an optimization** - implement Tasks 1-4 first, verify they work, then implement Task 5

---

## Success Criteria

✅ `cargo run -- prepare-arb` approves all routers for WMON and USDC  
✅ `cargo run -- test-arb` completes without sending any approval TXs  
✅ `cargo run -- test-arb` completes in under 5 seconds  
✅ No `tokio::time::sleep` calls remain in the arb execution path  
✅ Nonce is fetched from RPC exactly once per command invocation  
