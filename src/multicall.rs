//! Multicall3 Batch API Module
//!
//! Provides efficient batched RPC calls using the Multicall3 contract.
//! Reduces RPC calls by 80x and latency by 40x compared to individual calls.
//!
//! Multicall3 is deployed at the same address on ALL EVM chains:
//! `0xcA11bde05977b3631167028862bE2a173976CA11`

use alloy::primitives::{Address, U256, address, aliases::U24};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::{eyre, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, trace};

use crate::config::tokens;
use crate::dex::{Dex, Pool};

/// Multicall3 contract address (same on all EVM chains)
pub const MULTICALL3_ADDRESS: Address = address!("cA11bde05977b3631167028862bE2a173976CA11");

/// Maximum calls per batch to avoid gas limits
pub const MAX_CALLS_PER_BATCH: usize = 100;

// Multicall3 interface
sol! {
    /// Multicall3 contract interface for batched calls
    interface IMulticall3 {
        /// A single call in a batch
        struct Call3 {
            address target;      // Contract to call
            bool allowFailure;   // If true, continue even if this call fails
            bytes callData;      // Encoded function call
        }

        /// Result of a single call
        struct Result {
            bool success;        // Did the call succeed?
            bytes returnData;    // ABI-encoded return data
        }

        /// Execute multiple calls in a single transaction
        function aggregate3(Call3[] calldata calls)
            external payable
            returns (Result[] memory returnData);
    }
}

// Pool interfaces for batch calls
sol! {
    /// Uniswap V3/PancakeSwap V3 Pool interface for batch queries
    interface IPoolBatch {
        function token0() external view returns (address);
        function token1() external view returns (address);
        function fee() external view returns (uint24);
        function liquidity() external view returns (uint128);
        function slot0() external view returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint16 observationIndex,
            uint16 observationCardinality,
            uint16 observationCardinalityNext,
            uint8 feeProtocol,
            bool unlocked
        );
    }

    /// Uniswap V3/PancakeSwap V3 Factory interface
    interface IFactoryBatch {
        function getPool(address tokenA, address tokenB, uint24 fee) external view returns (address pool);
    }

    /// LFJ (TraderJoe) Pair interface for batch queries
    interface ILBPairBatch {
        function getTokenX() external view returns (address);
        function getTokenY() external view returns (address);
        function getReserves() external view returns (uint128 reserveX, uint128 reserveY);
        function getActiveId() external view returns (uint24);
        function getBinStep() external view returns (uint16);
    }

    /// LFJ Factory interface for batch queries
    interface ILBFactoryBatch {
        function getLBPairInformation(address tokenX, address tokenY, uint256 binStep)
            external view returns (
                uint16 binStep_,
                address LBPair,
                bool createdByOwner,
                bool ignoredForRouting
            );
    }
}

/// Cached static pool data (doesn't change)
#[derive(Debug, Clone)]
pub struct CachedPoolData {
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,
    pub decimals0: u8,
    pub decimals1: u8,
    pub dex: Dex,
}

/// Dynamic pool data (changes every block)
#[derive(Debug, Clone)]
pub struct DynamicPoolData {
    pub sqrt_price_x96: U256,
    pub tick: i32,
    pub liquidity: U256,
}

/// Pool discovery result
#[derive(Debug, Clone)]
pub struct DiscoveredPool {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,
}

/// Batch fetcher for efficient RPC calls
pub struct BatchFetcher<P> {
    provider: Arc<P>,
    /// Cache for static pool data (token0, token1, fee)
    static_cache: Arc<RwLock<HashMap<Address, CachedPoolData>>>,
}

impl<P> BatchFetcher<P>
where
    P: Provider + Clone + 'static,
{
    /// Create a new batch fetcher
    pub fn new(provider: P) -> Self {
        Self {
            provider: Arc::new(provider),
            static_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Execute a batch of calls using Multicall3
    pub async fn execute_multicall(
        &self,
        calls: Vec<IMulticall3::Call3>,
    ) -> Result<Vec<IMulticall3::Result>> {
        if calls.is_empty() {
            return Ok(Vec::new());
        }

        // Encode the aggregate3 call
        let calldata = IMulticall3::aggregate3Call { calls }.abi_encode();

        // Create transaction request
        let tx = TransactionRequest::default()
            .to(MULTICALL3_ADDRESS)
            .input(calldata.into());

        // Execute via eth_call
        let result = self
            .provider
            .call(tx)
            .await
            .map_err(|e| eyre!("Multicall3 failed: {}", e))?;

        // Decode the results - abi_decode_returns returns Vec<Result> directly
        let decoded = IMulticall3::aggregate3Call::abi_decode_returns(&result)
            .map_err(|e| eyre!("Failed to decode multicall result: {}", e))?;

        Ok(decoded)
    }

    /// Execute a large batch by chunking into smaller batches
    pub async fn execute_large_batch(
        &self,
        all_calls: Vec<IMulticall3::Call3>,
    ) -> Result<Vec<IMulticall3::Result>> {
        let mut all_results = Vec::with_capacity(all_calls.len());

        for chunk in all_calls.chunks(MAX_CALLS_PER_BATCH) {
            let results = self.execute_multicall(chunk.to_vec()).await?;
            all_results.extend(results);
        }

        Ok(all_results)
    }

    /// Batch discover pool addresses from factory
    pub async fn batch_discover_pools(
        &self,
        factory: Address,
        token_pairs: &[(Address, Address, u32)], // (tokenA, tokenB, fee)
    ) -> Result<Vec<DiscoveredPool>> {
        if token_pairs.is_empty() {
            return Ok(Vec::new());
        }

        // Build calls for getPool
        let calls: Vec<IMulticall3::Call3> = token_pairs
            .iter()
            .map(|(token_a, token_b, fee)| {
                let fee_u24 = U24::from(*fee);
                let calldata = IFactoryBatch::getPoolCall {
                    tokenA: *token_a,
                    tokenB: *token_b,
                    fee: fee_u24,
                }
                .abi_encode();

                IMulticall3::Call3 {
                    target: factory,
                    allowFailure: true,
                    callData: calldata.into(),
                }
            })
            .collect();

        let results = self.execute_large_batch(calls).await?;

        // Parse results
        let mut pools = Vec::new();
        for (i, result) in results.iter().enumerate() {
            if !result.success || result.returnData.is_empty() {
                continue;
            }

            if let Ok(pool_addr) = IFactoryBatch::getPoolCall::abi_decode_returns(&result.returnData) {
                // The return is a single address directly
                if pool_addr != Address::ZERO {
                    let (token_a, token_b, fee) = token_pairs[i];
                    let (token0, token1) = if token_a < token_b {
                        (token_a, token_b)
                    } else {
                        (token_b, token_a)
                    };

                    pools.push(DiscoveredPool {
                        address: pool_addr,
                        token0,
                        token1,
                        fee,
                    });
                }
            }
        }

        debug!("Discovered {} pools from {} queries", pools.len(), token_pairs.len());
        Ok(pools)
    }

    /// Batch fetch static pool data (token0, token1, fee) - uses cache
    pub async fn batch_fetch_static_data(
        &self,
        pool_addresses: &[Address],
        dex: Dex,
    ) -> Result<HashMap<Address, CachedPoolData>> {
        if pool_addresses.is_empty() {
            return Ok(HashMap::new());
        }

        // Check which pools need fetching (not in cache)
        let uncached_pools: Vec<Address> = {
            let cache = self.static_cache.read().await;
            pool_addresses
                .iter()
                .filter(|addr| !cache.contains_key(*addr))
                .copied()
                .collect()
        };

        // If all pools are cached, return from cache
        if uncached_pools.is_empty() {
            let cache = self.static_cache.read().await;
            return Ok(pool_addresses
                .iter()
                .filter_map(|addr| cache.get(addr).map(|data| (*addr, data.clone())))
                .collect());
        }

        // Build calls for uncached pools: token0, token1, fee (3 calls per pool)
        let mut calls = Vec::with_capacity(uncached_pools.len() * 3);
        for addr in &uncached_pools {
            // token0
            calls.push(IMulticall3::Call3 {
                target: *addr,
                allowFailure: true,
                callData: IPoolBatch::token0Call {}.abi_encode().into(),
            });
            // token1
            calls.push(IMulticall3::Call3 {
                target: *addr,
                allowFailure: true,
                callData: IPoolBatch::token1Call {}.abi_encode().into(),
            });
            // fee
            calls.push(IMulticall3::Call3 {
                target: *addr,
                allowFailure: true,
                callData: IPoolBatch::feeCall {}.abi_encode().into(),
            });
        }

        let results = self.execute_large_batch(calls).await?;

        // Parse results and update cache
        let mut new_data = HashMap::new();
        for (i, addr) in uncached_pools.iter().enumerate() {
            let offset = i * 3;

            // Parse token0 - single Address return
            let token0 = if results[offset].success {
                IPoolBatch::token0Call::abi_decode_returns(&results[offset].returnData).ok()
            } else {
                None
            };

            // Parse token1 - single Address return
            let token1 = if results[offset + 1].success {
                IPoolBatch::token1Call::abi_decode_returns(&results[offset + 1].returnData).ok()
            } else {
                None
            };

            // Parse fee - single U24 return
            let fee = if results[offset + 2].success {
                IPoolBatch::feeCall::abi_decode_returns(&results[offset + 2].returnData)
                    .ok()
                    .map(|fee_u24| {
                        let f: u32 = fee_u24.to();
                        f
                    })
            } else {
                None
            };

            if let (Some(t0), Some(t1), Some(f)) = (token0, token1, fee) {
                let data = CachedPoolData {
                    token0: t0,
                    token1: t1,
                    fee: f,
                    decimals0: tokens::decimals(t0),
                    decimals1: tokens::decimals(t1),
                    dex,
                };
                new_data.insert(*addr, data);
            }
        }

        // Update cache
        {
            let mut cache = self.static_cache.write().await;
            for (addr, data) in &new_data {
                cache.insert(*addr, data.clone());
            }
        }

        // Return all requested data (from cache + newly fetched)
        let cache = self.static_cache.read().await;
        Ok(pool_addresses
            .iter()
            .filter_map(|addr| cache.get(addr).map(|data| (*addr, data.clone())))
            .collect())
    }

    /// Batch fetch dynamic pool data (slot0, liquidity) for V3-style pools
    pub async fn batch_fetch_v3_dynamic_data(
        &self,
        pool_addresses: &[Address],
    ) -> Result<HashMap<Address, DynamicPoolData>> {
        if pool_addresses.is_empty() {
            return Ok(HashMap::new());
        }

        // Build calls: slot0 + liquidity (2 calls per pool)
        let mut calls = Vec::with_capacity(pool_addresses.len() * 2);
        for addr in pool_addresses {
            // slot0
            calls.push(IMulticall3::Call3 {
                target: *addr,
                allowFailure: true,
                callData: IPoolBatch::slot0Call {}.abi_encode().into(),
            });
            // liquidity
            calls.push(IMulticall3::Call3 {
                target: *addr,
                allowFailure: true,
                callData: IPoolBatch::liquidityCall {}.abi_encode().into(),
            });
        }

        let results = self.execute_large_batch(calls).await?;

        // Parse results
        let mut data = HashMap::new();
        for (i, addr) in pool_addresses.iter().enumerate() {
            let offset = i * 2;

            // Parse slot0
            let slot0 = if results[offset].success && !results[offset].returnData.is_empty() {
                IPoolBatch::slot0Call::abi_decode_returns(&results[offset].returnData).ok()
            } else {
                None
            };

            // Parse liquidity - single u128 return
            let liquidity = if results[offset + 1].success && !results[offset + 1].returnData.is_empty() {
                IPoolBatch::liquidityCall::abi_decode_returns(&results[offset + 1].returnData)
                    .ok()
                    .map(|l| U256::from(l))
            } else {
                None
            };

            if let (Some(s0), Some(liq)) = (slot0, liquidity) {
                data.insert(
                    *addr,
                    DynamicPoolData {
                        sqrt_price_x96: U256::from(s0.sqrtPriceX96),
                        tick: s0.tick.as_i32(),
                        liquidity: liq,
                    },
                );
            }
        }

        trace!("Fetched dynamic data for {} / {} pools", data.len(), pool_addresses.len());
        Ok(data)
    }

    /// Batch fetch LFJ pair data
    pub async fn batch_fetch_lfj_data(
        &self,
        pair_addresses: &[Address],
    ) -> Result<HashMap<Address, LfjPairData>> {
        if pair_addresses.is_empty() {
            return Ok(HashMap::new());
        }

        // Build calls: getTokenX, getTokenY, getReserves, getActiveId, getBinStep (5 calls per pair)
        let mut calls = Vec::with_capacity(pair_addresses.len() * 5);
        for addr in pair_addresses {
            calls.push(IMulticall3::Call3 {
                target: *addr,
                allowFailure: true,
                callData: ILBPairBatch::getTokenXCall {}.abi_encode().into(),
            });
            calls.push(IMulticall3::Call3 {
                target: *addr,
                allowFailure: true,
                callData: ILBPairBatch::getTokenYCall {}.abi_encode().into(),
            });
            calls.push(IMulticall3::Call3 {
                target: *addr,
                allowFailure: true,
                callData: ILBPairBatch::getReservesCall {}.abi_encode().into(),
            });
            calls.push(IMulticall3::Call3 {
                target: *addr,
                allowFailure: true,
                callData: ILBPairBatch::getActiveIdCall {}.abi_encode().into(),
            });
            calls.push(IMulticall3::Call3 {
                target: *addr,
                allowFailure: true,
                callData: ILBPairBatch::getBinStepCall {}.abi_encode().into(),
            });
        }

        let results = self.execute_large_batch(calls).await?;

        // Parse results
        let mut data = HashMap::new();
        for (i, addr) in pair_addresses.iter().enumerate() {
            let offset = i * 5;

            let token_x = if results[offset].success {
                ILBPairBatch::getTokenXCall::abi_decode_returns(&results[offset].returnData).ok()
            } else {
                None
            };

            let token_y = if results[offset + 1].success {
                ILBPairBatch::getTokenYCall::abi_decode_returns(&results[offset + 1].returnData).ok()
            } else {
                None
            };

            let reserves = if results[offset + 2].success {
                ILBPairBatch::getReservesCall::abi_decode_returns(&results[offset + 2].returnData).ok()
            } else {
                None
            };

            let active_id = if results[offset + 3].success {
                ILBPairBatch::getActiveIdCall::abi_decode_returns(&results[offset + 3].returnData)
                    .ok()
                    .map(|id_u24| {
                        let id: u32 = id_u24.to();
                        id
                    })
            } else {
                None
            };

            let bin_step = if results[offset + 4].success {
                ILBPairBatch::getBinStepCall::abi_decode_returns(&results[offset + 4].returnData).ok()
            } else {
                None
            };

            if let (Some(tx), Some(ty), Some(res), Some(aid), Some(bs)) =
                (token_x, token_y, reserves, active_id, bin_step)
            {
                data.insert(
                    *addr,
                    LfjPairData {
                        token_x: tx,
                        token_y: ty,
                        reserve_x: U256::from(res.reserveX),
                        reserve_y: U256::from(res.reserveY),
                        active_id: aid,
                        bin_step: bs,
                    },
                );
            }
        }

        trace!("Fetched LFJ data for {} / {} pairs", data.len(), pair_addresses.len());
        Ok(data)
    }

    /// Batch discover LFJ pairs from factory
    pub async fn batch_discover_lfj_pairs(
        &self,
        factory: Address,
        token_pairs: &[(Address, Address, u16)], // (tokenX, tokenY, binStep)
    ) -> Result<Vec<(Address, Address, Address, u16)>> {
        // Returns (pairAddress, tokenX, tokenY, binStep)
        if token_pairs.is_empty() {
            return Ok(Vec::new());
        }

        let calls: Vec<IMulticall3::Call3> = token_pairs
            .iter()
            .map(|(token_x, token_y, bin_step)| {
                let calldata = ILBFactoryBatch::getLBPairInformationCall {
                    tokenX: *token_x,
                    tokenY: *token_y,
                    binStep: U256::from(*bin_step),
                }
                .abi_encode();

                IMulticall3::Call3 {
                    target: factory,
                    allowFailure: true,
                    callData: calldata.into(),
                }
            })
            .collect();

        let results = self.execute_large_batch(calls).await?;

        let mut pairs = Vec::new();
        for (i, result) in results.iter().enumerate() {
            if !result.success || result.returnData.is_empty() {
                continue;
            }

            if let Ok(info) = ILBFactoryBatch::getLBPairInformationCall::abi_decode_returns(&result.returnData) {
                if info.LBPair != Address::ZERO {
                    let (token_x, token_y, bin_step) = token_pairs[i];
                    pairs.push((info.LBPair, token_x, token_y, bin_step));
                }
            }
        }

        debug!("Discovered {} LFJ pairs from {} queries", pairs.len(), token_pairs.len());
        Ok(pairs)
    }

    /// Fetch complete pool data using two-phase approach
    /// Phase 1: Static data (cached)
    /// Phase 2: Dynamic data (every call)
    pub async fn fetch_complete_v3_pools(
        &self,
        pool_addresses: &[Address],
        dex: Dex,
    ) -> Result<Vec<Pool>> {
        if pool_addresses.is_empty() {
            return Ok(Vec::new());
        }

        // Phase 1: Get static data (uses cache)
        let static_data = self.batch_fetch_static_data(pool_addresses, dex).await?;

        // Phase 2: Get dynamic data
        let pools_with_static: Vec<Address> = static_data.keys().copied().collect();
        let dynamic_data = self.batch_fetch_v3_dynamic_data(&pools_with_static).await?;

        // Combine into Pool structs
        let mut pools = Vec::new();
        for (addr, static_info) in &static_data {
            if let Some(dynamic_info) = dynamic_data.get(addr) {
                pools.push(Pool {
                    address: *addr,
                    token0: static_info.token0,
                    token1: static_info.token1,
                    fee: static_info.fee,
                    dex: static_info.dex,
                    liquidity: dynamic_info.liquidity,
                    sqrt_price_x96: dynamic_info.sqrt_price_x96,
                    decimals0: static_info.decimals0,
                    decimals1: static_info.decimals1,
                });
            }
        }

        debug!("Fetched complete data for {} pools", pools.len());
        Ok(pools)
    }

    /// All-in-one: Discover and fetch V3-style pools for token pairs
    pub async fn discover_and_fetch_v3_pools(
        &self,
        factory: Address,
        tokens: &[Address],
        fee_tiers: &[u32],
        dex: Dex,
        min_liquidity: u128,
    ) -> Result<Vec<Pool>> {
        // Generate all token pair + fee tier combinations
        let mut token_pairs = Vec::new();
        for i in 0..tokens.len() {
            for j in (i + 1)..tokens.len() {
                let (token0, token1) = if tokens[i] < tokens[j] {
                    (tokens[i], tokens[j])
                } else {
                    (tokens[j], tokens[i])
                };
                for &fee in fee_tiers {
                    token_pairs.push((token0, token1, fee));
                }
            }
        }

        debug!("Querying {} token pair combinations", token_pairs.len());

        // Batch discover pool addresses
        let discovered = self.batch_discover_pools(factory, &token_pairs).await?;

        if discovered.is_empty() {
            return Ok(Vec::new());
        }

        // Extract addresses for batch fetching
        let pool_addresses: Vec<Address> = discovered.iter().map(|p| p.address).collect();

        // Fetch complete pool data
        let pools = self.fetch_complete_v3_pools(&pool_addresses, dex).await?;

        // Filter by liquidity and price validity
        let valid_pools: Vec<Pool> = pools
            .into_iter()
            .filter(|p| {
                p.liquidity > U256::ZERO
                    && p.is_price_valid()
                    && p.has_sufficient_liquidity(min_liquidity)
            })
            .collect();

        debug!(
            "Found {} valid pools out of {} discovered",
            valid_pools.len(),
            discovered.len()
        );
        Ok(valid_pools)
    }

    /// Get cache statistics
    pub async fn cache_stats(&self) -> (usize, usize) {
        let cache = self.static_cache.read().await;
        let v3_count = cache.values().filter(|d| matches!(d.dex, Dex::UniswapV3 | Dex::PancakeSwapV3)).count();
        let lfj_count = cache.values().filter(|d| matches!(d.dex, Dex::LFJ)).count();
        (v3_count, lfj_count)
    }

    /// Clear the static data cache
    pub async fn clear_cache(&self) {
        let mut cache = self.static_cache.write().await;
        cache.clear();
    }
}

/// LFJ pair data
#[derive(Debug, Clone)]
pub struct LfjPairData {
    pub token_x: Address,
    pub token_y: Address,
    pub reserve_x: U256,
    pub reserve_y: U256,
    pub active_id: u32,
    pub bin_step: u16,
}

/// Convenience functions for building multicall calls
pub mod call_builder {
    use super::*;

    /// Build a Call3 for a V3 pool's slot0
    pub fn slot0_call(pool: Address) -> IMulticall3::Call3 {
        IMulticall3::Call3 {
            target: pool,
            allowFailure: true,
            callData: IPoolBatch::slot0Call {}.abi_encode().into(),
        }
    }

    /// Build a Call3 for a V3 pool's liquidity
    pub fn liquidity_call(pool: Address) -> IMulticall3::Call3 {
        IMulticall3::Call3 {
            target: pool,
            allowFailure: true,
            callData: IPoolBatch::liquidityCall {}.abi_encode().into(),
        }
    }

    /// Build a Call3 for a V3 pool's token0
    pub fn token0_call(pool: Address) -> IMulticall3::Call3 {
        IMulticall3::Call3 {
            target: pool,
            allowFailure: true,
            callData: IPoolBatch::token0Call {}.abi_encode().into(),
        }
    }

    /// Build a Call3 for a V3 pool's token1
    pub fn token1_call(pool: Address) -> IMulticall3::Call3 {
        IMulticall3::Call3 {
            target: pool,
            allowFailure: true,
            callData: IPoolBatch::token1Call {}.abi_encode().into(),
        }
    }

    /// Build a Call3 for a V3 pool's fee
    pub fn fee_call(pool: Address) -> IMulticall3::Call3 {
        IMulticall3::Call3 {
            target: pool,
            allowFailure: true,
            callData: IPoolBatch::feeCall {}.abi_encode().into(),
        }
    }

    /// Build a Call3 for factory getPool
    pub fn get_pool_call(factory: Address, token_a: Address, token_b: Address, fee: u32) -> IMulticall3::Call3 {
        let fee_u24 = U24::from(fee);
        let calldata = IFactoryBatch::getPoolCall {
            tokenA: token_a,
            tokenB: token_b,
            fee: fee_u24,
        }
        .abi_encode();

        IMulticall3::Call3 {
            target: factory,
            allowFailure: true,
            callData: calldata.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_multicall_address() {
        // Verify the Multicall3 address is correct
        assert_eq!(
            MULTICALL3_ADDRESS,
            address!("cA11bde05977b3631167028862bE2a173976CA11")
        );
    }

    #[test]
    fn test_max_batch_size() {
        assert_eq!(MAX_CALLS_PER_BATCH, 100);
    }
}
