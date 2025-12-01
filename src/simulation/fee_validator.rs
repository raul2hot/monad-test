//! Fee Validator Module
//!
//! Queries actual fee tiers from pool contracts to ensure accurate profit calculations.
//! Fixes the suspected bug where Uniswap V3 fees (in hundredths of bps) were displayed incorrectly.

use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::sol;
use eyre::Result;
use std::sync::Arc;

use crate::dex::Dex;

/// Pool fee information retrieved from on-chain
#[derive(Debug, Clone)]
pub struct PoolFeeInfo {
    pub pool_address: Address,
    pub dex_type: Dex,
    /// Fee in basis points (e.g., 30 = 0.30%)
    pub fee_bps: u32,
    /// True if fee can change per-swap (e.g., V4 with hooks, LFJ dynamic fees)
    pub is_dynamic: bool,
    /// Raw fee value as returned from contract
    pub raw_fee: u32,
}

// Solidity interfaces for fee queries

// Uniswap V3 / PancakeSwap V3 Pool interface
sol! {
    #[sol(rpc)]
    interface IUniswapV3Pool {
        function fee() external view returns (uint24);
        function token0() external view returns (address);
        function token1() external view returns (address);
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
    }
}

// LFJ (Liquidity Book) Pool interface
sol! {
    #[sol(rpc)]
    interface ILBPair {
        function getTokenX() external view returns (address);
        function getTokenY() external view returns (address);
        function getActiveId() external view returns (uint24);
        function getBin(uint24 id) external view returns (uint128 binReserveX, uint128 binReserveY);
        function getStaticFeeParameters() external view returns (
            uint16 baseFactor,
            uint16 filterPeriod,
            uint16 decayPeriod,
            uint16 reductionFactor,
            uint24 variableFeeControl,
            uint16 protocolShare,
            uint24 maxVolatilityAccumulator
        );
        function getSwapOut(uint128 amountIn, bool swapForY) external view returns (
            uint128 amountInLeft,
            uint128 amountOut,
            uint128 fee
        );
    }
}

// Uniswap V4 StateView interface for reading pool data
sol! {
    #[sol(rpc)]
    interface IStateView {
        function getSlot0(bytes32 poolId) external view returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint24 protocolFee,
            uint24 lpFee
        );
        function getLiquidity(bytes32 poolId) external view returns (uint128);
    }
}

/// Fee validator for querying actual pool fees on-chain
pub struct FeeValidator<P> {
    provider: Arc<P>,
}

impl<P> FeeValidator<P>
where
    P: Provider + Clone + 'static,
{
    pub fn new(provider: P) -> Self {
        Self {
            provider: Arc::new(provider),
        }
    }

    /// Query the actual fee tier from a Uniswap V3 style pool
    ///
    /// IMPORTANT: V3 pools return fee in hundredths of bps (e.g., 3000 = 0.30% = 30 bps)
    /// This function converts to basis points for consistent usage.
    pub async fn get_v3_pool_fee(&self, pool_address: Address) -> Result<PoolFeeInfo> {
        let pool = IUniswapV3Pool::new(pool_address, self.provider.clone());

        // Call fee() - returns fee in hundredths of bps
        let raw_fee: u32 = pool.fee().call().await?.to();

        // Convert from hundredths of bps to bps
        // 100 -> 1 bps (0.01%)
        // 500 -> 5 bps (0.05%)
        // 3000 -> 30 bps (0.30%)
        // 10000 -> 100 bps (1.00%)
        let fee_bps = raw_fee / 100;

        Ok(PoolFeeInfo {
            pool_address,
            dex_type: Dex::UniswapV3,
            fee_bps,
            is_dynamic: false,
            raw_fee,
        })
    }

    /// Query fee from a PancakeSwap V3 pool (same interface as Uniswap V3)
    pub async fn get_pancakeswap_pool_fee(&self, pool_address: Address) -> Result<PoolFeeInfo> {
        let mut info = self.get_v3_pool_fee(pool_address).await?;
        info.dex_type = Dex::PancakeSwapV3;
        Ok(info)
    }

    /// Query fee from LFJ pool using getSwapOut to get actual fee for a trade
    ///
    /// LFJ uses dynamic fees based on volatility, so we simulate a trade to get the actual fee.
    pub async fn get_lfj_pool_fee(
        &self,
        pool_address: Address,
        amount_in: u128,
        swap_for_y: bool,
    ) -> Result<PoolFeeInfo> {
        let pool = ILBPair::new(pool_address, self.provider.clone());

        // Call getSwapOut to get the actual fee for this trade
        let result = pool.getSwapOut(amount_in, swap_for_y).call().await?;
        let fee = result.fee;

        // Calculate effective fee in bps
        // fee is the absolute amount of tokens taken as fee
        // effective_fee_bps = (fee / amount_in) * 10000
        let effective_fee_bps = if amount_in > 0 {
            ((fee as u64 * 10000) / (amount_in as u64)) as u32
        } else {
            0
        };

        Ok(PoolFeeInfo {
            pool_address,
            dex_type: Dex::LFJ,
            fee_bps: effective_fee_bps,
            is_dynamic: true, // LFJ fees can vary based on volatility
            raw_fee: effective_fee_bps * 100, // Store in hundredths of bps for consistency
        })
    }

    /// Query fee from LFJ pool using static fee parameters (faster, no trade simulation)
    pub async fn get_lfj_base_fee(&self, pool_address: Address) -> Result<PoolFeeInfo> {
        let pool = ILBPair::new(pool_address, self.provider.clone());

        // Get static fee parameters
        let params = pool.getStaticFeeParameters().call().await?;

        // Base fee calculation for LFJ
        // baseFactor is in 1e4 scale (10000 = 100%)
        // Typical base fee is around 15-50 bps
        let base_factor = params.baseFactor as u32;

        // Convert baseFactor to bps (approximate)
        // baseFactor of 5000 means ~50 bps base fee
        let fee_bps = base_factor / 100;

        Ok(PoolFeeInfo {
            pool_address,
            dex_type: Dex::LFJ,
            fee_bps,
            is_dynamic: true,
            raw_fee: base_factor,
        })
    }

    /// Get pool fee based on DEX type
    pub async fn get_pool_fee(
        &self,
        pool_address: Address,
        dex_type: Dex,
        amount_in: Option<u128>,
    ) -> Result<PoolFeeInfo> {
        match dex_type {
            Dex::UniswapV3 => self.get_v3_pool_fee(pool_address).await,
            Dex::PancakeSwapV3 => self.get_pancakeswap_pool_fee(pool_address).await,
            Dex::LFJ => {
                // For LFJ, prefer simulating with actual amount if provided
                if let Some(amount) = amount_in {
                    self.get_lfj_pool_fee(pool_address, amount, true).await
                } else {
                    self.get_lfj_base_fee(pool_address).await
                }
            }
            Dex::UniswapV4 => {
                // V4 fees are stored differently - need pool ID
                // For now, return a placeholder; will be enhanced in quote_fetcher
                Ok(PoolFeeInfo {
                    pool_address,
                    dex_type: Dex::UniswapV4,
                    fee_bps: 30, // Default assumption, will be overridden by actual quote
                    is_dynamic: true, // V4 can have dynamic fees via hooks
                    raw_fee: 3000,
                })
            }
            Dex::Kuru => {
                // Kuru not implemented yet
                Ok(PoolFeeInfo {
                    pool_address,
                    dex_type: Dex::Kuru,
                    fee_bps: 30,
                    is_dynamic: false,
                    raw_fee: 3000,
                })
            }
        }
    }

    /// Validate and correct fee from existing pool data
    ///
    /// This function takes a raw fee value (in hundredths of bps as stored in Pool struct)
    /// and converts it to proper basis points.
    pub fn convert_raw_fee_to_bps(raw_fee: u32, dex_type: Dex) -> u32 {
        match dex_type {
            // V3-style DEXes store fee in hundredths of bps
            Dex::UniswapV3 | Dex::PancakeSwapV3 | Dex::UniswapV4 => raw_fee / 100,
            // LFJ fees are already computed as effective fee
            Dex::LFJ => raw_fee,
            Dex::Kuru => raw_fee / 100,
        }
    }

    /// Get fee description string for logging
    pub fn fee_description(fee_bps: u32) -> String {
        format!("{} bps ({:.2}%)", fee_bps, fee_bps as f64 / 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fee_conversion() {
        // Test V3 fee conversion
        assert_eq!(FeeValidator::<()>::convert_raw_fee_to_bps(100, Dex::UniswapV3), 1);
        assert_eq!(FeeValidator::<()>::convert_raw_fee_to_bps(500, Dex::UniswapV3), 5);
        assert_eq!(FeeValidator::<()>::convert_raw_fee_to_bps(3000, Dex::UniswapV3), 30);
        assert_eq!(FeeValidator::<()>::convert_raw_fee_to_bps(10000, Dex::UniswapV3), 100);

        // Test PancakeSwap fee conversion
        assert_eq!(FeeValidator::<()>::convert_raw_fee_to_bps(2500, Dex::PancakeSwapV3), 25);
    }

    #[test]
    fn test_fee_description() {
        assert_eq!(FeeValidator::<()>::fee_description(30), "30 bps (0.30%)");
        assert_eq!(FeeValidator::<()>::fee_description(100), "100 bps (1.00%)");
        assert_eq!(FeeValidator::<()>::fee_description(5), "5 bps (0.05%)");
    }
}
