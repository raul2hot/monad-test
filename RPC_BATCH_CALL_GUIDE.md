# RPC Batch Call Implementation Guide (Alchemy/Ethereum)

> Based on production implementation achieving **80x fewer RPC calls** and **40x faster execution**
> - Before: ~240 individual RPC calls, 12+ seconds
> - After: 2-3 batched calls, ~300ms

---

## Table of Contents

1. [Overview](#overview)
2. [The Multicall3 Pattern](#the-multicall3-pattern)
3. [Implementation in Rust (Alloy)](#implementation-in-rust-alloy)
4. [Caching Strategy](#caching-strategy)
5. [Handling Different Pool Types](#handling-different-pool-types)
6. [Critical Gotchas & Nuances](#critical-gotchas--nuances)
7. [Performance Optimization Tips](#performance-optimization-tips)
8. [Error Handling Patterns](#error-handling-patterns)
9. [Rate Limiting Considerations](#rate-limiting-considerations)
10. [Complete Working Example](#complete-working-example)

---

## Overview

When building DeFi bots, MEV searchers, or any Ethereum data aggregation system, you'll quickly hit RPC rate limits and experience unacceptable latency if you make individual calls for each piece of data.

**The Problem:**
```
Fetching 50 pools × 4 calls each = 200 RPC calls
At 50ms/call = 10+ seconds per scan
Alchemy rate limits: You're toast
```

**The Solution: Multicall3**
```
50 pools × 4 calls = 200 calls batched into 2-3 RPC requests
Total time: ~300ms
Rate limit usage: Minimal
```

---

## The Multicall3 Pattern

### What is Multicall3?

Multicall3 is a smart contract deployed at the **same address on ALL EVM chains**:

```
0xcA11bde05977b3631167028862bE2a173976CA11
```

It aggregates multiple contract calls into a single RPC request, returning all results atomically.

### Supported Chains

Multicall3 is deployed on:
- Ethereum Mainnet
- Arbitrum
- Optimism
- Polygon
- BSC
- Avalanche
- Base
- And virtually all other EVM chains

### The Interface

```solidity
interface IMulticall3 {
    struct Call3 {
        address target;      // Contract to call
        bool allowFailure;   // If true, continue even if this call fails
        bytes callData;      // Encoded function call
    }
    
    struct Result {
        bool success;        // Did the call succeed?
        bytes returnData;    // ABI-encoded return data
    }
    
    function aggregate3(Call3[] calldata calls) 
        external payable 
        returns (Result[] memory returnData);
}
```

---

## Implementation in Rust (Alloy)

### Define the Interfaces

```rust
use alloy_primitives::{Address, Bytes, U256, address};
use alloy_sol_types::{sol, SolCall};

// Multicall3 interface
sol! {
    interface IMulticall3 {
        struct Call3 {
            address target;
            bool allowFailure;
            bytes callData;
        }
        
        struct Result {
            bool success;
            bytes returnData;
        }
        
        function aggregate3(Call3[] calldata calls) 
            external payable returns (Result[] memory returnData);
    }
}

// Target contract interfaces
sol! {
    interface IUniswapV3Pool {
        function slot0() external view returns (
            uint160 sqrtPriceX96, 
            int24 tick, 
            uint16 observationIndex,
            uint16 observationCardinality, 
            uint16 observationCardinalityNext,
            uint8 feeProtocol, 
            bool unlocked
        );
        function liquidity() external view returns (uint128);
        function token0() external view returns (address);
        function token1() external view returns (address);
        function fee() external view returns (uint24);
    }
    
    interface IUniswapV2Pair {
        function getReserves() external view returns (
            uint112 reserve0, 
            uint112 reserve1, 
            uint32 blockTimestampLast
        );
        function token0() external view returns (address);
        function token1() external view returns (address);
    }
}

const MULTICALL3: Address = address!("cA11bde05977b3631167028862bE2a173976CA11");
```

### Execute the Multicall

```rust
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types::TransactionRequest;
use eyre::{eyre, Result};

async fn execute_multicall(
    rpc_url: &str, 
    calls: Vec<IMulticall3::Call3>
) -> Result<Vec<IMulticall3::Result>> {
    if calls.is_empty() {
        return Ok(Vec::new());
    }
    
    let provider = ProviderBuilder::new()
        .on_http(rpc_url.parse()?);
    
    // Encode the aggregate3 call
    let calldata = IMulticall3::aggregate3Call { calls }.abi_encode();
    
    // Create the transaction request
    let tx = TransactionRequest::default()
        .to(MULTICALL3)
        .input(calldata.into());
    
    // Execute via eth_call
    let result = provider.call(tx).await
        .map_err(|e| eyre!("Multicall3 failed: {}", e))?;
    
    // Decode the results
    let decoded = IMulticall3::aggregate3Call::abi_decode_returns(&result)
        .map_err(|e| eyre!("Failed to decode multicall result: {}", e))?;
    
    Ok(decoded)
}
```

---

## Caching Strategy

### The Key Insight: Separate Static from Dynamic Data

**Static Data (Cache Forever):**
- `token0()` address
- `token1()` address
- `fee()` tier
- Token decimals

**Dynamic Data (Fetch Every Scan):**
- `slot0()` - price, tick
- `liquidity()`
- `getReserves()` - for V2 pools

### Implementation with Lazy Static + RwLock

```rust
use std::collections::HashMap;
use tokio::sync::RwLock;
use lazy_static::lazy_static;

#[derive(Debug, Clone)]
struct CachedPoolData {
    token0: Address,
    token1: Address,
    token0_decimals: u8,
    token1_decimals: u8,
    fee: u32,
}

lazy_static! {
    static ref POOL_CACHE: RwLock<HashMap<Address, CachedPoolData>> = 
        RwLock::new(HashMap::new());
}

async fn get_cached_static_data(pool_addr: Address) -> Option<CachedPoolData> {
    POOL_CACHE.read().await.get(&pool_addr).cloned()
}

async fn cache_static_data(pool_addr: Address, data: CachedPoolData) {
    POOL_CACHE.write().await.insert(pool_addr, data);
}
```

### Two-Phase Fetch Pattern

```rust
pub async fn fetch_all_pools(&self) -> Result<Vec<PoolState>> {
    let all_pool_infos = get_all_known_pools();
    
    // ============================================
    // PHASE 1: Fetch static data for UNCACHED pools only
    // ============================================
    let uncached_infos: Vec<&PoolInfo> = {
        let cache = POOL_CACHE.read().await;
        all_pool_infos.iter()
            .filter(|info| {
                let addr = Address::from_str(info.address).ok();
                addr.map(|a| !cache.contains_key(&a)).unwrap_or(false)
            })
            .collect()
    };
    
    if !uncached_infos.is_empty() {
        let new_cache_data = self.fetch_static_data_batch(&uncached_infos).await?;
        
        // Update cache
        let mut cache = POOL_CACHE.write().await;
        for (addr, data) in new_cache_data {
            cache.insert(addr, data);
        }
    }
    
    // ============================================
    // PHASE 2: Fetch dynamic data for ALL pools
    // ============================================
    let dynamic_data = self.fetch_dynamic_data_batch(&all_pool_infos).await?;
    
    // Combine and return...
}
```

---

## Handling Different Pool Types

### The Challenge

Different DEXes have different interfaces:
- **Uniswap V3**: `slot0()` + `liquidity()`
- **Uniswap V2**: `getReserves()`
- **Balancer**: Different query contract entirely
- **Curve**: `get_dy()` for accurate pricing

### Solution: Polymorphic Call Building

```rust
async fn fetch_dynamic_data_batch(
    &self,
    pool_infos: &[PoolInfo],
) -> Result<Vec<DynamicPoolData>> {
    let mut calls: Vec<IMulticall3::Call3> = Vec::new();
    let mut pool_data: Vec<(Address, &PoolInfo)> = Vec::new();
    
    for info in pool_infos {
        let addr = Address::from_str(info.address)?;
        pool_data.push((addr, info));
        
        match info.pool_type {
            PoolType::V3 => {
                // V3: slot0 + liquidity
                calls.push(IMulticall3::Call3 {
                    target: addr,
                    allowFailure: true,
                    callData: IUniswapV3Pool::slot0Call {}.abi_encode().into(),
                });
                calls.push(IMulticall3::Call3 {
                    target: addr,
                    allowFailure: true,
                    callData: IUniswapV3Pool::liquidityCall {}.abi_encode().into(),
                });
            }
            PoolType::V2 | PoolType::Balancer => {
                // V2: getReserves + placeholder
                calls.push(IMulticall3::Call3 {
                    target: addr,
                    allowFailure: true,
                    callData: IUniswapV2Pair::getReservesCall {}.abi_encode().into(),
                });
                // CRITICAL: Placeholder to maintain consistent indexing!
                calls.push(IMulticall3::Call3 {
                    target: addr,
                    allowFailure: true,
                    callData: Bytes::new(), // Empty call, will fail
                });
            }
        }
    }
    
    let results = self.execute_multicall(calls).await?;
    
    // Parse results with type-aware decoding...
}
```

---

## Critical Gotchas & Nuances

### 1. ⚠️ Placeholder Calls for Consistent Indexing

**Problem:** If V3 pools need 2 calls and V2 pools need 1, your result indexing breaks.

**Solution:** Always use the same number of calls per pool, adding placeholders:

```rust
// V2 only needs getReserves, but we add a placeholder
calls.push(IMulticall3::Call3 {
    target: addr,
    allowFailure: true,
    callData: Bytes::new(), // Will fail, that's OK
});
```

**Parsing:**
```rust
for (i, (addr, info)) in pool_data.iter().enumerate() {
    let offset = i * 2; // Always 2 calls per pool
    
    // results[offset] = first call
    // results[offset + 1] = second call (may be placeholder)
}
```

### 2. ⚠️ Always Use `allowFailure: true`

**Why?** One bad pool address shouldn't fail your entire batch.

```rust
IMulticall3::Call3 {
    target: addr,
    allowFailure: true,  // ALWAYS true for batch queries
    callData: data.into(),
}
```

**Then check success before decoding:**
```rust
let token0 = if results[offset].success {
    IUniswapV3Pool::token0Call::abi_decode_returns(&results[offset].returnData)
        .ok()
} else {
    None  // Skip this pool
};
```

### 3. ⚠️ Batch Size Limits

**Problem:** Too many calls = gas limit exceeded or RPC timeout.

**Solution:** Limit to ~100 calls per batch:

```rust
const MAX_CALLS_PER_BATCH: usize = 100;

async fn execute_large_batch(&self, all_calls: Vec<Call3>) -> Result<Vec<Result>> {
    let mut all_results = Vec::new();
    
    for chunk in all_calls.chunks(MAX_CALLS_PER_BATCH) {
        let results = self.execute_multicall(chunk.to_vec()).await?;
        all_results.extend(results);
    }
    
    Ok(all_results)
}
```

### 4. ⚠️ Fee Fallback for V2 Pools

V2 pools don't have a `fee()` function. The call will fail.

```rust
let fee = if results[offset + 2].success {
    IUniswapV3Pool::feeCall::abi_decode_returns(&results[offset + 2].returnData)
        .ok()
        .map(|f| f.to())
        .unwrap_or(info.fee)  // Fallback to known fee
} else {
    info.fee  // Use default from PoolInfo (e.g., 3000 = 0.3%)
};
```

### 5. ⚠️ Decimal Handling

Token decimals are critical for price calculation. Cache them!

```rust
pub fn get_token_decimals(address: &Address) -> u8 {
    let a = format!("{:?}", address).to_lowercase();

    // 6 decimals (stablecoins)
    if a.contains("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")  // USDC
        || a.contains("dac17f958d2ee523a2206206994597c13d831ec7")  // USDT
    {
        return 6;
    }

    // 8 decimals
    if a.contains("2260fac5e5542a773aa44fbcfedf7c193bc2c599")  // WBTC
    {
        return 8;
    }

    // Default: 18 decimals
    18
}
```

### 6. ⚠️ Price Calculation Differences

**V3 Pools (sqrtPriceX96):**
```rust
fn v3_price(sqrt_price_x96: U256, decimals0: u8, decimals1: u8) -> f64 {
    let sp = sqrt_price_x96.to::<u128>() as f64;
    if sp == 0.0 { return 0.0; }
    
    let price_raw = (sp / 2_f64.powi(96)).powi(2);
    price_raw * 10_f64.powi(decimals0 as i32 - decimals1 as i32)
}
```

**V2 Pools (reserves):**
```rust
fn v2_price(reserve0: u128, reserve1: u128, decimals0: u8, decimals1: u8) -> f64 {
    if reserve0 == 0 { return 0.0; }
    
    (reserve1 as f64 / reserve0 as f64)
        * 10_f64.powi(decimals0 as i32 - decimals1 as i32)
}
```

### 7. ⚠️ Curve Pool Pricing

Curve pools don't use simple reserve ratios. You need `get_dy()`:

```rust
sol! {
    interface ICurvePool {
        function get_dy(int128 i, int128 j, uint256 dx) 
            external view returns (uint256);
    }
}

// Query: "If I swap 1e18 of token0, how much token1 do I get?"
let call = ICurvePool::get_dyCall { 
    i: 0.into(), 
    j: 1.into(), 
    dx: U256::from(10).pow(U256::from(18)) 
}.abi_encode();
```

### 8. ⚠️ Empty Results vs Failed Calls

```rust
// Empty calldata will fail, but that's expected
if results[offset].returnData.is_empty() {
    // This was a placeholder or the contract doesn't exist
    continue;
}

// Non-empty but decode fails = ABI mismatch
match IUniswapV3Pool::slot0Call::abi_decode_returns(&results[offset].returnData) {
    Ok(slot0) => { /* use data */ }
    Err(e) => {
        // Log this - might indicate wrong ABI or contract upgrade
        warn!("Decode failed for {:?}: {}", addr, e);
    }
}
```

---

## Performance Optimization Tips

### 1. Prefetch Before Heavy Operations

If you know which pools you'll simulate, prefetch their data:

```rust
pub async fn prefetch_v2_reserves(&self, pools: &[Address]) -> Result<usize> {
    let calls: Vec<_> = pools.iter()
        .map(|addr| IMulticall3::Call3 {
            target: *addr,
            allowFailure: true,
            callData: IUniswapV2Pair::getReservesCall {}.abi_encode().into(),
        })
        .collect();

    let results = self.execute_multicall(calls).await?;
    
    // Cache results for later use
    for (addr, result) in pools.iter().zip(results.iter()) {
        if result.success {
            // Store in your reserve cache
        }
    }
    
    Ok(pools.len())
}
```

### 2. Parallel Static + Dynamic Fetching

For first-time runs where cache is empty:

```rust
use futures::future::try_join;

let (static_data, dynamic_data) = try_join(
    self.fetch_static_data_batch(&pool_infos),
    self.fetch_dynamic_data_batch(&pool_infos)
).await?;
```

### 3. Selective Refresh

Don't refetch everything if you only need prices:

```rust
// Fast path: only fetch slot0/reserves
async fn refresh_prices_only(&self, pools: &[Address]) -> Result<Vec<PriceUpdate>> {
    // Skip token0, token1, fee calls - use cache
}
```

---

## Error Handling Patterns

### Graceful Degradation

```rust
async fn fetch_with_fallback(&self, pools: &[PoolInfo]) -> Result<Vec<PoolState>> {
    // Try multicall first
    match self.execute_multicall(calls).await {
        Ok(results) => self.parse_results(results),
        Err(e) => {
            warn!("Multicall failed: {}, falling back to individual calls", e);
            self.fetch_individually(pools).await
        }
    }
}
```

### Individual Call Fallback

```rust
async fn call_individual(&self, to: Address, data: Vec<u8>) -> Result<Vec<u8>> {
    let provider = ProviderBuilder::new().on_http(self.rpc_url.parse()?);
    let tx = TransactionRequest::default().to(to).input(data.into());
    Ok(provider.call(tx).await?.to_vec())
}
```

---

## Rate Limiting Considerations

### Alchemy Limits

| Plan | CU/Second | CU/Day |
|------|-----------|--------|
| Free | 330 | 300M |
| Growth | 660 | Unlimited |

**Multicall3 cost:** ~26 CU base + minimal per-call overhead

### Best Practices

1. **Batch aggressively** - 100 calls in 1 multicall = ~30 CU vs 2600 CU individually
2. **Cache static data** - Token addresses don't change
3. **Respect backoff** - If you get 429, wait exponentially

```rust
async fn execute_with_retry(&self, calls: Vec<Call3>) -> Result<Vec<Result>> {
    let mut delay = Duration::from_millis(100);
    
    for attempt in 0..5 {
        match self.execute_multicall(calls.clone()).await {
            Ok(results) => return Ok(results),
            Err(e) if e.to_string().contains("429") => {
                warn!("Rate limited, waiting {:?}", delay);
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
            Err(e) => return Err(e),
        }
    }
    
    Err(eyre!("Max retries exceeded"))
}
```

---

## Complete Working Example

```rust
use alloy_primitives::{Address, Bytes, U256, address};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_sol_types::{sol, SolCall};
use alloy_rpc_types::TransactionRequest;
use eyre::{eyre, Result};
use std::collections::HashMap;
use tokio::sync::RwLock;

sol! {
    interface IMulticall3 {
        struct Call3 { address target; bool allowFailure; bytes callData; }
        struct Result { bool success; bytes returnData; }
        function aggregate3(Call3[] calldata calls) external payable returns (Result[] memory);
    }
    
    interface IUniswapV3Pool {
        function slot0() external view returns (uint160, int24, uint16, uint16, uint16, uint8, bool);
        function liquidity() external view returns (uint128);
        function token0() external view returns (address);
        function token1() external view returns (address);
        function fee() external view returns (uint24);
    }
}

const MULTICALL3: Address = address!("cA11bde05977b3631167028862bE2a173976CA11");
const MAX_CALLS_PER_BATCH: usize = 100;

pub struct BatchFetcher {
    rpc_url: String,
    cache: RwLock<HashMap<Address, CachedData>>,
}

#[derive(Clone)]
struct CachedData {
    token0: Address,
    token1: Address,
    fee: u32,
}

impl BatchFetcher {
    pub fn new(rpc_url: String) -> Self {
        Self { rpc_url, cache: RwLock::new(HashMap::new()) }
    }
    
    async fn execute_multicall(&self, calls: Vec<IMulticall3::Call3>) -> Result<Vec<IMulticall3::Result>> {
        if calls.is_empty() { return Ok(Vec::new()); }
        
        let provider = ProviderBuilder::new().on_http(self.rpc_url.parse()?);
        let calldata = IMulticall3::aggregate3Call { calls }.abi_encode();
        let tx = TransactionRequest::default().to(MULTICALL3).input(calldata.into());
        
        let result = provider.call(tx).await?;
        let decoded = IMulticall3::aggregate3Call::abi_decode_returns(&result)?;
        
        Ok(decoded)
    }
    
    pub async fn fetch_pools(&self, pool_addrs: &[Address]) -> Result<Vec<PoolData>> {
        let mut calls = Vec::new();
        
        for addr in pool_addrs {
            // slot0
            calls.push(IMulticall3::Call3 {
                target: *addr,
                allowFailure: true,
                callData: IUniswapV3Pool::slot0Call {}.abi_encode().into(),
            });
            // liquidity
            calls.push(IMulticall3::Call3 {
                target: *addr,
                allowFailure: true,
                callData: IUniswapV3Pool::liquidityCall {}.abi_encode().into(),
            });
        }
        
        let results = self.execute_multicall(calls).await?;
        let mut pools = Vec::new();
        
        for (i, addr) in pool_addrs.iter().enumerate() {
            let offset = i * 2;
            
            if !results[offset].success { continue; }
            
            if let Ok(slot0) = IUniswapV3Pool::slot0Call::abi_decode_returns(&results[offset].returnData) {
                let liquidity = results.get(offset + 1)
                    .filter(|r| r.success)
                    .and_then(|r| IUniswapV3Pool::liquidityCall::abi_decode_returns(&r.returnData).ok())
                    .unwrap_or(0);
                
                pools.push(PoolData {
                    address: *addr,
                    sqrt_price_x96: slot0.0,
                    tick: slot0.1,
                    liquidity,
                });
            }
        }
        
        Ok(pools)
    }
}

#[derive(Debug)]
pub struct PoolData {
    pub address: Address,
    pub sqrt_price_x96: alloy_primitives::Uint<160, 3>,
    pub tick: i32,
    pub liquidity: u128,
}
```

---

## Summary Checklist

- [ ] Use Multicall3 at `0xcA11bde05977b3631167028862bE2a173976CA11`
- [ ] Always set `allowFailure: true` for batch queries
- [ ] Limit batches to ~100 calls to avoid gas limits
- [ ] Use placeholder calls to maintain consistent result indexing
- [ ] Cache static data (tokens, fees) - only fetch once
- [ ] Fetch dynamic data (prices, liquidity) every scan
- [ ] Handle decode failures gracefully
- [ ] Implement fallback to individual calls
- [ ] Respect RPC rate limits with exponential backoff
- [ ] Test with one pool before scaling to many

---

*Generated from production MEV bot implementation. Performance: 2-3 RPC calls for 50+ pools in ~300ms.*
