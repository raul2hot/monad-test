use alloy::{
    primitives::{Address, U256},
    providers::Provider,
    sol,
};
use async_trait::async_trait;

use super::{Dex, DexClient, Pool};
use crate::config::contracts::lfj::LB_FACTORY;
use crate::config::thresholds;
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

        /// Get static fee parameters for accurate fee calculation
        /// Returns: baseFactor, filterPeriod, decayPeriod, reductionFactor, variableFeeControl, protocolShare, maxVolatilityAccumulator
        function getStaticFeeParameters() external view returns (
            uint16 baseFactor,
            uint16 filterPeriod,
            uint16 decayPeriod,
            uint16 reductionFactor,
            uint24 variableFeeControl,
            uint16 protocolShare,
            uint24 maxVolatilityAccumulator
        );

        /// Get the total fee (base + variable) for a swap
        /// Returns fee in basis points (1e4 = 100%)
        function getSwapOut(uint128 amountIn, bool swapForY) external view returns (
            uint128 amountInLeft,
            uint128 amountOut,
            uint128 fee
        );
    }
}

/// Common bin steps used in LFJ (Liquidity Book)
/// Bin step determines the price increment between bins (in basis points)
const LB_BIN_STEPS: [u32; 6] = [1, 2, 5, 10, 15, 20];

/// Test amount for getting realistic quote-based prices (1 WMON = 1e18)
/// Using a smaller amount to ensure we don't run into liquidity issues during discovery
const QUOTE_TEST_AMOUNT: u128 = 1_000_000_000_000_000_000; // 1 token with 18 decimals

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

    /// Get quote-based exchange rate by calling getSwapOut on the pair contract.
    /// This gives a realistic price that accounts for actual liquidity in the pool.
    /// Returns (rate, fee) where rate = amount_out / amount_in (decimal-adjusted)
    async fn get_quote_based_rate(
        &self,
        pair_address: Address,
        token_x: Address,
        token_y: Address,
        swap_for_y: bool,
    ) -> eyre::Result<Option<(f64, u128)>> {
        let pair_contract = ILBPair::new(pair_address, &self.provider);

        // Determine decimals for the input token
        let (token_in, token_out) = if swap_for_y {
            (token_x, token_y)
        } else {
            (token_y, token_x)
        };

        let decimals_in = tokens::decimals(token_in);
        let decimals_out = tokens::decimals(token_out);

        // Adjust test amount based on token decimals
        // We want to test with approximately 1 unit of the token
        let test_amount: u128 = if decimals_in == 18 {
            QUOTE_TEST_AMOUNT
        } else {
            10u128.pow(decimals_in as u32)
        };

        // Call getSwapOut to get actual quote
        let result = match pair_contract
            .getSwapOut(test_amount, swap_for_y)
            .call()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::trace!("getSwapOut failed for quote: {}", e);
                return Ok(None);
            }
        };

        let amount_in_left = result.amountInLeft;
        let amount_out = result.amountOut;
        let fee = result.fee;

        // If no output or all input left, there's no liquidity in this direction
        if amount_out == 0 || amount_in_left == test_amount {
            tracing::trace!(
                "No liquidity for swap_for_y={}: amountInLeft={}, amountOut={}",
                swap_for_y, amount_in_left, amount_out
            );
            return Ok(None);
        }

        // Calculate the actual amount that was swapped
        let actual_amount_in = test_amount - amount_in_left;

        // Reject if partial fill lost too much (less than 50% swapped)
        if actual_amount_in < test_amount / 2 {
            tracing::trace!(
                "Partial fill too severe: only {} of {} swapped",
                actual_amount_in, test_amount
            );
            return Ok(None);
        }

        // Calculate rate with decimal adjustment
        // rate = (amount_out / 10^decimals_out) / (actual_amount_in / 10^decimals_in)
        //      = amount_out * 10^decimals_in / (actual_amount_in * 10^decimals_out)
        let rate = if decimals_in >= decimals_out {
            let scale = 10u128.pow((decimals_in - decimals_out) as u32);
            (amount_out as f64 * scale as f64) / actual_amount_in as f64
        } else {
            let scale = 10u128.pow((decimals_out - decimals_in) as u32);
            amount_out as f64 / (actual_amount_in as f64 * scale as f64)
        };

        // Sanity check: rate should be positive and reasonable
        if rate <= 0.0 || !rate.is_finite() || rate > 1e18 || rate < 1e-18 {
            tracing::trace!("Quote rate out of bounds: {}", rate);
            return Ok(None);
        }

        Ok(Some((rate, fee as u128)))
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

        // Get the actual tokenX and tokenY from the pair
        // LFJ uses tokenX < tokenY ordering (by address)
        let token_x = pair_contract.getTokenX().call().await?;
        let token_y = pair_contract.getTokenY().call().await?;

        // Get reserves and active bin
        let reserves = pair_contract.getReserves().call().await?;
        let _active_id: u32 = pair_contract.getActiveId().call().await?.to();

        // Get fee parameters from contract
        let fee = match pair_contract.getStaticFeeParameters().call().await {
            Ok(params) => {
                // baseFee = baseFactor * binStep / 10000 (result in basis points)
                // We need it in hundredths of bips for consistency with V3
                let base_fee_bps = (params.baseFactor as u32 * bin_step) / 10_000;
                // Convert to hundredths of bips (multiply by 100)
                base_fee_bps * 100
            }
            Err(e) => {
                tracing::debug!("Failed to get LFJ fee params for {}: {}, using fallback", pair_address, e);
                // LFJ default: bin_step as basis points * 100 = hundredths of bips
                bin_step * 100
            }
        };

        // Get decimals for all tokens
        let decimals0 = tokens::decimals(token0);
        let decimals1 = tokens::decimals(token1);
        let decimals_x = tokens::decimals(token_x);
        let decimals_y = tokens::decimals(token_y);

        // CRITICAL FIX: Use getSwapOut to get REALISTIC quote-based prices
        // instead of theoretical prices from getPriceFromId.
        // This accounts for actual liquidity and prevents false positive arbitrage detection.
        //
        // We call getSwapOut for the X→Y direction and derive the price from actual quotes.
        let quote_based_price = match self.get_quote_based_rate(pair_address, token_x, token_y, true).await {
            Ok(Some((rate_x_to_y, _fee))) => {
                // rate_x_to_y is the decimal-adjusted rate: how many Y per 1 X
                // We need price_0_to_1 = how many token1 per 1 token0

                if token_x == token0 {
                    // tokenX == token0, so rate X→Y = rate 0→1 = price_0_to_1
                    Some(rate_x_to_y)
                } else {
                    // tokenX == token1, so rate X→Y = rate 1→0
                    // price_0_to_1 = 1 / rate_1_to_0
                    if rate_x_to_y > 0.0 {
                        Some(1.0 / rate_x_to_y)
                    } else {
                        None
                    }
                }
            }
            Ok(None) => {
                // No valid quote available - try the reverse direction
                match self.get_quote_based_rate(pair_address, token_x, token_y, false).await {
                    Ok(Some((rate_y_to_x, _fee))) => {
                        // rate_y_to_x is: how many X per 1 Y
                        if token_x == token0 {
                            // rate Y→X = rate 1→0, so price_0_to_1 = 1/rate
                            if rate_y_to_x > 0.0 {
                                Some(1.0 / rate_y_to_x)
                            } else {
                                None
                            }
                        } else {
                            // rate Y→X = rate 0→1 = price_0_to_1
                            Some(rate_y_to_x)
                        }
                    }
                    _ => None
                }
            }
            Err(e) => {
                tracing::trace!("Quote-based price failed for {}: {}", pair_address, e);
                None
            }
        };

        // Fallback to getPriceFromId if quote-based pricing failed
        let actual_price = match quote_based_price {
            Some(price) if price > 0.0 && price.is_finite() => {
                tracing::debug!(
                    "LFJ pool {} using quote-based price: {:.8}",
                    pair_address, price
                );
                price
            }
            _ => {
                // Fallback: use getPriceFromId (less accurate but still useful)
                let price_x128: U256 = pair_contract
                    .getPriceFromId(_active_id.try_into()?)
                    .call()
                    .await?;
                let lfj_price = q128_to_f64(price_x128);

                // Adjust for token ordering
                let fallback_price = if token_x == token0 {
                    lfj_price
                } else {
                    if lfj_price > 0.0 { 1.0 / lfj_price } else { 0.0 }
                };

                tracing::debug!(
                    "LFJ pool {} using fallback getPriceFromId: {:.8}",
                    pair_address, fallback_price
                );
                fallback_price
            }
        };

        // Sanity check: log if price seems unusual
        if actual_price <= 0.0 || !actual_price.is_finite() {
            tracing::warn!(
                "LFJ pool {} has invalid price: {} (tokenX={}, token0={})",
                pair_address,
                actual_price,
                token_x,
                token0
            );
        }

        // Convert to sqrtPriceX96 format for consistency with Uniswap V3
        // sqrtPriceX96 = sqrt(price) * 2^96
        // where price is the raw ratio (token1_units / token0_units)
        // Pool::price_0_to_1() will apply decimal adjustment
        let sqrt_price = actual_price.sqrt();
        let sqrt_price_x96 = (sqrt_price * 2_f64.powi(96)) as u128;

        // Normalize reserves to 18 decimals for consistent liquidity comparison
        let normalized_reserve_x = thresholds::normalize_to_18_decimals(
            U256::from(reserves.reserveX),
            decimals_x,
        );
        let normalized_reserve_y = thresholds::normalize_to_18_decimals(
            U256::from(reserves.reserveY),
            decimals_y,
        );
        let total_normalized_liquidity = normalized_reserve_x.saturating_add(normalized_reserve_y);

        Ok(Pool {
            address: pair_address,
            token0,
            token1,
            fee,
            dex: Dex::LFJ,
            liquidity: total_normalized_liquidity, // Now in normalized 18-decimal units
            sqrt_price_x96: U256::from(sqrt_price_x96),
            decimals0,
            decimals1,
        })
    }
}

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
                                // NOTE: pool.liquidity is now normalized to 18 decimals
                                if pool.liquidity > U256::ZERO
                                    && pool.is_price_valid()
                                    && pool.has_sufficient_liquidity_normalized(thresholds::MIN_NORMALIZED_LIQUIDITY)
                                {
                                    tracing::debug!(
                                        "Found valid LFJ pool: {} (bin_step: {}, normalized_liq: {}, price: {:.8})",
                                        pair_addr,
                                        bin_step,
                                        pool.liquidity,
                                        pool.price_0_to_1()
                                    );
                                    pools.push(pool);
                                } else {
                                    tracing::trace!(
                                        "Skipping LFJ pool {} - insufficient liquidity ({}) or invalid price (threshold: {})",
                                        pair_addr,
                                        pool.liquidity,
                                        thresholds::MIN_NORMALIZED_LIQUIDITY
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
