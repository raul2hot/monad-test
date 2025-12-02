# Monad Arbitrage Bot MVP - Fix Instructions

## Critical Bugs to Fix

### 1. U256 Overflow Crash in `src/dex/mod.rs`

**File:** `src/dex/mod.rs`
**Line:** 49 (in `price_0_to_1()` method)

**Current broken code:**
```rust
pub fn price_0_to_1(&self) -> f64 {
    let sqrt_price = self.sqrt_price_x96.to::<u128>() as f64 / (2_f64.powi(96));
    // ...
}
```

**Problem:** `sqrt_price_x96` is a `uint160` in Solidity. Maximum value is `1.46e48` which exceeds `u128::MAX` (`3.4e38`). The `.to::<u128>()` panics on overflow.

**Fix:** Replace all price calculation methods in `Pool` struct:

```rust
impl Pool {
    /// Calculate the ADJUSTED price of token0 in terms of token1
    /// Uses U256 arithmetic throughout to prevent overflow
    pub fn price_0_to_1(&self) -> f64 {
        if self.sqrt_price_x96.is_zero() {
            return 0.0;
        }
        
        // For values that fit in u128, use simple path
        if self.sqrt_price_x96 <= U256::from(u128::MAX) {
            let sqrt_price = self.sqrt_price_x96.to::<u128>() as f64 / (2_f64.powi(96));
            let raw_price = sqrt_price * sqrt_price;
            let decimal_adjustment = 10_f64.powi(self.decimals0 as i32 - self.decimals1 as i32);
            return raw_price * decimal_adjustment;
        }
        
        // For large values, use high-precision path
        // Split into high and low parts to avoid overflow
        let sqrt_f64 = u256_to_f64_safe(self.sqrt_price_x96);
        let sqrt_price = sqrt_f64 / (2_f64.powi(96));
        let raw_price = sqrt_price * sqrt_price;
        let decimal_adjustment = 10_f64.powi(self.decimals0 as i32 - self.decimals1 as i32);
        raw_price * decimal_adjustment
    }

    pub fn price_1_to_0(&self) -> f64 {
        let price = self.price_0_to_1();
        if price > 0.0 && price.is_finite() {
            1.0 / price
        } else {
            0.0
        }
    }

    pub fn effective_price_0_to_1(&self) -> f64 {
        let fee_factor = 1.0 - (self.fee as f64 / 1_000_000.0);
        self.price_0_to_1() * fee_factor
    }

    pub fn effective_price_1_to_0(&self) -> f64 {
        let fee_factor = 1.0 - (self.fee as f64 / 1_000_000.0);
        self.price_1_to_0() * fee_factor
    }

    pub fn is_price_valid(&self) -> bool {
        let price = self.price_0_to_1();
        price > 0.0 && price.is_finite() && price < 1e18 && price > 1e-18
    }

    pub fn has_sufficient_liquidity(&self, min_liquidity: u128) -> bool {
        self.liquidity >= U256::from(min_liquidity)
    }

    pub fn liquidity_normalized(&self) -> f64 {
        u256_to_f64_safe(self.liquidity)
    }
}

/// Safely convert U256 to f64, handling values larger than u128::MAX
fn u256_to_f64_safe(value: U256) -> f64 {
    if value.is_zero() {
        return 0.0;
    }
    
    if value <= U256::from(u128::MAX) {
        return value.to::<u128>() as f64;
    }
    
    // For values > u128::MAX, use bit manipulation
    // Find the most significant 64 bits and the exponent
    let bits = 256 - value.leading_zeros();
    let shift = bits.saturating_sub(64);
    let mantissa = (value >> shift).to::<u64>() as f64;
    mantissa * 2_f64.powi(shift as i32)
}
```

---

### 2. Uniswap V4 Implementation Rewrite

**Files:** `src/dex/uniswap_v4.rs`, `src/config.rs`

**Problems:**
1. Dynamic fee pools (fee=8388608=0x800000) are included but break fee calculations
2. Pools with unknown tokens ("???") are added to the graph
3. Hook-enabled pools return unpredictable prices
4. The StateView lpFee field is not properly converted

**Fix - Complete rewrite of `src/dex/uniswap_v4.rs`:**

```rust
use alloy::{
    primitives::{aliases::I24, keccak256, Address, FixedBytes, Uint, U256},
    providers::Provider,
    rpc::types::Filter,
    sol,
    sol_types::{SolEvent, SolValue},
};
use async_trait::async_trait;

use super::{Dex, DexClient, Pool};
use crate::config::contracts::uniswap_v4::{POOL_MANAGER, STATE_VIEW};
use crate::config::tokens;

/// Dynamic fee flag - pools with this cannot be reliably quoted without V4Quoter
const DYNAMIC_FEE_FLAG: u32 = 0x800000;

/// Hook permission bits that modify swap amounts (unsafe for arbitrage)
const BEFORE_SWAP_RETURNS_DELTA: u32 = 0x08;
const AFTER_SWAP_RETURNS_DELTA: u32 = 0x04;
const UNSAFE_HOOK_FLAGS: u32 = BEFORE_SWAP_RETURNS_DELTA | AFTER_SWAP_RETURNS_DELTA;

sol! {
    #[derive(Debug)]
    struct PoolKey {
        address currency0;
        address currency1;
        uint24 fee;
        int24 tickSpacing;
        address hooks;
    }
}

sol! {
    #[sol(rpc)]
    interface IPoolManager {
        #[derive(Debug)]
        event Initialize(
            bytes32 indexed id,
            address indexed currency0,
            address indexed currency1,
            uint24 fee,
            int24 tickSpacing,
            address hooks,
            uint160 sqrtPriceX96,
            int24 tick
        );
    }
}

sol! {
    #[sol(rpc)]
    interface IStateView {
        function getSlot0(bytes32 poolId) external view returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint24 protocolFee,
            uint24 lpFee
        );
        function getLiquidity(bytes32 poolId) external view returns (uint128 liquidity);
    }
}

fn compute_pool_id(pool_key: &PoolKey) -> FixedBytes<32> {
    let encoded = pool_key.abi_encode();
    keccak256(&encoded)
}

/// Check if a V4 pool is safe for arbitrage
fn is_pool_safe_for_arb(fee: u32, hooks: Address) -> bool {
    // Reject dynamic fee pools - fee is unpredictable
    if fee & DYNAMIC_FEE_FLAG != 0 {
        return false;
    }
    
    // No hooks = safe vanilla pool
    if hooks == Address::ZERO {
        return true;
    }
    
    // Check hook permission bits in address
    let addr_bytes = hooks.as_slice();
    let low_bits = u32::from(addr_bytes[19]) | (u32::from(addr_bytes[18]) << 8);
    
    // Reject hooks that can modify swap amounts
    (low_bits & UNSAFE_HOOK_FLAGS) == 0
}

/// Check if token is in our monitored list
fn is_monitored_token(token: Address) -> bool {
    tokens::symbol(token) != "???"
}

#[derive(Debug, Clone)]
struct DiscoveredPool {
    pool_id: FixedBytes<32>,
    currency0: Address,
    currency1: Address,
    fee: u32,
    tick_spacing: i32,
    hooks: Address,
    initial_sqrt_price: u128,
}

pub struct UniswapV4Client<P> {
    provider: P,
    pool_manager: Address,
    state_view: Address,
}

impl<P: Provider + Clone> UniswapV4Client<P> {
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            pool_manager: POOL_MANAGER,
            state_view: STATE_VIEW,
        }
    }

    fn create_pool_key(
        token0: Address,
        token1: Address,
        fee: u32,
        tick_spacing: i32,
        hooks: Address,
    ) -> PoolKey {
        PoolKey {
            currency0: token0,
            currency1: token1,
            fee: Uint::<24, 1>::from(fee),
            tickSpacing: I24::try_from(tick_spacing).unwrap_or(I24::ZERO),
            hooks,
        }
    }

    async fn get_pool_state(&self, dp: &DiscoveredPool) -> eyre::Result<Option<Pool>> {
        // Skip unsafe pools
        if !is_pool_safe_for_arb(dp.fee, dp.hooks) {
            tracing::debug!(
                "Skipping unsafe V4 pool {}-{}: fee={:#x}, hooks={}",
                tokens::symbol(dp.currency0),
                tokens::symbol(dp.currency1),
                dp.fee,
                dp.hooks
            );
            return Ok(None);
        }

        // Skip pools with unknown tokens
        if !is_monitored_token(dp.currency0) || !is_monitored_token(dp.currency1) {
            tracing::debug!(
                "Skipping V4 pool with unknown token: {}-{}",
                dp.currency0,
                dp.currency1
            );
            return Ok(None);
        }

        let state_view = IStateView::new(self.state_view, &self.provider);

        let slot0 = state_view.getSlot0(dp.pool_id).call().await?;

        // Pool not initialized
        if slot0.sqrtPriceX96 == 0 {
            return Ok(None);
        }

        let liquidity: u128 = state_view.getLiquidity(dp.pool_id).call().await?;

        // Convert lpFee from Uint<24,1> to u32 - this is in hundredths of bps
        let lp_fee_raw: u32 = slot0.lpFee.to::<u32>();
        
        // For V4, the lpFee from slot0 is the actual fee to use
        // It's already in hundredths of bps (e.g., 3000 = 0.30%)
        let effective_fee = lp_fee_raw;

        Ok(Some(Pool {
            address: Address::from_slice(&dp.pool_id[12..32]),
            token0: dp.currency0,
            token1: dp.currency1,
            fee: effective_fee,
            dex: Dex::UniswapV4,
            liquidity: U256::from(liquidity),
            sqrt_price_x96: U256::from(slot0.sqrtPriceX96),
            decimals0: tokens::decimals(dp.currency0),
            decimals1: tokens::decimals(dp.currency1),
        }))
    }

    async fn discover_pools_from_events(
        &self,
        tokens: &[Address],
    ) -> eyre::Result<Vec<DiscoveredPool>> {
        use std::collections::HashSet;

        let token_set: HashSet<Address> = tokens.iter().copied().collect();
        let mut discovered = Vec::new();

        let latest_block = self.provider.get_block_number().await?;
        let start_block = latest_block.saturating_sub(50_000);

        tracing::info!(
            "V4: Scanning Initialize events from block {} to {}",
            start_block,
            latest_block
        );

        let chunk_size: u64 = 10_000;
        let mut current_from = start_block;

        while current_from < latest_block {
            let current_to = std::cmp::min(current_from + chunk_size - 1, latest_block);

            let filter = Filter::new()
                .address(self.pool_manager)
                .event_signature(IPoolManager::Initialize::SIGNATURE_HASH)
                .from_block(current_from)
                .to_block(current_to);

            if let Ok(logs) = self.provider.get_logs(&filter).await {
                for log in logs {
                    if let Ok(decoded) = log.log_decode::<IPoolManager::Initialize>() {
                        let event = decoded.inner.data;
                        let currency0 = event.currency0;
                        let currency1 = event.currency1;

                        // Only include pools where BOTH tokens are monitored
                        if token_set.contains(&currency0) && token_set.contains(&currency1) {
                            let fee: u32 = event.fee.to::<u32>();
                            let tick_spacing: i32 = event.tickSpacing.as_i32();

                            // Pre-filter unsafe pools
                            if !is_pool_safe_for_arb(fee, event.hooks) {
                                tracing::debug!(
                                    "Skipping discovered unsafe pool: {}-{} (fee={:#x}, hooks={})",
                                    tokens::symbol(currency0),
                                    tokens::symbol(currency1),
                                    fee,
                                    event.hooks
                                );
                                continue;
                            }

                            let pool_key = Self::create_pool_key(
                                currency0,
                                currency1,
                                fee,
                                tick_spacing,
                                event.hooks,
                            );
                            let pool_id = compute_pool_id(&pool_key);

                            discovered.push(DiscoveredPool {
                                pool_id,
                                currency0,
                                currency1,
                                fee,
                                tick_spacing,
                                hooks: event.hooks,
                                initial_sqrt_price: event.sqrtPriceX96.to::<u128>(),
                            });

                            tracing::debug!(
                                "Discovered safe V4 pool: {}-{} (fee={}, tickSpacing={})",
                                tokens::symbol(currency0),
                                tokens::symbol(currency1),
                                fee,
                                tick_spacing
                            );
                        }
                    }
                }
            }

            current_from = current_to + 1;
        }

        tracing::info!(
            "V4: Discovered {} safe pools with monitored tokens",
            discovered.len()
        );

        Ok(discovered)
    }
}

const MIN_LIQUIDITY: u128 = 1000 * 10u128.pow(18);

#[async_trait]
impl<P: Provider + Clone + Send + Sync> DexClient for UniswapV4Client<P> {
    async fn get_pools(&self, tokens: &[Address]) -> eyre::Result<Vec<Pool>> {
        let mut pools = Vec::new();

        let discovered = match self.discover_pools_from_events(tokens).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("V4 event discovery failed: {}", e);
                return Ok(pools);
            }
        };

        if discovered.is_empty() {
            tracing::info!("V4: No safe pools found for monitored tokens");
            return Ok(pools);
        }

        for dp in &discovered {
            match self.get_pool_state(dp).await {
                Ok(Some(pool)) => {
                    if pool.liquidity > U256::ZERO
                        && pool.is_price_valid()
                        && pool.has_sufficient_liquidity(MIN_LIQUIDITY)
                    {
                        tracing::info!(
                            "V4 pool added: {}-{} (fee={} bps, liq={}, price={:.8})",
                            tokens::symbol(pool.token0),
                            tokens::symbol(pool.token1),
                            pool.fee / 100,
                            pool.liquidity,
                            pool.price_0_to_1()
                        );
                        pools.push(pool);
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::debug!("V4 pool state error: {}", e);
                }
            }
        }

        tracing::info!("V4: {} valid pools from {} discovered", pools.len(), discovered.len());
        Ok(pools)
    }

    fn dex(&self) -> Dex {
        Dex::UniswapV4
    }
}
```

---

### 3. Fix Cycle Detection to Find More Paths

**File:** `src/graph/bellman_ford.rs`

**Problems:**
1. Only 3 paths found despite 19 pools
2. Same cycles appearing multiple times in output
3. No cross-DEX or long-chain arbitrage detected

**Fix - Improve deduplication and logging:**

In `find_all_cycles`, add detailed logging:

```rust
pub fn find_all_cycles(&self, base_tokens: &[Address]) -> Vec<ArbitrageCycle> {
    let mut all_cycles = Vec::new();
    let mut seen_signatures: HashSet<String> = HashSet::new();

    tracing::info!(
        "Searching for cycles from {} base tokens in graph with {} nodes, {} edges",
        base_tokens.len(),
        self.graph.node_count(),
        self.graph.edge_count()
    );

    for &token in base_tokens {
        let token_symbol = crate::config::tokens::symbol(token);
        let cycles = self.find_cycles_from(token);
        
        tracing::debug!(
            "Found {} raw cycles starting from {}",
            cycles.len(),
            token_symbol
        );

        for cycle in cycles {
            let signature = create_cycle_signature(&cycle);
            if !seen_signatures.contains(&signature) {
                seen_signatures.insert(signature.clone());
                
                // Log each unique cycle found
                tracing::info!(
                    "Unique cycle: {} | {} hops | {:.2}% profit | {}",
                    cycle.token_path(),
                    cycle.hop_count(),
                    cycle.profit_percentage(),
                    if cycle.is_cross_dex() { "CROSS-DEX" } else { "single-dex" }
                );
                
                all_cycles.push(cycle);
            }
        }
    }

    tracing::info!(
        "Total unique cycles found: {} (from {} base tokens)",
        all_cycles.len(),
        base_tokens.len()
    );

    // Sort by expected return (best first)
    all_cycles.sort_by(|a, b| {
        b.expected_return
            .partial_cmp(&a.expected_return)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    all_cycles
}
```

---

### 4. Expand Token List for Better Coverage

**File:** `src/config.rs`

Add more tokens to increase path diversity:

```rust
// Add to tokens_to_monitor in main.rs:
let tokens_to_monitor = vec![
    // Core tokens
    tokens::WMON,
    tokens::USDC,
    tokens::USDT,
    tokens::WETH,
    tokens::WBTC,
    // Additional stablecoins
    tokens::AUSD,
    tokens::USD1,
    tokens::LVUSD,
    // LSTs (important for MON arbitrage)
    tokens::SMON,
    tokens::GMON,
    tokens::SHMON,
    tokens::APRMON,
    // Wrapped assets
    tokens::WSTETH,
    tokens::WEETH,
    tokens::SOL,
    tokens::BTCB,
    // Meme/community
    tokens::GMONAD,
];
```

Also expand `BASE_TOKENS` to start cycles from more tokens:

```rust
pub const BASE_TOKENS: [Address; 8] = [
    WMON, USDC, USDT, WETH, WBTC, SMON, GMON, SHMON
];
```

---

### 5. Add Cross-DEX Detection Logging

**File:** `src/main.rs`

Add detailed logging after cycle detection to verify DEX diversity:

```rust
// After cycles are found, before simulation:
if !cycles.is_empty() {
    // Analyze cycle composition
    let cross_dex_count = cycles.iter().filter(|c| c.is_cross_dex()).count();
    let v3_only = cycles.iter().filter(|c| c.dexes.iter().all(|d| *d == Dex::UniswapV3)).count();
    let v4_only = cycles.iter().filter(|c| c.dexes.iter().all(|d| *d == Dex::UniswapV4)).count();
    let pancake_only = cycles.iter().filter(|c| c.dexes.iter().all(|d| *d == Dex::PancakeSwapV3)).count();
    let lfj_only = cycles.iter().filter(|c| c.dexes.iter().all(|d| *d == Dex::LFJ)).count();
    
    let hop_counts: std::collections::HashMap<usize, usize> = cycles.iter()
        .map(|c| c.hop_count())
        .fold(std::collections::HashMap::new(), |mut acc, h| {
            *acc.entry(h).or_insert(0) += 1;
            acc
        });

    info!("=== CYCLE ANALYSIS ===");
    info!("Total candidates: {}", cycles.len());
    info!("Cross-DEX: {}", cross_dex_count);
    info!("Uniswap V3 only: {}", v3_only);
    info!("Uniswap V4 only: {}", v4_only);
    info!("PancakeSwap only: {}", pancake_only);
    info!("LFJ only: {}", lfj_only);
    info!("Hop distribution: {:?}", hop_counts);
    info!("======================");

    // Log first 10 unique paths for debugging
    for (i, cycle) in cycles.iter().take(10).enumerate() {
        info!(
            "Candidate #{}: {} | DEXes: {} | Profit: {:.2}%",
            i + 1,
            cycle.token_path(),
            cycle.dex_path(),
            cycle.profit_percentage()
        );
    }
}
```

---

### 6. Fix LFJ Price Calculation

**File:** `src/dex/lfj.rs`

The `u256_to_f64` function has precision issues for Q128.128 format:

```rust
/// Convert LFJ Q128.128 price to f64 with proper precision
fn q128_to_f64(price_x128: U256) -> f64 {
    if price_x128.is_zero() {
        return 0.0;
    }
    
    // price_x128 = actual_price * 2^128
    // We need to divide by 2^128 to get actual_price
    
    // For small values that fit in u128
    if price_x128 <= U256::from(u128::MAX) {
        return price_x128.to::<u128>() as f64 / (2_f64.powi(128));
    }
    
    // For large values, shift right first to avoid precision loss
    // Get the top 64 bits and the shift amount
    let bits = 256 - price_x128.leading_zeros();
    let shift = bits.saturating_sub(64);
    let mantissa = (price_x128 >> shift).to::<u64>() as f64;
    
    // Combine: mantissa * 2^shift / 2^128 = mantissa * 2^(shift-128)
    let exponent = shift as i32 - 128;
    mantissa * 2_f64.powi(exponent)
}

// Use in get_pool_state:
let price_x128: U256 = pair_contract.getPriceFromId(active_id.try_into()?).call().await?;
let actual_price = q128_to_f64(price_x128);

// Convert to sqrtPriceX96 format for Pool struct
let sqrt_price = actual_price.sqrt();
let sqrt_price_x96 = (sqrt_price * 2_f64.powi(96)) as u128;
```

---

### 7. Add Pool Discovery Summary

**File:** `src/main.rs`

After fetching pools from each DEX, add summary:

```rust
// After all DEX pool fetching, before building graph:
info!("=== POOL DISCOVERY SUMMARY ===");
info!("Uniswap V3: {} pools", uniswap_v3_count);
info!("Uniswap V4: {} pools", uniswap_v4_count);
info!("PancakeSwap: {} pools", pancakeswap_count);
info!("LFJ: {} pools", lfj_count);
info!("Total: {} pools", total_pools);
info!("==============================");

// Log token connectivity
let mut token_pool_count: std::collections::HashMap<Address, usize> = std::collections::HashMap::new();
// ... count pools per token ...
for (token, count) in token_pool_count.iter() {
    info!("  {}: {} pools", tokens::symbol(*token), count);
}
```

---

### 8. Fix Quote Fetcher Fee Handling

**File:** `src/simulation/quote_fetcher.rs`

The fee_bps calculation is inconsistent across DEXes:

```rust
// In quote_v3_pool:
// V3 fees are in hundredths of bps (3000 = 0.30% = 30 bps)
let fee_bps = pool.fee / 100;

// In quote_lfj_pool:
// LFJ fees are calculated from actual swap, already effective
// No conversion needed - fee is absolute amount

// In quote_v4_pool:
// V4 lpFee from slot0 is in hundredths of bps
let fee_bps = pool.fee / 100;
```

Ensure all `PoolQuote.fee_bps` are in the same units (basis points).

---

## Verification Checklist

After implementing fixes, verify:

1. **No more panics**: Run with extreme price pools (WBTC/USDC, etc.)
2. **V4 pools filtered**: Only vanilla pools with static fees appear
3. **Cross-DEX detection**: See "CROSS-DEX" label in cycle analysis
4. **Long chains**: See 3-hop and 4-hop cycles in hop distribution
5. **Multiple DEXes**: See non-zero counts for V3, PancakeSwap, LFJ
6. **No duplicate paths**: Each path appears once in output

---

## Test Commands

```bash
# Run with debug logging to see all pool discovery
RUST_LOG=monad_arb_mvp=debug cargo run

# Run with trace to see every pool considered
RUST_LOG=monad_arb_mvp=trace cargo run 2>&1 | head -1000
```

---

## Expected Output After Fixes

```
=== POOL DISCOVERY SUMMARY ===
Uniswap V3: 8 pools
Uniswap V4: 2 pools (vanilla only)
PancakeSwap: 5 pools
LFJ: 4 pools
Total: 19 pools
==============================

=== CYCLE ANALYSIS ===
Total candidates: 15
Cross-DEX: 7
Uniswap V3 only: 3
Uniswap V4 only: 0
PancakeSwap only: 2
LFJ only: 3
Hop distribution: {2: 8, 3: 5, 4: 2}
======================
```
