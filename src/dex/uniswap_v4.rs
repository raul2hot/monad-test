use alloy::{
    primitives::{aliases::I24, keccak256, Address, FixedBytes, Uint, U256},
    providers::Provider,
    sol,
    sol_types::SolValue,
};
use async_trait::async_trait;

use super::{Dex, DexClient, Pool};
use crate::config::contracts::uniswap_v4::{
    COMMON_FEE_TIERS, COMMON_TICK_SPACINGS, DYNAMIC_FEE_FLAG, STATE_VIEW,
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

/// Uniswap V4 DEX client
/// Uses the singleton PoolManager pattern with StateView for queries
pub struct UniswapV4Client<P> {
    provider: P,
    state_view: Address,
    fee_tiers: Vec<u32>,
    tick_spacings: Vec<i32>,
}

impl<P: Provider + Clone> UniswapV4Client<P> {
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            state_view: STATE_VIEW,
            fee_tiers: COMMON_FEE_TIERS.to_vec(),
            tick_spacings: COMMON_TICK_SPACINGS.to_vec(),
        }
    }

    /// Generate PoolKey for a token pair with given fee and tick spacing
    /// Note: currency0 must be < currency1 (sorted by address)
    fn create_pool_key(
        token0: Address,
        token1: Address,
        fee: u32,
        tick_spacing: i32,
    ) -> PoolKey {
        PoolKey {
            currency0: token0,
            currency1: token1,
            fee: Uint::<24, 1>::from(fee),
            tickSpacing: I24::try_from(tick_spacing).unwrap_or(I24::ZERO),
            hooks: Address::ZERO, // Only query pools without hooks for safety
        }
    }

    /// Check if a pool exists and get its state
    async fn get_pool_state(
        &self,
        token0: Address,
        token1: Address,
        fee: u32,
        tick_spacing: i32,
    ) -> eyre::Result<Option<Pool>> {
        let pool_key = Self::create_pool_key(token0, token1, fee, tick_spacing);
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
}

/// Minimum liquidity threshold (1000 units with 18 decimals)
const MIN_LIQUIDITY: u128 = 1000 * 10u128.pow(18);

#[async_trait]
impl<P: Provider + Clone + Send + Sync> DexClient for UniswapV4Client<P> {
    async fn get_pools(&self, tokens: &[Address]) -> eyre::Result<Vec<Pool>> {
        let mut pools = Vec::new();
        let mut checked_count = 0u32;
        let mut error_count = 0u32;

        // Check all token pairs across fee tiers and tick spacings
        for i in 0..tokens.len() {
            for j in (i + 1)..tokens.len() {
                // Ensure tokens are sorted (currency0 < currency1)
                let (token0, token1) = if tokens[i] < tokens[j] {
                    (tokens[i], tokens[j])
                } else {
                    (tokens[j], tokens[i])
                };

                // V4 pools can have various fee/tickSpacing combinations
                // Common pairings: fee=500/tickSpacing=10, fee=3000/tickSpacing=60, etc.
                for &fee in &self.fee_tiers {
                    for &tick_spacing in &self.tick_spacings {
                        // Skip obviously invalid combinations
                        // (tick spacing should be appropriate for the fee tier)
                        if !is_valid_fee_tick_combo(fee, tick_spacing) {
                            continue;
                        }

                        checked_count += 1;
                        match self.get_pool_state(token0, token1, fee, tick_spacing).await {
                            Ok(Some(pool)) => {
                                if pool.liquidity > U256::ZERO
                                    && pool.is_price_valid()
                                    && pool.has_sufficient_liquidity(MIN_LIQUIDITY)
                                {
                                    tracing::info!(
                                        "Found valid Uniswap V4 pool: {}-{} (fee: {}, tickSpacing: {}, liq: {}, price: {:.8})",
                                        tokens::symbol(token0),
                                        tokens::symbol(token1),
                                        fee,
                                        tick_spacing,
                                        pool.liquidity,
                                        pool.price_0_to_1()
                                    );
                                    pools.push(pool);
                                } else {
                                    tracing::trace!(
                                        "Skipping V4 pool {}-{} - insufficient liquidity or invalid price",
                                        token0,
                                        token1
                                    );
                                }
                            }
                            Ok(None) => {
                                // Pool doesn't exist, skip silently
                            }
                            Err(e) => {
                                error_count += 1;
                                // Only log first few errors to avoid spam
                                if error_count <= 3 {
                                    tracing::warn!(
                                        "V4 StateView error for {}-{}: {} (this may indicate V4 is not deployed)",
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

        // Log summary
        if error_count > 0 {
            tracing::warn!(
                "Uniswap V4: checked {} pool configs, {} errors (StateView may not be deployed at {})",
                checked_count,
                error_count,
                self.state_view
            );
        } else if pools.is_empty() && checked_count > 0 {
            tracing::info!(
                "Uniswap V4: checked {} pool configs, no V4 pools found for monitored tokens",
                checked_count
            );
        }

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
