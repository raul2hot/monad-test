use alloy::{
    primitives::{aliases::I24, keccak256, Address, FixedBytes, Uint, U256},
    providers::Provider,
    rpc::types::Filter,
    sol,
    sol_types::{SolEvent, SolValue},
};
use async_trait::async_trait;

use super::{Dex, DexClient, Pool};
use crate::config::contracts::uniswap_v4::{
    COMMON_FEE_TIERS, COMMON_TICK_SPACINGS, DYNAMIC_FEE_FLAG, POOL_MANAGER, STATE_VIEW,
};
use crate::config::tokens;

// Uniswap V4 PoolKey structure for computing PoolId
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

// Uniswap V4 PoolManager Initialize event for pool discovery
sol! {
    #[sol(rpc)]
    interface IPoolManager {
        /// Emitted when a new pool is initialized
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

// Uniswap V4 StateView interface for off-chain state queries
sol! {
    #[sol(rpc)]
    interface IStateView {
        /// Get slot0 data for a pool (price, tick, fees)
        function getSlot0(bytes32 poolId) external view returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint24 protocolFee,
            uint24 lpFee
        );

        /// Get current liquidity for a pool
        function getLiquidity(bytes32 poolId) external view returns (uint128 liquidity);

        /// Get tick info for a specific tick
        function getTickInfo(bytes32 poolId, int24 tick) external view returns (
            uint128 liquidityGross,
            int128 liquidityNet,
            uint256 feeGrowthOutside0X128,
            uint256 feeGrowthOutside1X128
        );
    }
}

/// Compute PoolId from PoolKey using keccak256(abi.encode(PoolKey))
fn compute_pool_id(pool_key: &PoolKey) -> FixedBytes<32> {
    let encoded = pool_key.abi_encode();
    keccak256(&encoded)
}

/// Check if a fee has dynamic fee flag set
fn is_dynamic_fee(fee: u32) -> bool {
    fee & DYNAMIC_FEE_FLAG != 0
}

/// Discovered V4 pool info from Initialize events
#[derive(Debug, Clone)]
struct DiscoveredPool {
    currency0: Address,
    currency1: Address,
    fee: u32,
    tick_spacing: i32,
    hooks: Address,
}

/// Uniswap V4 DEX client
/// Uses the singleton PoolManager pattern with StateView for queries
pub struct UniswapV4Client<P> {
    provider: P,
    pool_manager: Address,
    state_view: Address,
    fee_tiers: Vec<u32>,
    tick_spacings: Vec<i32>,
}

impl<P: Provider + Clone> UniswapV4Client<P> {
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            pool_manager: POOL_MANAGER,
            state_view: STATE_VIEW,
            fee_tiers: COMMON_FEE_TIERS.to_vec(),
            tick_spacings: COMMON_TICK_SPACINGS.to_vec(),
        }
    }

    /// Generate PoolKey for a token pair with given fee, tick spacing, and hooks
    /// Note: currency0 must be < currency1 (sorted by address)
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

    /// Check if a pool exists and get its state
    async fn get_pool_state(
        &self,
        token0: Address,
        token1: Address,
        fee: u32,
        tick_spacing: i32,
        hooks: Address,
    ) -> eyre::Result<Option<Pool>> {
        let pool_key = Self::create_pool_key(token0, token1, fee, tick_spacing, hooks);
        let pool_id = compute_pool_id(&pool_key);

        let state_view = IStateView::new(self.state_view, &self.provider);

        // Try to get slot0 - if pool doesn't exist, this will return zeros
        let slot0 = state_view.getSlot0(pool_id).call().await?;

        // Check if pool exists (sqrtPriceX96 > 0 means pool is initialized)
        if slot0.sqrtPriceX96 == 0 {
            return Ok(None);
        }

        // Convert lpFee from Uint<24,1> to u32
        let lp_fee_u32: u32 = slot0.lpFee.to::<u32>();

        // Skip pools with dynamic fees (hooks may manipulate pricing)
        if is_dynamic_fee(lp_fee_u32) {
            tracing::trace!(
                "Skipping V4 pool with dynamic fee: {}-{} fee={} tickSpacing={}",
                token0,
                token1,
                fee,
                tick_spacing
            );
            return Ok(None);
        }

        // Get liquidity
        let liquidity: u128 = state_view.getLiquidity(pool_id).call().await?;

        // Use the actual LP fee from slot0 for accurate calculations
        let effective_fee: u32 = lp_fee_u32;

        Ok(Some(Pool {
            // For V4, we use the pool_id as a pseudo-address since pools don't have individual addresses
            address: Address::from_slice(&pool_id[12..32]),
            token0,
            token1,
            fee: effective_fee,
            dex: Dex::UniswapV4,
            liquidity: U256::from(liquidity),
            sqrt_price_x96: U256::from(slot0.sqrtPriceX96),
            decimals0: tokens::decimals(token0),
            decimals1: tokens::decimals(token1),
        }))
    }

    /// Discover V4 pools by querying Initialize events from PoolManager
    async fn discover_pools_from_events(
        &self,
        tokens: &[Address],
    ) -> eyre::Result<Vec<DiscoveredPool>> {
        use std::collections::HashSet;

        let token_set: HashSet<Address> = tokens.iter().copied().collect();
        let mut discovered = Vec::new();

        // Get the latest block number
        let latest_block = self.provider.get_block_number().await?;

        // Query Initialize events from PoolManager
        // Look back ~500k blocks to catch all pools (adjust based on chain age)
        let from_block = latest_block.saturating_sub(500_000);

        tracing::debug!(
            "Querying V4 Initialize events from block {} to {} on PoolManager {}",
            from_block,
            latest_block,
            self.pool_manager
        );

        let filter = Filter::new()
            .address(self.pool_manager)
            .event_signature(IPoolManager::Initialize::SIGNATURE_HASH)
            .from_block(from_block)
            .to_block(latest_block);

        let logs = self.provider.get_logs(&filter).await?;

        tracing::debug!("Found {} Initialize events from PoolManager", logs.len());

        for log in logs {
            // Parse the Initialize event
            match log.log_decode::<IPoolManager::Initialize>() {
                Ok(decoded) => {
                    let event = decoded.inner.data;
                    let currency0 = event.currency0;
                    let currency1 = event.currency1;

                    // Only include pools with at least one monitored token
                    if token_set.contains(&currency0) || token_set.contains(&currency1) {
                        let fee: u32 = event.fee.to::<u32>();
                        let tick_spacing: i32 = event.tickSpacing.as_i32();

                        discovered.push(DiscoveredPool {
                            currency0,
                            currency1,
                            fee,
                            tick_spacing,
                            hooks: event.hooks,
                        });

                        tracing::debug!(
                            "Discovered V4 pool: {}-{} (fee: {}, tickSpacing: {}, hooks: {})",
                            tokens::symbol(currency0),
                            tokens::symbol(currency1),
                            fee,
                            tick_spacing,
                            event.hooks
                        );
                    }
                }
                Err(e) => {
                    tracing::trace!("Failed to decode Initialize event: {}", e);
                }
            }
        }

        Ok(discovered)
    }

    /// Fallback brute-force pool discovery for vanilla pools (hooks=ZERO)
    /// Used when event indexing fails or finds no pools
    async fn get_pools_brute_force(&self, tokens: &[Address]) -> eyre::Result<Vec<Pool>> {
        let mut pools = Vec::new();
        let mut checked_count = 0u32;
        let mut error_count = 0u32;

        // Check all token pairs across fee tiers and tick spacings (vanilla pools only)
        for i in 0..tokens.len() {
            for j in (i + 1)..tokens.len() {
                let (token0, token1) = if tokens[i] < tokens[j] {
                    (tokens[i], tokens[j])
                } else {
                    (tokens[j], tokens[i])
                };

                for &fee in &self.fee_tiers {
                    for &tick_spacing in &self.tick_spacings {
                        if !is_valid_fee_tick_combo(fee, tick_spacing) {
                            continue;
                        }

                        checked_count += 1;
                        // Use Address::ZERO for hooks (vanilla pools only)
                        match self
                            .get_pool_state(token0, token1, fee, tick_spacing, Address::ZERO)
                            .await
                        {
                            Ok(Some(pool)) => {
                                if pool.liquidity > U256::ZERO
                                    && pool.is_price_valid()
                                    && pool.has_sufficient_liquidity(MIN_LIQUIDITY)
                                {
                                    tracing::info!(
                                        "Found vanilla V4 pool: {}-{} (fee: {}, tickSpacing: {}, liq: {}, price: {:.8})",
                                        tokens::symbol(token0),
                                        tokens::symbol(token1),
                                        fee,
                                        tick_spacing,
                                        pool.liquidity,
                                        pool.price_0_to_1()
                                    );
                                    pools.push(pool);
                                }
                            }
                            Ok(None) => {}
                            Err(e) => {
                                error_count += 1;
                                if error_count <= 3 {
                                    tracing::warn!(
                                        "V4 StateView error for {}-{}: {}",
                                        tokens::symbol(token0),
                                        tokens::symbol(token1),
                                        e
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        if error_count > 0 {
            tracing::warn!(
                "Uniswap V4 (brute-force): checked {} configs, {} errors",
                checked_count,
                error_count
            );
        } else {
            tracing::info!(
                "Uniswap V4 (brute-force): checked {} configs, found {} vanilla pools",
                checked_count,
                pools.len()
            );
        }

        Ok(pools)
    }
}

/// Minimum liquidity threshold (1000 units with 18 decimals)
const MIN_LIQUIDITY: u128 = 1000 * 10u128.pow(18);

#[async_trait]
impl<P: Provider + Clone + Send + Sync> DexClient for UniswapV4Client<P> {
    async fn get_pools(&self, tokens: &[Address]) -> eyre::Result<Vec<Pool>> {
        let mut pools = Vec::new();
        let mut error_count = 0u32;

        // Step 1: Discover pools from Initialize events
        let discovered = match self.discover_pools_from_events(tokens).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(
                    "Failed to query V4 Initialize events from PoolManager {}: {}",
                    self.pool_manager,
                    e
                );
                // Fall back to brute-force approach with hooks=ZERO for vanilla pools
                return self.get_pools_brute_force(tokens).await;
            }
        };

        if discovered.is_empty() {
            tracing::info!(
                "Uniswap V4: no Initialize events found for monitored tokens, trying brute-force"
            );
            // Fall back to brute-force for vanilla pools (hooks=ZERO)
            return self.get_pools_brute_force(tokens).await;
        }

        tracing::info!(
            "Uniswap V4: discovered {} pools from Initialize events",
            discovered.len()
        );

        // Step 2: Query StateView for each discovered pool
        for dp in &discovered {
            match self
                .get_pool_state(
                    dp.currency0,
                    dp.currency1,
                    dp.fee,
                    dp.tick_spacing,
                    dp.hooks,
                )
                .await
            {
                Ok(Some(pool)) => {
                    if pool.liquidity > U256::ZERO
                        && pool.is_price_valid()
                        && pool.has_sufficient_liquidity(MIN_LIQUIDITY)
                    {
                        tracing::info!(
                            "Found valid Uniswap V4 pool: {}-{} (fee: {}, tickSpacing: {}, hooks: {}, liq: {}, price: {:.8})",
                            tokens::symbol(dp.currency0),
                            tokens::symbol(dp.currency1),
                            dp.fee,
                            dp.tick_spacing,
                            dp.hooks,
                            pool.liquidity,
                            pool.price_0_to_1()
                        );
                        pools.push(pool);
                    } else {
                        tracing::debug!(
                            "Skipping V4 pool {}-{} - insufficient liquidity or invalid price",
                            tokens::symbol(dp.currency0),
                            tokens::symbol(dp.currency1)
                        );
                    }
                }
                Ok(None) => {
                    tracing::debug!(
                        "V4 pool {}-{} not initialized (sqrtPrice=0)",
                        tokens::symbol(dp.currency0),
                        tokens::symbol(dp.currency1)
                    );
                }
                Err(e) => {
                    error_count += 1;
                    if error_count <= 3 {
                        tracing::warn!(
                            "V4 StateView error for {}-{}: {}",
                            tokens::symbol(dp.currency0),
                            tokens::symbol(dp.currency1),
                            e
                        );
                    }
                }
            }
        }

        if error_count > 3 {
            tracing::warn!(
                "Uniswap V4: {} total StateView errors (suppressed after first 3)",
                error_count
            );
        }

        tracing::info!(
            "Uniswap V4: found {} valid pools from {} discovered",
            pools.len(),
            discovered.len()
        );

        Ok(pools)
    }

    fn dex(&self) -> Dex {
        Dex::UniswapV4
    }
}

/// Check if fee and tick spacing combination is valid/common
/// This helps reduce unnecessary RPC calls for unlikely pool configurations
fn is_valid_fee_tick_combo(fee: u32, tick_spacing: i32) -> bool {
    // Common V4 pool configurations:
    // - 100 bps (0.01%) with tick spacing 1
    // - 500 bps (0.05%) with tick spacing 10
    // - 3000 bps (0.3%) with tick spacing 60
    // - 10000 bps (1%) with tick spacing 200
    // But V4 allows any combination, so we're somewhat lenient
    match fee {
        100 => tick_spacing == 1 || tick_spacing == 10,
        500 => tick_spacing == 10 || tick_spacing == 1,
        3000 => tick_spacing == 60 || tick_spacing == 10,
        10000 => tick_spacing == 200 || tick_spacing == 60,
        100000 => tick_spacing == 200, // 10% fee tier (rare but possible)
        _ => true, // Allow other combinations
    }
}
