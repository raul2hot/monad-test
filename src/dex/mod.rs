pub mod batch_client;
pub mod lfj;
pub mod pancakeswap;
pub mod uniswap_v3;
pub mod uniswap_v4;

use alloy::primitives::{Address, U256};
use async_trait::async_trait;

/// Enum representing supported DEXes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dex {
    UniswapV3,
    UniswapV4,
    PancakeSwapV3,
    LFJ,
    Kuru,
}

impl std::fmt::Display for Dex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Dex::UniswapV3 => write!(f, "Uniswap V3"),
            Dex::UniswapV4 => write!(f, "Uniswap V4"),
            Dex::PancakeSwapV3 => write!(f, "PancakeSwap V3"),
            Dex::LFJ => write!(f, "LFJ"),
            Dex::Kuru => write!(f, "Kuru"),
        }
    }
}

/// Represents a liquidity pool on a DEX
#[derive(Debug, Clone)]
pub struct Pool {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,             // Fee in hundredths of bip
    pub dex: Dex,
    pub liquidity: U256,      // Current liquidity
    pub sqrt_price_x96: U256, // Current price (sqrt(price) * 2^96)
    pub decimals0: u8,        // Decimals of token0
    pub decimals1: u8,        // Decimals of token1
}

impl Pool {
    /// Calculate the ADJUSTED price of token0 in terms of token1
    /// Uses U256 arithmetic throughout to prevent overflow
    /// Formula: price = (sqrtPriceX96 / 2^96)^2 * 10^(decimals0 - decimals1)
    pub fn price_0_to_1(&self) -> f64 {
        if self.sqrt_price_x96.is_zero() {
            return 0.0;
        }

        // For values that fit in u128, use simple path
        if self.sqrt_price_x96 <= U256::from(u128::MAX) {
            let sqrt_price = self.sqrt_price_x96.to::<u128>() as f64 / (2_f64.powi(96));
            let raw_price = sqrt_price * sqrt_price;
            let decimal_adjustment = 10_f64.powi(self.decimals0 as i32 - self.decimals1 as i32);
            return raw_price * decimal_adjustment;
        }

        // For large values, use high-precision path
        // Split into high and low parts to avoid overflow
        let sqrt_f64 = u256_to_f64_safe(self.sqrt_price_x96);
        let sqrt_price = sqrt_f64 / (2_f64.powi(96));
        let raw_price = sqrt_price * sqrt_price;
        let decimal_adjustment = 10_f64.powi(self.decimals0 as i32 - self.decimals1 as i32);
        raw_price * decimal_adjustment
    }

    /// Calculate the ADJUSTED price of token1 in terms of token0
    pub fn price_1_to_0(&self) -> f64 {
        let price = self.price_0_to_1();
        if price > 0.0 && price.is_finite() {
            1.0 / price
        } else {
            0.0
        }
    }

    /// Get effective price after fees (token0 -> token1)
    pub fn effective_price_0_to_1(&self) -> f64 {
        let fee_factor = 1.0 - (self.fee as f64 / 1_000_000.0);
        self.price_0_to_1() * fee_factor
    }

    /// Get effective price after fees (token1 -> token0)
    pub fn effective_price_1_to_0(&self) -> f64 {
        let fee_factor = 1.0 - (self.fee as f64 / 1_000_000.0);
        self.price_1_to_0() * fee_factor
    }

    /// Validate that the price is within reasonable bounds
    pub fn is_price_valid(&self) -> bool {
        let price = self.price_0_to_1();
        price > 0.0 && price.is_finite() && price < 1e18 && price > 1e-18
    }

    /// Check if pool has sufficient liquidity
    pub fn has_sufficient_liquidity(&self, min_liquidity: u128) -> bool {
        self.liquidity >= U256::from(min_liquidity)
    }

    /// Get liquidity as a normalized value
    pub fn liquidity_normalized(&self) -> f64 {
        u256_to_f64_safe(self.liquidity)
    }
}

/// Safely convert U256 to f64, handling values larger than u128::MAX
fn u256_to_f64_safe(value: U256) -> f64 {
    if value.is_zero() {
        return 0.0;
    }

    if value <= U256::from(u128::MAX) {
        return value.to::<u128>() as f64;
    }

    // For values > u128::MAX, use bit manipulation
    // Find the most significant 64 bits and the exponent
    let bits = 256 - value.leading_zeros();
    let shift = bits.saturating_sub(64);
    let mantissa = (value >> shift).to::<u64>() as f64;
    mantissa * 2_f64.powi(shift as i32)
}

/// Trait for DEX clients to implement
#[async_trait]
pub trait DexClient: Send + Sync {
    /// Get all pools for given token pairs
    async fn get_pools(&self, tokens: &[Address]) -> eyre::Result<Vec<Pool>>;

    /// Get the DEX identifier
    fn dex(&self) -> Dex;
}
