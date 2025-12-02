//! Batch-enabled DEX Client
//!
//! Provides efficient batched pool discovery and state fetching across all DEXes
//! using Multicall3. Reduces RPC calls by 80x compared to individual calls.

use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use async_trait::async_trait;
use std::sync::Arc;
use tracing::{debug, info, warn};

use super::{Dex, DexClient, Pool};
use crate::config::contracts;
use crate::config::tokens;
use crate::multicall::{BatchFetcher, LfjPairData};

/// Minimum liquidity threshold (1000 units with 18 decimals)
const MIN_LIQUIDITY: u128 = 1000 * 10u128.pow(18);

/// Common LFJ bin steps to query
const LFJ_BIN_STEPS: [u16; 6] = [1, 5, 10, 15, 20, 25];

/// Batch-enabled Uniswap V3 client
pub struct BatchUniswapV3Client<P> {
    batch_fetcher: Arc<BatchFetcher<P>>,
    factory: Address,
    fee_tiers: Vec<u32>,
}

impl<P: Provider + Clone + 'static> BatchUniswapV3Client<P> {
    pub fn new(provider: P) -> Self {
        Self {
            batch_fetcher: Arc::new(BatchFetcher::new(provider)),
            factory: contracts::uniswap_v3::FACTORY,
            fee_tiers: contracts::uniswap_v3::FEE_TIERS.to_vec(),
        }
    }

    pub fn with_fetcher(batch_fetcher: Arc<BatchFetcher<P>>) -> Self {
        Self {
            batch_fetcher,
            factory: contracts::uniswap_v3::FACTORY,
            fee_tiers: contracts::uniswap_v3::FEE_TIERS.to_vec(),
        }
    }
}

#[async_trait]
impl<P: Provider + Clone + Send + Sync + 'static> DexClient for BatchUniswapV3Client<P> {
    async fn get_pools(&self, tokens: &[Address]) -> eyre::Result<Vec<Pool>> {
        self.batch_fetcher
            .discover_and_fetch_v3_pools(
                self.factory,
                tokens,
                &self.fee_tiers,
                Dex::UniswapV3,
                MIN_LIQUIDITY,
            )
            .await
    }

    fn dex(&self) -> Dex {
        Dex::UniswapV3
    }
}

/// Batch-enabled PancakeSwap V3 client
pub struct BatchPancakeSwapClient<P> {
    batch_fetcher: Arc<BatchFetcher<P>>,
    factory: Address,
    fee_tiers: Vec<u32>,
}

impl<P: Provider + Clone + 'static> BatchPancakeSwapClient<P> {
    pub fn new(provider: P) -> Self {
        Self {
            batch_fetcher: Arc::new(BatchFetcher::new(provider)),
            factory: contracts::pancakeswap_v3::FACTORY,
            fee_tiers: contracts::pancakeswap_v3::FEE_TIERS.to_vec(),
        }
    }

    pub fn with_fetcher(batch_fetcher: Arc<BatchFetcher<P>>) -> Self {
        Self {
            batch_fetcher,
            factory: contracts::pancakeswap_v3::FACTORY,
            fee_tiers: contracts::pancakeswap_v3::FEE_TIERS.to_vec(),
        }
    }
}

#[async_trait]
impl<P: Provider + Clone + Send + Sync + 'static> DexClient for BatchPancakeSwapClient<P> {
    async fn get_pools(&self, tokens: &[Address]) -> eyre::Result<Vec<Pool>> {
        self.batch_fetcher
            .discover_and_fetch_v3_pools(
                self.factory,
                tokens,
                &self.fee_tiers,
                Dex::PancakeSwapV3,
                MIN_LIQUIDITY,
            )
            .await
    }

    fn dex(&self) -> Dex {
        Dex::PancakeSwapV3
    }
}

/// Batch-enabled LFJ (TraderJoe V2.1) client
pub struct BatchLfjClient<P> {
    batch_fetcher: Arc<BatchFetcher<P>>,
    factory: Address,
    bin_steps: Vec<u16>,
}

impl<P: Provider + Clone + 'static> BatchLfjClient<P> {
    pub fn new(provider: P) -> Self {
        Self {
            batch_fetcher: Arc::new(BatchFetcher::new(provider)),
            factory: contracts::lfj::LB_FACTORY,
            bin_steps: LFJ_BIN_STEPS.to_vec(),
        }
    }

    pub fn with_fetcher(batch_fetcher: Arc<BatchFetcher<P>>) -> Self {
        Self {
            batch_fetcher,
            factory: contracts::lfj::LB_FACTORY,
            bin_steps: LFJ_BIN_STEPS.to_vec(),
        }
    }

    /// Convert LFJ pair data to Pool struct
    fn lfj_to_pool(pair_addr: Address, data: &LfjPairData) -> Pool {
        // Sort tokens to get consistent token0/token1
        let (token0, token1) = if data.token_x < data.token_y {
            (data.token_x, data.token_y)
        } else {
            (data.token_y, data.token_x)
        };

        // Calculate effective fee from bin step (approximate)
        // LFJ fee = baseFee + variableFee, roughly 0.1% - 1% depending on volatility
        // bin_step / 10000 gives approximate base fee percentage
        let fee_bps = (data.bin_step as u32) * 10; // Convert to bps-like value

        // Calculate liquidity from reserves
        let total_liquidity = data.reserve_x + data.reserve_y;

        // Calculate sqrt price from active ID
        // Price at bin = (1 + binStep/10000)^(activeId - 8388608)
        // This is an approximation for display purposes
        let sqrt_price = calculate_lfj_sqrt_price(data.active_id, data.bin_step);

        Pool {
            address: pair_addr,
            token0,
            token1,
            fee: fee_bps * 100, // Convert to hundredths of bps like V3
            dex: Dex::LFJ,
            liquidity: total_liquidity,
            sqrt_price_x96: sqrt_price,
            decimals0: tokens::decimals(token0),
            decimals1: tokens::decimals(token1),
        }
    }
}

/// Calculate approximate sqrt price from LFJ active ID and bin step
fn calculate_lfj_sqrt_price(active_id: u32, bin_step: u16) -> U256 {
    // LFJ uses: price = (1 + binStep/10000)^(activeId - 2^23)
    // We calculate an approximate sqrtPriceX96 for consistency
    const ID_OFFSET: i64 = 8388608; // 2^23
    let exponent = (active_id as i64) - ID_OFFSET;
    let base = 1.0 + (bin_step as f64) / 10000.0;
    let price = base.powf(exponent as f64);
    let sqrt_price = price.sqrt();
    let sqrt_price_x96 = sqrt_price * 2_f64.powi(96);

    // Convert to U256, clamping to avoid overflow
    if sqrt_price_x96 >= u128::MAX as f64 {
        U256::from(u128::MAX)
    } else if sqrt_price_x96 <= 0.0 {
        U256::from(1u64) // Minimum valid price
    } else {
        U256::from(sqrt_price_x96 as u128)
    }
}

#[async_trait]
impl<P: Provider + Clone + Send + Sync + 'static> DexClient for BatchLfjClient<P> {
    async fn get_pools(&self, tokens: &[Address]) -> eyre::Result<Vec<Pool>> {
        // Generate token pair + bin step combinations
        let mut token_pairs = Vec::new();
        for i in 0..tokens.len() {
            for j in (i + 1)..tokens.len() {
                for &bin_step in &self.bin_steps {
                    // LFJ uses tokenX/tokenY ordering (not sorted)
                    token_pairs.push((tokens[i], tokens[j], bin_step));
                    // Also try reverse order
                    token_pairs.push((tokens[j], tokens[i], bin_step));
                }
            }
        }

        debug!("Querying {} LFJ pair combinations", token_pairs.len());

        // Batch discover pairs
        let discovered = self
            .batch_fetcher
            .batch_discover_lfj_pairs(self.factory, &token_pairs)
            .await?;

        if discovered.is_empty() {
            return Ok(Vec::new());
        }

        // Extract unique pair addresses
        let pair_addresses: Vec<Address> = discovered.iter().map(|(addr, _, _, _)| *addr).collect();

        // Batch fetch pair data
        let pair_data = self.batch_fetcher.batch_fetch_lfj_data(&pair_addresses).await?;

        // Convert to Pool structs
        let mut pools = Vec::new();
        for (addr, data) in pair_data {
            let pool = Self::lfj_to_pool(addr, &data);

            // Filter by liquidity
            if pool.liquidity > U256::ZERO && pool.has_sufficient_liquidity(MIN_LIQUIDITY) {
                pools.push(pool);
            }
        }

        debug!("Found {} valid LFJ pools", pools.len());
        Ok(pools)
    }

    fn dex(&self) -> Dex {
        Dex::LFJ
    }
}

/// Unified batch DEX manager for all supported DEXes
pub struct BatchDexManager<P> {
    batch_fetcher: Arc<BatchFetcher<P>>,
    uniswap_v3: BatchUniswapV3Client<P>,
    pancakeswap: BatchPancakeSwapClient<P>,
    lfj: BatchLfjClient<P>,
}

impl<P: Provider + Clone + Send + Sync + 'static> BatchDexManager<P> {
    /// Create a new batch DEX manager with shared batch fetcher
    pub fn new(provider: P) -> Self {
        let batch_fetcher = Arc::new(BatchFetcher::new(provider));

        Self {
            uniswap_v3: BatchUniswapV3Client::with_fetcher(batch_fetcher.clone()),
            pancakeswap: BatchPancakeSwapClient::with_fetcher(batch_fetcher.clone()),
            lfj: BatchLfjClient::with_fetcher(batch_fetcher.clone()),
            batch_fetcher,
        }
    }

    /// Fetch pools from all DEXes using batch calls
    pub async fn fetch_all_pools(&self, tokens: &[Address]) -> AllPoolsResult {
        let mut result = AllPoolsResult::default();

        // Fetch from Uniswap V3
        match self.uniswap_v3.get_pools(tokens).await {
            Ok(pools) => {
                result.uniswap_v3_count = pools.len();
                result.pools.extend(pools);
            }
            Err(e) => {
                warn!("Failed to fetch Uniswap V3 pools: {}", e);
            }
        }

        // Fetch from PancakeSwap
        match self.pancakeswap.get_pools(tokens).await {
            Ok(pools) => {
                result.pancakeswap_count = pools.len();
                result.pools.extend(pools);
            }
            Err(e) => {
                warn!("Failed to fetch PancakeSwap pools: {}", e);
            }
        }

        // Fetch from LFJ
        match self.lfj.get_pools(tokens).await {
            Ok(pools) => {
                result.lfj_count = pools.len();
                result.pools.extend(pools);
            }
            Err(e) => {
                warn!("Failed to fetch LFJ pools: {}", e);
            }
        }

        result
    }

    /// Get cache statistics
    pub async fn cache_stats(&self) -> (usize, usize) {
        self.batch_fetcher.cache_stats().await
    }

    /// Clear the cache
    pub async fn clear_cache(&self) {
        self.batch_fetcher.clear_cache().await;
    }
}

/// Result of fetching pools from all DEXes
#[derive(Debug, Default)]
pub struct AllPoolsResult {
    pub pools: Vec<Pool>,
    pub uniswap_v3_count: usize,
    pub uniswap_v4_count: usize,
    pub pancakeswap_count: usize,
    pub lfj_count: usize,
}

impl AllPoolsResult {
    pub fn total_count(&self) -> usize {
        self.uniswap_v3_count + self.uniswap_v4_count + self.pancakeswap_count + self.lfj_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lfj_sqrt_price_calculation() {
        // Test middle ID (should give price ~1)
        let sqrt_price = calculate_lfj_sqrt_price(8388608, 10);
        assert!(sqrt_price > U256::ZERO);

        // Test higher ID (should give higher price)
        let sqrt_price_high = calculate_lfj_sqrt_price(8388708, 10);
        assert!(sqrt_price_high > sqrt_price);

        // Test lower ID (should give lower price)
        let sqrt_price_low = calculate_lfj_sqrt_price(8388508, 10);
        assert!(sqrt_price_low < sqrt_price);
    }
}
