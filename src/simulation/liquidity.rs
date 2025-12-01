//! Liquidity Depth Analysis Module
//!
//! Analyzes pool liquidity to determine maximum viable trade sizes
//! and estimate price impact for given trade amounts.

use alloy::primitives::{Address, U256, aliases::U24};
use alloy::providers::Provider;
use alloy::sol;
use eyre::Result;
use std::sync::Arc;

use crate::dex::Dex;

/// Liquidity information for a pool
#[derive(Debug, Clone)]
pub struct LiquidityInfo {
    pub pool_address: Address,
    pub dex: Dex,
    /// Total liquidity in the pool (raw value)
    pub total_liquidity: U256,
    /// Estimated liquidity in USD
    pub liquidity_usd: f64,
    /// Maximum trade size for <0.5% price impact
    pub max_trade_05pct_slippage: U256,
    /// Maximum trade size for <1% price impact
    pub max_trade_1pct_slippage: U256,
    /// Maximum trade size for <2% price impact
    pub max_trade_2pct_slippage: U256,
    /// Current tick (for V3-style pools)
    pub current_tick: Option<i32>,
    /// Active bin ID (for LFJ pools)
    pub active_bin_id: Option<u32>,
}

// Uniswap V3 Pool interface for liquidity queries
sol! {
    #[sol(rpc)]
    interface IUniswapV3PoolLiquidity {
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
        function tickSpacing() external view returns (int24);
        function ticks(int24 tick) external view returns (
            uint128 liquidityGross,
            int128 liquidityNet,
            uint256 feeGrowthOutside0X128,
            uint256 feeGrowthOutside1X128,
            int56 tickCumulativeOutside,
            uint160 secondsPerLiquidityOutsideX128,
            uint32 secondsOutside,
            bool initialized
        );
    }
}

// LFJ Pool interface for liquidity queries
sol! {
    #[sol(rpc)]
    interface ILBPairLiquidity {
        function getActiveId() external view returns (uint24);
        function getBin(uint24 id) external view returns (uint128 binReserveX, uint128 binReserveY);
        function getReserves() external view returns (uint128 reserveX, uint128 reserveY);
    }
}

// Uniswap V4 StateView for liquidity queries
sol! {
    #[sol(rpc)]
    interface IStateViewLiquidity {
        function getLiquidity(bytes32 poolId) external view returns (uint128);
        function getSlot0(bytes32 poolId) external view returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint24 protocolFee,
            uint24 lpFee
        );
    }
}

/// Liquidity analyzer for determining trade viability
pub struct LiquidityAnalyzer<P> {
    provider: Arc<P>,
}

impl<P> LiquidityAnalyzer<P>
where
    P: Provider + Clone + 'static,
{
    pub fn new(provider: P) -> Self {
        Self {
            provider: Arc::new(provider),
        }
    }

    /// Get liquidity info for a V3-style pool (Uniswap V3, PancakeSwap V3)
    pub async fn get_v3_liquidity(&self, pool_address: Address, dex: Dex) -> Result<LiquidityInfo> {
        let pool = IUniswapV3PoolLiquidity::new(pool_address, self.provider.clone());

        // Get current liquidity
        let liquidity: u128 = pool.liquidity().call().await?;
        let slot0 = pool.slot0().call().await?;

        let total_liquidity = U256::from(liquidity);

        // Estimate max trade sizes based on liquidity
        // Rule of thumb: trade < X% of liquidity for <Y% slippage
        // These are conservative estimates; actual slippage depends on tick distribution
        let max_trade_05pct = total_liquidity / U256::from(200); // 0.5% of liquidity
        let max_trade_1pct = total_liquidity / U256::from(100);  // 1% of liquidity
        let max_trade_2pct = total_liquidity / U256::from(50);   // 2% of liquidity

        // Convert Signed<24, 1> tick to i32
        let tick_i32: i32 = slot0.tick.as_i32();

        Ok(LiquidityInfo {
            pool_address,
            dex,
            total_liquidity,
            liquidity_usd: 0.0, // Would need price oracle to calculate
            max_trade_05pct_slippage: max_trade_05pct,
            max_trade_1pct_slippage: max_trade_1pct,
            max_trade_2pct_slippage: max_trade_2pct,
            current_tick: Some(tick_i32),
            active_bin_id: None,
        })
    }

    /// Get liquidity info for an LFJ pool
    pub async fn get_lfj_liquidity(&self, pool_address: Address) -> Result<LiquidityInfo> {
        let pool = ILBPairLiquidity::new(pool_address, self.provider.clone());

        // Get active bin ID - U24 type
        let active_id: u32 = pool.getActiveId().call().await?.to::<u32>();

        // Get reserves from active bin and neighboring bins
        let mut total_reserve_x = U256::ZERO;
        let mut total_reserve_y = U256::ZERO;

        // Check active bin and 10 bins on each side (total 21 bins)
        // This covers most typical trading scenarios
        for offset in -10i32..=10 {
            let bin_id = (active_id as i32 + offset) as u32;
            if let Ok(bin) = pool.getBin(U24::from(bin_id)).call().await {
                total_reserve_x += U256::from(bin.binReserveX);
                total_reserve_y += U256::from(bin.binReserveY);
            }
        }

        let total_liquidity = total_reserve_x + total_reserve_y;

        // LFJ has better slippage characteristics due to concentrated liquidity
        // More aggressive estimates than V3
        let max_trade_05pct = total_liquidity / U256::from(100); // 1% of liquidity for 0.5% slippage
        let max_trade_1pct = total_liquidity / U256::from(50);   // 2% of liquidity for 1% slippage
        let max_trade_2pct = total_liquidity / U256::from(25);   // 4% of liquidity for 2% slippage

        Ok(LiquidityInfo {
            pool_address,
            dex: Dex::LFJ,
            total_liquidity,
            liquidity_usd: 0.0,
            max_trade_05pct_slippage: max_trade_05pct,
            max_trade_1pct_slippage: max_trade_1pct,
            max_trade_2pct_slippage: max_trade_2pct,
            current_tick: None,
            active_bin_id: Some(active_id),
        })
    }

    /// Get liquidity info for a V4 pool
    pub async fn get_v4_liquidity(
        &self,
        _pool_address: Address,
        pool_id: [u8; 32],
    ) -> Result<LiquidityInfo> {
        use crate::config::contracts::uniswap_v4;

        let state_view = IStateViewLiquidity::new(uniswap_v4::STATE_VIEW, self.provider.clone());

        let liquidity: u128 = state_view.getLiquidity(pool_id.into()).call().await?;
        let slot0 = state_view.getSlot0(pool_id.into()).call().await?;

        let total_liquidity = U256::from(liquidity);

        // Similar estimates to V3
        let max_trade_05pct = total_liquidity / U256::from(200);
        let max_trade_1pct = total_liquidity / U256::from(100);
        let max_trade_2pct = total_liquidity / U256::from(50);

        // Convert Signed<24, 1> tick to i32
        let tick_i32: i32 = slot0.tick.as_i32();

        Ok(LiquidityInfo {
            pool_address: Address::ZERO, // V4 uses pool IDs, not addresses
            dex: Dex::UniswapV4,
            total_liquidity,
            liquidity_usd: 0.0,
            max_trade_05pct_slippage: max_trade_05pct,
            max_trade_1pct_slippage: max_trade_1pct,
            max_trade_2pct_slippage: max_trade_2pct,
            current_tick: Some(tick_i32),
            active_bin_id: None,
        })
    }

    /// Get liquidity info based on DEX type
    pub async fn get_liquidity(
        &self,
        pool_address: Address,
        dex: Dex,
        pool_id: Option<[u8; 32]>,
    ) -> Result<LiquidityInfo> {
        match dex {
            Dex::UniswapV3 => self.get_v3_liquidity(pool_address, Dex::UniswapV3).await,
            Dex::PancakeSwapV3 => self.get_v3_liquidity(pool_address, Dex::PancakeSwapV3).await,
            Dex::LFJ => self.get_lfj_liquidity(pool_address).await,
            Dex::UniswapV4 => {
                if let Some(id) = pool_id {
                    self.get_v4_liquidity(pool_address, id).await
                } else {
                    // Fallback to V3-style query if no pool ID
                    self.get_v3_liquidity(pool_address, Dex::UniswapV4).await
                }
            }
            Dex::Kuru => {
                // Kuru not implemented - return empty liquidity
                Ok(LiquidityInfo {
                    pool_address,
                    dex: Dex::Kuru,
                    total_liquidity: U256::ZERO,
                    liquidity_usd: 0.0,
                    max_trade_05pct_slippage: U256::ZERO,
                    max_trade_1pct_slippage: U256::ZERO,
                    max_trade_2pct_slippage: U256::ZERO,
                    current_tick: None,
                    active_bin_id: None,
                })
            }
        }
    }

    /// Calculate the minimum liquidity across a path of pools
    pub async fn get_path_min_liquidity(
        &self,
        pools: &[(Address, Dex)],
    ) -> Result<U256> {
        let mut min_liquidity = U256::MAX;

        for (address, dex) in pools {
            let info = self.get_liquidity(*address, *dex, None).await?;
            if info.total_liquidity < min_liquidity {
                min_liquidity = info.total_liquidity;
            }
        }

        Ok(min_liquidity)
    }

    /// Estimate price impact for a given trade size
    /// Returns estimated slippage in basis points
    pub fn estimate_price_impact(&self, trade_size: U256, liquidity: &LiquidityInfo) -> u32 {
        if liquidity.total_liquidity.is_zero() {
            return 10000; // 100% slippage if no liquidity
        }

        // Estimate based on trade size relative to liquidity
        let ratio = trade_size
            .checked_mul(U256::from(10000))
            .and_then(|n| n.checked_div(liquidity.total_liquidity))
            .map(|r| r.to::<u32>())
            .unwrap_or(10000);

        // Apply a multiplier based on DEX type
        // LFJ generally has better slippage due to bin-based liquidity
        let multiplier = match liquidity.dex {
            Dex::LFJ => 50,       // Lower multiplier for LFJ
            Dex::UniswapV4 => 75, // V4 might have better execution
            _ => 100,             // Standard for V3-style
        };

        (ratio * multiplier / 100).min(10000)
    }

    /// Check if a trade size is viable for a given slippage tolerance
    pub fn is_trade_viable(
        &self,
        trade_size: U256,
        liquidity: &LiquidityInfo,
        max_slippage_bps: u32,
    ) -> bool {
        let estimated_slippage = self.estimate_price_impact(trade_size, liquidity);
        estimated_slippage <= max_slippage_bps
    }

    /// Get the recommended trade size for a given slippage target
    pub fn recommended_trade_size(&self, liquidity: &LiquidityInfo, target_slippage_bps: u32) -> U256 {
        match target_slippage_bps {
            0..=50 => liquidity.max_trade_05pct_slippage,
            51..=100 => liquidity.max_trade_1pct_slippage,
            _ => liquidity.max_trade_2pct_slippage,
        }
    }
}

impl LiquidityInfo {
    /// Get a human-readable description of liquidity
    pub fn description(&self) -> String {
        let liquidity_str = if self.total_liquidity > U256::from(10u128.pow(24)) {
            format!("{}M", self.total_liquidity / U256::from(10u128.pow(24)))
        } else if self.total_liquidity > U256::from(10u128.pow(21)) {
            format!("{}K", self.total_liquidity / U256::from(10u128.pow(21)))
        } else {
            format!("{}", self.total_liquidity / U256::from(10u128.pow(18)))
        };

        format!(
            "{} liquidity (max 1% slippage trade: {})",
            liquidity_str,
            self.max_trade_1pct_slippage / U256::from(10u128.pow(18))
        )
    }

    /// Check if pool has sufficient liquidity for a minimum trade
    pub fn has_sufficient_liquidity(&self, min_liquidity: U256) -> bool {
        self.total_liquidity >= min_liquidity
    }
}
