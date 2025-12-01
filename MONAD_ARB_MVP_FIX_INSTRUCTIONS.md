# Monad Arbitrage MVP - Critical Bug Fixes

**Document For:** Claude Code Opus  
**Repository:** `raul2hot/monad-test`  
**Date:** December 1, 2025  
**Priority:** HIGH - Current output shows false 18-21% profits

---

## Executive Summary

The MVP is detecting arbitrage opportunities with **unrealistically high profit margins (18-21%)**. Real arbitrage opportunities in crypto markets are typically 0.1-1%. The primary cause is **missing decimal normalization** in price calculations.

### Root Causes Identified

1. **Missing Token Decimal Handling** - USDC/USDT use 6 decimals, WMON/WETH use 18 decimals
2. **No Minimum Liquidity Thresholds** - Low liquidity pools create false opportunities
3. **Price Calculation Bug** - sqrt_price_x96 not adjusted for decimal differences
4. **Missing Sanity Checks** - No validation that prices are within reasonable bounds

---

## Issue #1: Token Decimals Not Handled

### Current Code (`src/dex/mod.rs`)

```rust
impl Pool {
    /// Calculate the price of token0 in terms of token1
    pub fn price_0_to_1(&self) -> f64 {
        let sqrt_price = self.sqrt_price_x96.to::<u128>() as f64 / (2_f64.powi(96));
        sqrt_price * sqrt_price  // ❌ MISSING DECIMAL ADJUSTMENT
    }
}
```

### Problem

The Uniswap V3 `sqrtPriceX96` encodes price as:
```
sqrtPriceX96 = sqrt(price) * 2^96
price = (sqrtPriceX96 / 2^96)^2
```

But this gives you the **raw price in token units**, not accounting for decimals:
- If token0 = USDC (6 decimals) and token1 = WETH (18 decimals)
- Raw price might be `0.0000000003` when it should be `3000`

### Fix Required

**Step 1:** Add token decimals to configuration (`src/config.rs`)

```rust
pub mod tokens {
    use super::*;
    
    // Token addresses (existing)
    pub const WMON: Address = address!("3bd359C1119dA7Da1D913D1C4D2B7c461115433A");
    pub const USDC: Address = address!("754704Bc059F8C67012fEd69BC8A327a5aafb603");
    pub const USDT: Address = address!("e7cd86e13AC4309349F30B3435a9d337750fC82D");
    pub const WETH: Address = address!("EE8c0E9f1BFFb4Eb878d8f15f368A02a35481242");
    pub const WBTC: Address = address!("0555E30da8f98308EdB960aa94C0Db47230d2B9c");
    pub const SMON: Address = address!("A3227C5969757783154C60bF0bC1944180ed81B9");
    pub const GMON: Address = address!("8498312A6B3CbD158bf0c93AbdCF29E6e4F55081");

    // ADD: Token decimals mapping
    pub fn decimals(addr: Address) -> u8 {
        match addr {
            a if a == WMON => 18,
            a if a == USDC => 6,
            a if a == USDT => 6,
            a if a == WETH => 18,
            a if a == WBTC => 8,
            a if a == SMON => 18,
            a if a == GMON => 18,
            _ => 18, // Default assumption
        }
    }
    
    // Existing symbol function...
}
```

**Step 2:** Update Pool struct (`src/dex/mod.rs`)

```rust
use crate::config::tokens;

#[derive(Debug, Clone)]
pub struct Pool {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,
    pub dex: Dex,
    pub liquidity: U256,
    pub sqrt_price_x96: U256,
    // ADD: Store decimals for each token
    pub decimals0: u8,
    pub decimals1: u8,
}

impl Pool {
    /// Calculate the ADJUSTED price of token0 in terms of token1
    /// Formula: price = (sqrtPriceX96 / 2^96)^2 * 10^(decimals0 - decimals1)
    pub fn price_0_to_1(&self) -> f64 {
        let sqrt_price = self.sqrt_price_x96.to::<u128>() as f64 / (2_f64.powi(96));
        let raw_price = sqrt_price * sqrt_price;
        
        // Adjust for decimal difference
        let decimal_adjustment = 10_f64.powi(self.decimals0 as i32 - self.decimals1 as i32);
        raw_price * decimal_adjustment
    }
    
    /// Calculate the ADJUSTED price of token1 in terms of token0
    pub fn price_1_to_0(&self) -> f64 {
        let price = self.price_0_to_1();
        if price > 0.0 && price.is_finite() {
            1.0 / price
        } else {
            0.0
        }
    }
    
    /// Get effective price after fees (token0 -> token1)
    pub fn effective_price_0_to_1(&self) -> f64 {
        let fee_factor = 1.0 - (self.fee as f64 / 1_000_000.0);
        self.price_0_to_1() * fee_factor
    }
    
    /// Get effective price after fees (token1 -> token0)  
    pub fn effective_price_1_to_0(&self) -> f64 {
        let fee_factor = 1.0 - (self.fee as f64 / 1_000_000.0);
        self.price_1_to_0() * fee_factor
    }
    
    /// Validate that the price is within reasonable bounds
    pub fn is_price_valid(&self) -> bool {
        let price = self.price_0_to_1();
        price > 0.0 && price.is_finite() && price < 1e18 && price > 1e-18
    }
}
```

**Step 3:** Update DEX clients to populate decimals

In `src/dex/uniswap_v3.rs`, update `get_pool_state`:

```rust
async fn get_pool_state(
    &self,
    pool_address: Address,
    token0: Address,
    token1: Address,
    fee: u32,
) -> eyre::Result<Pool> {
    let pool_contract = IUniswapV3Pool::new(pool_address, &self.provider);
    
    let liquidity: u128 = pool_contract.liquidity().call().await?;
    let slot0 = pool_contract.slot0().call().await?;
    
    Ok(Pool {
        address: pool_address,
        token0,
        token1,
        fee,
        dex: Dex::UniswapV3,
        liquidity: U256::from(liquidity),
        sqrt_price_x96: U256::from(slot0.sqrtPriceX96),
        decimals0: tokens::decimals(token0),  // ADD
        decimals1: tokens::decimals(token1),  // ADD
    })
}
```

Do the same for `src/dex/pancakeswap.rs` and `src/dex/lfj.rs`.

---

## Issue #2: No Minimum Liquidity Filter

### Current Problem

Pools with very low liquidity (e.g., $10) can show huge price discrepancies that are:
1. Not executable at any meaningful size
2. Often just stale/abandoned pools
3. Create false positive arbitrage opportunities

### Fix Required

**Step 1:** Add liquidity threshold to config (`src/config.rs`)

```rust
pub struct Config {
    pub rpc_url: String,
    pub chain_id: u64,
    pub poll_interval_ms: u64,
    pub max_hops: usize,
    pub min_profit_bps: u32,
    // ADD: Minimum liquidity thresholds
    pub min_liquidity_usd: f64,      // e.g., $1000 minimum
    pub min_liquidity_native: u128,  // e.g., 1000 MON minimum
}

impl Config {
    pub fn from_env() -> eyre::Result<Self> {
        dotenvy::dotenv().ok();
        
        Ok(Self {
            rpc_url: env::var("ALCHEMY_RPC_URL")
                .unwrap_or_else(|_| "https://rpc.monad.xyz".to_string()),
            chain_id: 143,
            poll_interval_ms: 1000,
            max_hops: 4,
            min_profit_bps: 10,
            min_liquidity_usd: 1000.0,        // ADD: $1000 minimum
            min_liquidity_native: 1000 * 10u128.pow(18),  // ADD: 1000 MON
        })
    }
}
```

**Step 2:** Add liquidity check in Pool

```rust
impl Pool {
    /// Check if pool has sufficient liquidity
    /// Uses a simple heuristic: liquidity > threshold
    pub fn has_sufficient_liquidity(&self, min_liquidity: u128) -> bool {
        self.liquidity >= U256::from(min_liquidity)
    }
    
    /// Get liquidity as a normalized value
    pub fn liquidity_normalized(&self) -> f64 {
        // For concentrated liquidity pools, the liquidity value
        // represents virtual liquidity, not USD value
        // This is a rough approximation
        self.liquidity.to::<u128>() as f64
    }
}
```

**Step 3:** Filter in DEX clients

Update `get_pools` in each DEX client:

```rust
#[async_trait]
impl<P: Provider + Clone + Send + Sync> DexClient for UniswapV3Client<P> {
    async fn get_pools(&self, tokens: &[Address]) -> eyre::Result<Vec<Pool>> {
        let mut pools = Vec::new();
        
        for i in 0..tokens.len() {
            for j in (i + 1)..tokens.len() {
                let (token0, token1) = if tokens[i] < tokens[j] {
                    (tokens[i], tokens[j])
                } else {
                    (tokens[j], tokens[i])
                };
                
                for &fee in &self.fee_tiers {
                    if let Ok(Some(pool_addr)) = self.get_pool_address(token0, token1, fee).await {
                        match self.get_pool_state(pool_addr, token0, token1, fee).await {
                            Ok(pool) => {
                                // ADD: Multiple validity checks
                                if pool.liquidity > U256::ZERO 
                                    && pool.is_price_valid()
                                    && pool.has_sufficient_liquidity(1000 * 10u128.pow(18)) 
                                {
                                    tracing::debug!(
                                        "Found valid pool: {} (liq: {}, price: {})",
                                        pool_addr,
                                        pool.liquidity,
                                        pool.price_0_to_1()
                                    );
                                    pools.push(pool);
                                } else {
                                    tracing::trace!(
                                        "Skipping pool {} - insufficient liquidity or invalid price",
                                        pool_addr
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::debug!("Failed to get pool state for {}: {}", pool_addr, e);
                            }
                        }
                    }
                }
            }
        }
        
        Ok(pools)
    }
}
```

---

## Issue #3: Graph Edge Weight Calculation

### Current Code (`src/graph/builder.rs`)

```rust
pub fn add_pool(&mut self, pool: &Pool) {
    let node0 = self.get_or_create_node(pool.token0);
    let node1 = self.get_or_create_node(pool.token1);
    
    let liquidity = pool.liquidity.to::<u128>() as f64;
    
    // Add edge from token0 -> token1
    let price_0_to_1 = pool.effective_price_0_to_1();
    if price_0_to_1.is_finite() && price_0_to_1 > 0.0 {
        let edge_data = EdgeData::new(pool.address, pool.dex, price_0_to_1, pool.fee, liquidity);
        self.graph.add_edge(node0, node1, edge_data);
    }
    // ...
}
```

### Problem

The edge weight uses `-ln(price)` but doesn't validate that prices are reasonable. Tiny prices create very negative weights, leading to false "profitable" cycles.

### Fix Required

Add validation in `add_pool`:

```rust
pub fn add_pool(&mut self, pool: &Pool) {
    // Skip pools with invalid prices
    if !pool.is_price_valid() {
        tracing::trace!("Skipping pool {} - invalid price", pool.address);
        return;
    }
    
    let node0 = self.get_or_create_node(pool.token0);
    let node1 = self.get_or_create_node(pool.token1);
    
    let liquidity = pool.liquidity.to::<u128>() as f64;
    
    // Add edge from token0 -> token1
    let price_0_to_1 = pool.effective_price_0_to_1();
    
    // ADD: Validate price is in reasonable range (e.g., 1e-10 to 1e10)
    if price_0_to_1.is_finite() && price_0_to_1 > 1e-10 && price_0_to_1 < 1e10 {
        let edge_data = EdgeData::new(pool.address, pool.dex, price_0_to_1, pool.fee, liquidity);
        self.graph.add_edge(node0, node1, edge_data);
    } else {
        tracing::trace!(
            "Skipping edge {} -> {} - price {} out of range",
            tokens::symbol(pool.token0),
            tokens::symbol(pool.token1),
            price_0_to_1
        );
    }
    
    // Add edge from token1 -> token0
    let price_1_to_0 = pool.effective_price_1_to_0();
    if price_1_to_0.is_finite() && price_1_to_0 > 1e-10 && price_1_to_0 < 1e10 {
        let edge_data = EdgeData::new(pool.address, pool.dex, price_1_to_0, pool.fee, liquidity);
        self.graph.add_edge(node1, node0, edge_data);
    }
}
```

---

## Issue #4: Bellman-Ford Cycle Validation

### Current Code (`src/graph/bellman_ford.rs`)

The `ArbitrageCycle::is_valid()` caps returns at 1000% (10x), but this is still way too high.

### Fix Required

```rust
impl ArbitrageCycle {
    /// Validate the cycle structure and profitability
    pub fn is_valid(&self) -> bool {
        // ... existing checks ...
        
        // CHANGE: Cap at 50% to filter noise (real arb is usually < 5%)
        // Even 50% is generous - most real opportunities are < 1%
        if self.expected_return > 1.5 {
            tracing::trace!(
                "Rejecting cycle with unrealistic return: {:.2}%",
                self.profit_percentage()
            );
            return false;
        }
        
        // ADD: Minimum return check (avoid dust)
        if self.expected_return < 1.0001 {  // Less than 0.01%
            return false;
        }
        
        true
    }
    
    /// Confidence score based on various factors
    pub fn confidence_score(&self) -> f64 {
        let mut score = 1.0;
        
        // Cross-DEX is more likely to be real
        if self.is_cross_dex() {
            score *= 1.5;
        }
        
        // Lower profit is more likely to be real
        if self.profit_percentage() < 1.0 {
            score *= 1.2;
        } else if self.profit_percentage() > 5.0 {
            score *= 0.5;  // Suspicious
        }
        
        // Fewer hops is better
        if self.hop_count() <= 3 {
            score *= 1.1;
        }
        
        score
    }
}
```

---

## Issue #5: LFJ Price Calculation Bug

### Current Code (`src/dex/lfj.rs`)

```rust
// Get price from active bin (this is a Q128.128 fixed point number)
let price_x128: U256 = pair_contract
    .getPriceFromId(active_id.try_into()?)
    .call()
    .await?;

// Convert Q128.128 to sqrt_price_x96 format for compatibility
let price_u128: u128 = price_x128.to();
let sqrt_price = (price_u128 as f64).sqrt();
let sqrt_price_x96 = (sqrt_price * (1u128 << 32) as f64) as u128;
```

### Problem

1. LFJ uses Q128.128 format, not the same as Uniswap's sqrtPriceX96
2. The conversion is incorrect
3. Decimals are not handled

### Fix Required

```rust
async fn get_pool_state(
    &self,
    pair_address: Address,
    token0: Address,
    token1: Address,
    bin_step: u32,
) -> eyre::Result<Pool> {
    let pair_contract = ILBPair::new(pair_address, &self.provider);

    let reserves = pair_contract.getReserves().call().await?;
    let active_id: u32 = pair_contract.getActiveId().call().await?.to();

    // Get price from active bin (Q128.128 fixed point)
    let price_x128: U256 = pair_contract
        .getPriceFromId(active_id.try_into()?)
        .call()
        .await?;

    // FIXED: Proper conversion from Q128.128
    // price_x128 = actual_price * 2^128
    // We need to convert to sqrtPriceX96 = sqrt(actual_price) * 2^96
    
    // First get the actual price as f64
    let price_x128_f64 = price_x128.to::<u128>() as f64;
    let actual_price = price_x128_f64 / (2_f64.powi(128));
    
    // Get decimals for adjustment
    let decimals0 = tokens::decimals(token0);
    let decimals1 = tokens::decimals(token1);
    
    // Adjust for decimals (LFJ stores price without decimal adjustment)
    let decimal_adjustment = 10_f64.powi(decimals0 as i32 - decimals1 as i32);
    let adjusted_price = actual_price * decimal_adjustment;
    
    // Convert to sqrtPriceX96 format for consistency with Uniswap
    let sqrt_price = adjusted_price.sqrt();
    let sqrt_price_x96 = (sqrt_price * 2_f64.powi(96)) as u128;

    let total_liquidity = U256::from(reserves.reserveX) + U256::from(reserves.reserveY);
    let fee = bin_step * 100; // Convert to hundredths of bip

    Ok(Pool {
        address: pair_address,
        token0,
        token1,
        fee,
        dex: Dex::LFJ,
        liquidity: total_liquidity,
        sqrt_price_x96: U256::from(sqrt_price_x96),
        decimals0,
        decimals1,
    })
}
```

---

## Issue #6: Improved Logging for Debugging

### Add Debug Output

Update `src/main.rs` to add more debugging:

```rust
// After fetching pools, log price information
for pool in &pools {
    tracing::debug!(
        "Pool: {} | {}/{} | price: {:.8} | liq: {}",
        pool.address,
        tokens::symbol(pool.token0),
        tokens::symbol(pool.token1),
        pool.price_0_to_1(),
        pool.liquidity
    );
}

// When logging opportunities, add more context
for (i, cycle) in cycles.iter().take(10).enumerate() {
    // ADD: Confidence scoring
    let confidence = if cycle.profit_percentage() < 1.0 { "HIGH" }
        else if cycle.profit_percentage() < 5.0 { "MEDIUM" }
        else { "LOW - VERIFY" };
    
    println!();
    println!("--- Opportunity #{} [{}-DEX] [Confidence: {}] ---",
        i + 1,
        if cycle.is_cross_dex() { "CROSS" } else { "SINGLE" },
        confidence
    );
    // ... rest of logging
}
```

---

## Complete File Changes Summary

### Files to Modify:

1. **`src/config.rs`**
   - Add `decimals(addr: Address) -> u8` function
   - Add `min_liquidity_usd` and `min_liquidity_native` to Config

2. **`src/dex/mod.rs`**
   - Add `decimals0: u8` and `decimals1: u8` to Pool struct
   - Fix `price_0_to_1()` to include decimal adjustment
   - Add `is_price_valid()` method
   - Add `has_sufficient_liquidity()` method

3. **`src/dex/uniswap_v3.rs`**
   - Update `get_pool_state()` to populate decimals
   - Add liquidity and price validation in `get_pools()`

4. **`src/dex/pancakeswap.rs`**
   - Same changes as uniswap_v3.rs

5. **`src/dex/lfj.rs`**
   - Fix Q128.128 to sqrtPriceX96 conversion
   - Add decimal handling

6. **`src/graph/builder.rs`**
   - Add price validation in `add_pool()`
   - Add reasonable price range checks

7. **`src/graph/bellman_ford.rs`**
   - Lower max return cap from 10x to 1.5x
   - Add confidence scoring
   - Add minimum return threshold

8. **`src/main.rs`**
   - Add debug logging for prices
   - Add confidence indicators to output

---

## Testing Checklist

After implementing fixes, verify:

- [ ] WMON/USDC price is approximately $0.028 (current MON price)
- [ ] WETH/USDC price is approximately $3,600+ (current ETH price)
- [ ] No opportunities above 5% (suspicious)
- [ ] Cross-DEX opportunities appear (healthy sign)
- [ ] Low liquidity pools are filtered out
- [ ] Log shows reasonable price values

### Manual Price Verification

You can verify a pool's price on-chain:
1. Go to MonadVision explorer
2. Look up pool address (e.g., `0x8B736225c9dA7685e62CbDc6c39B69e000336F46`)
3. Check the slot0 values
4. Calculate: `price = (sqrtPriceX96 / 2^96)^2 * 10^(dec0 - dec1)`

---

## Expected Output After Fixes

```
🚀 Monad Arbitrage MVP Starting...
📡 Connecting to Monad Mainnet (Chain ID: 143)
✅ Connected! Current block: 12345678
👀 Monitoring 7 tokens
📊 Found 12 valid Uniswap V3 pools (filtered from 28)
📊 Found 8 valid PancakeSwap V3 pools (filtered from 15)
📊 Found 5 valid LFJ pools (filtered from 12)
🔗 Graph: 7 nodes, 50 edges
💰 Found 3 potential arbitrage opportunities!

--- Opportunity #1 [CROSS-DEX] [Confidence: HIGH] ---
   Path: WMON -> USDC -> WETH -> WMON
   Profit: 0.4521% (45 bps)
   Hops: 3
   DEXes: Uniswap V3 -> PancakeSwap V3 -> LFJ
   Avg Fee: 5.00 bps

--- Opportunity #2 [CROSS-DEX] [Confidence: HIGH] ---
   Path: USDC -> WMON -> WETH -> USDC
   Profit: 0.2134% (21 bps)
   Hops: 3
   DEXes: PancakeSwap V3 -> Uniswap V3 -> Uniswap V3
   Avg Fee: 3.33 bps

--- Opportunity #3 [SINGLE-DEX] [Confidence: MEDIUM] ---
   Path: WMON -> USDC -> USDT -> WMON
   Profit: 0.0823% (8 bps)
   Hops: 3
   DEXes: Uniswap V3 -> Uniswap V3 -> Uniswap V3
   Avg Fee: 1.00 bps
```

---

## Priority Order for Implementation

1. **CRITICAL**: Fix decimal handling in Pool price calculations
2. **CRITICAL**: Update all DEX clients to populate decimals
3. **HIGH**: Add liquidity filters
4. **HIGH**: Fix LFJ price conversion
5. **MEDIUM**: Add validation in graph builder
6. **MEDIUM**: Lower return caps in Bellman-Ford
7. **LOW**: Add confidence scoring and improved logging

---

*Document created: December 1, 2025*
*For Claude Code Opus implementation*
