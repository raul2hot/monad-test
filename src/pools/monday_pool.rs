use alloy::primitives::Bytes;
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;

use crate::config::PoolConfig;
use crate::pools::traits::{CallType, PriceCall};

// Monday Trade interface - trying V2-style getReserves
sol! {
    #[derive(Debug)]
    function getReserves() external view returns (
        uint112 reserve0,
        uint112 reserve1,
        uint32 blockTimestampLast
    );
}

/// Creates the calldata for getReserves() call
pub fn create_monday_reserves_call(pool: &PoolConfig) -> PriceCall {
    let calldata = getReservesCall {}.abi_encode();

    PriceCall {
        pool_name: pool.name.to_string(),
        pool_address: pool.address,
        calldata: Bytes::from(calldata),
        fee_bps: pool.fee_bps,
        call_type: CallType::MondayReserves,
    }
}

/// Decodes the getReserves response and calculates price
pub fn decode_reserves_to_price(data: &[u8]) -> Result<f64> {
    let decoded = getReservesCall::abi_decode_returns(data)?;

    // Convert from Uint<112> to f64 via string
    let reserve0: f64 = decoded.reserve0.to_string().parse().unwrap_or(0.0); // WMON (18 decimals)
    let reserve1: f64 = decoded.reserve1.to_string().parse().unwrap_or(0.0); // USDC (6 decimals)

    if reserve0 == 0.0 {
        return Ok(0.0);
    }

    // Price = reserve1 / reserve0
    // But need to adjust for decimals: WMON has 18, USDC has 6
    // So: price = (reserve1 / reserve0) * 10^(18-6) = (reserve1 / reserve0) * 10^12
    let price = (reserve1 / reserve0) * 1e12;

    Ok(price)
}
