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
    }
}

/// Compute PoolId from PoolKey using keccak256(abi.encode(PoolKey))
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

/// Discovered V4 pool info from Initialize events
#[derive(Debug, Clone)]
struct DiscoveredPool {
    pool_id: FixedBytes<32>,
    currency0: Address,
    currency1: Address,
    fee: u32,
    tick_spacing: i32,
    hooks: Address,
    #[allow(dead_code)]
    initial_sqrt_price: u128,
}

/// Uniswap V4 DEX client
/// Uses the singleton PoolManager pattern with StateView for queries
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

    /// Get pool state for a discovered pool
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
        // Look back ~50k blocks (reasonable range), query in chunks due to RPC limits
        let total_blocks_back: u64 = 50_000;
        let chunk_size: u64 = 10_000;
        let start_block = latest_block.saturating_sub(total_blocks_back);

        tracing::info!(
            "V4: Scanning Initialize events from block {} to {}",
            start_block,
            latest_block
        );

        let mut current_from = start_block;
        let mut total_logs = 0u64;

        while current_from < latest_block {
            let current_to = std::cmp::min(current_from + chunk_size - 1, latest_block);

            let filter = Filter::new()
                .address(self.pool_manager)
                .event_signature(IPoolManager::Initialize::SIGNATURE_HASH)
                .from_block(current_from)
                .to_block(current_to);

            if let Ok(logs) = self.provider.get_logs(&filter).await {
                total_logs += logs.len() as u64;
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

                            // Convert sqrtPriceX96 safely - it's a uint160 but may still overflow u128
                            // uint160 max is ~1.46e48 which exceeds u128 max (~3.4e38)
                            let sqrt_price_u256 = U256::from(event.sqrtPriceX96);
                            let initial_sqrt_price = if sqrt_price_u256 <= U256::from(u128::MAX) {
                                sqrt_price_u256.to::<u128>()
                            } else {
                                u128::MAX // Cap at u128::MAX for storage
                            };

                            discovered.push(DiscoveredPool {
                                pool_id,
                                currency0,
                                currency1,
                                fee,
                                tick_spacing,
                                hooks: event.hooks,
                                initial_sqrt_price,
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
            "V4: {} Initialize events found, {} safe pools with monitored tokens",
            total_logs,
            discovered.len()
        );

        Ok(discovered)
    }
}

/// Minimum liquidity threshold (1000 units with 18 decimals)
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

        tracing::info!(
            "V4: {} valid pools from {} discovered",
            pools.len(),
            discovered.len()
        );
        Ok(pools)
    }

    fn dex(&self) -> Dex {
        Dex::UniswapV4
    }
}
