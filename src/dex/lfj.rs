use alloy::{
    primitives::{Address, U256},
    providers::Provider,
    sol,
};
use async_trait::async_trait;

use super::{Dex, DexClient, Pool};
use crate::config::contracts::lfj::LB_FACTORY;
use crate::config::tokens;

// LFJ (TraderJoe) Liquidity Book Factory ABI
sol! {
    #[sol(rpc)]
    interface ILBFactory {
        /// Returns the LBPair address for the given token pair and bin step
        function getLBPairInformation(
            address tokenA,
            address tokenB,
            uint256 binStep
        ) external view returns (
            uint24 binStep_,
            address LBPair,
            bool createdByOwner,
            bool ignoredForRouting
        );

        /// Returns the number of LBPairs for the given token pair
        function getNumberOfLBPairs() external view returns (uint256);

        /// Get all bin steps available
        function getAllBinSteps() external view returns (uint256[] memory);
    }
}

// LFJ Liquidity Book Pair ABI
sol! {
    #[sol(rpc)]
    interface ILBPair {
        function getTokenX() external view returns (address);
        function getTokenY() external view returns (address);
        function getBinStep() external view returns (uint16);
        function getActiveId() external view returns (uint24);
        function getReserves() external view returns (uint128 reserveX, uint128 reserveY);

        /// Get the price from the active bin
        function getPriceFromId(uint24 id) external view returns (uint256);
    }
}

/// Common bin steps used in LFJ (Liquidity Book)
/// Bin step determines the price increment between bins (in basis points)
const LB_BIN_STEPS: [u32; 6] = [1, 2, 5, 10, 15, 20];

/// Convert LFJ Q128.128 price to f64 with proper precision
/// price_x128 = actual_price * 2^128
/// We need to divide by 2^128 to get actual_price
fn q128_to_f64(price_x128: U256) -> f64 {
    if price_x128.is_zero() {
        return 0.0;
    }

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

/// Convert U256 to f64 safely, handling values larger than u128::MAX
fn u256_to_f64(value: U256) -> f64 {
    // If the value fits in u128, use direct conversion
    if value <= U256::from(u128::MAX) {
        return value.to::<u128>() as f64;
    }

    // For larger values, use bit manipulation for precision
    let bits = 256 - value.leading_zeros();
    let shift = bits.saturating_sub(64);
    let mantissa = (value >> shift).to::<u64>() as f64;
    mantissa * 2_f64.powi(shift as i32)
}

/// LFJ (TraderJoe Liquidity Book) DEX client
pub struct LfjClient<P> {
    provider: P,
    factory: Address,
    bin_steps: Vec<u32>,
}

impl<P: Provider + Clone> LfjClient<P> {
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            factory: LB_FACTORY,
            bin_steps: LB_BIN_STEPS.to_vec(),
        }
    }

    async fn get_lb_pair(
        &self,
        token0: Address,
        token1: Address,
        bin_step: u32,
    ) -> eyre::Result<Option<Address>> {
        let factory = ILBFactory::new(self.factory, &self.provider);

        let pair_info = factory
            .getLBPairInformation(token0, token1, U256::from(bin_step))
            .call()
            .await?;

        if pair_info.LBPair == Address::ZERO {
            Ok(None)
        } else {
            Ok(Some(pair_info.LBPair))
        }
    }

    async fn get_pool_state(
        &self,
        pair_address: Address,
        token0: Address,
        token1: Address,
        bin_step: u32,
    ) -> eyre::Result<Pool> {
        let pair_contract = ILBPair::new(pair_address, &self.provider);

        // Get reserves and active bin
        let reserves = pair_contract.getReserves().call().await?;
        let active_id: u32 = pair_contract.getActiveId().call().await?.to();

        // Get price from active bin (this is a Q128.128 fixed point number)
        let price_x128: U256 = pair_contract
            .getPriceFromId(active_id.try_into()?)
            .call()
            .await?;

        // FIXED: Proper conversion from Q128.128 using dedicated function
        // price_x128 = actual_price * 2^128
        // q128_to_f64 handles the division by 2^128 with proper precision
        let actual_price = q128_to_f64(price_x128);

        // Get decimals for adjustment
        let decimals0 = tokens::decimals(token0);
        let decimals1 = tokens::decimals(token1);

        // Note: decimal_adjustment is calculated but LFJ already handles decimals in its price
        // The Pool::price_0_to_1() method will apply decimal adjustment using decimals0/decimals1
        let _decimal_adjustment = 10_f64.powi(decimals0 as i32 - decimals1 as i32);

        // Convert to sqrtPriceX96 format for consistency with Uniswap
        // But since Pool::price_0_to_1() will apply decimal adjustment again,
        // we store the raw sqrtPriceX96 and let the Pool methods handle decimals
        let sqrt_price = actual_price.sqrt();
        let sqrt_price_x96 = (sqrt_price * 2_f64.powi(96)) as u128;

        // Total liquidity is approximated from reserves
        let total_liquidity = U256::from(reserves.reserveX) + U256::from(reserves.reserveY);

        // LFJ fee is bin_step * 0.01% (e.g., bin_step=10 means 0.1% fee)
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
}

/// Minimum liquidity threshold (1000 units with 18 decimals)
const MIN_LIQUIDITY: u128 = 1000 * 10u128.pow(18);

#[async_trait]
impl<P: Provider + Clone + Send + Sync> DexClient for LfjClient<P> {
    async fn get_pools(&self, tokens: &[Address]) -> eyre::Result<Vec<Pool>> {
        let mut pools = Vec::new();

        // Check all token pairs across all bin steps
        for i in 0..tokens.len() {
            for j in (i + 1)..tokens.len() {
                let (token0, token1) = if tokens[i] < tokens[j] {
                    (tokens[i], tokens[j])
                } else {
                    (tokens[j], tokens[i])
                };

                for &bin_step in &self.bin_steps {
                    if let Ok(Some(pair_addr)) = self.get_lb_pair(token0, token1, bin_step).await {
                        match self.get_pool_state(pair_addr, token0, token1, bin_step).await {
                            Ok(pool) => {
                                // Multiple validity checks: liquidity, price validity, and minimum threshold
                                if pool.liquidity > U256::ZERO
                                    && pool.is_price_valid()
                                    && pool.has_sufficient_liquidity(MIN_LIQUIDITY)
                                {
                                    tracing::debug!(
                                        "Found valid LFJ pool: {} (bin_step: {}, liq: {}, price: {:.8})",
                                        pair_addr,
                                        bin_step,
                                        pool.liquidity,
                                        pool.price_0_to_1()
                                    );
                                    pools.push(pool);
                                } else {
                                    tracing::trace!(
                                        "Skipping LFJ pool {} - insufficient liquidity or invalid price",
                                        pair_addr
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::debug!(
                                    "Failed to get LFJ pool state for {}: {}",
                                    pair_addr,
                                    e
                                );
                            }
                        }
                    }
                }
            }
        }

        Ok(pools)
    }

    fn dex(&self) -> Dex {
        Dex::LFJ
    }
}
