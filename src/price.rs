use alloy::primitives::U160;

use crate::config::{USDC_DECIMALS, WMON_DECIMALS};

/// Converts sqrtPriceX96 to human-readable price (USDC per WMON)
///
/// Formula:
/// price = (sqrtPriceX96 / 2^96)^2
///
/// For WMON/USDC where:
/// - WMON (token0) has 18 decimals
/// - USDC (token1) has 6 decimals
///
/// Token order: WMON (0x3bd...) < USDC (0x754...) by address
/// Therefore: token0 = WMON, token1 = USDC
/// sqrtPriceX96 represents: sqrt(token1/token0) = sqrt(USDC/WMON)
///
/// price_adjusted = price * 10^(token0_decimals - token1_decimals)
///                = price * 10^(18 - 6) = price * 10^12
pub fn sqrt_price_x96_to_price(sqrt_price_x96: U160) -> f64 {
    // Convert to f64 for calculation
    // sqrtPriceX96 is typically < 2^128, safe for f64
    let sqrt_price: f64 = sqrt_price_x96
        .to_string()
        .parse()
        .unwrap_or(0.0);

    // 2^96 as f64
    let q96: f64 = 2.0_f64.powi(96);

    // (sqrtPriceX96 / 2^96)^2
    let price_ratio = (sqrt_price / q96).powi(2);

    // Adjust for decimals: WMON(18) - USDC(6) = 12
    let decimal_adjustment = 10.0_f64.powi((WMON_DECIMALS as i32) - (USDC_DECIMALS as i32));

    price_ratio * decimal_adjustment
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::U160;

    #[test]
    fn test_sqrt_price_conversion() {
        // Example sqrtPriceX96 value for a ~$0.037 WMON price
        // This is an approximate test value
        let sqrt_price = U160::from(15_230_000_000_000_000_000_000_000_000_u128);
        let price = sqrt_price_x96_to_price(sqrt_price);

        // Price should be in a reasonable range for WMON/USDC
        assert!(price > 0.0 && price < 1.0, "Price {} is out of expected range", price);
    }

    #[test]
    fn test_zero_price() {
        let sqrt_price = U160::ZERO;
        let price = sqrt_price_x96_to_price(sqrt_price);
        assert_eq!(price, 0.0);
    }
}
