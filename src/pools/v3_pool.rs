use alloy::primitives::{Bytes, U160};
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;

use crate::config::PoolConfig;
use crate::pools::traits::{CallType, PriceCall};
use crate::price::sqrt_price_x96_to_price;

// Define the slot0 interface for V3 pools
sol! {
    #[derive(Debug)]
    function slot0() external view returns (
        uint160 sqrtPriceX96,
        int24 tick,
        uint16 observationIndex,
        uint16 observationCardinality,
        uint16 observationCardinalityNext,
        uint8 feeProtocol,
        bool unlocked
    );
}

/// Creates the calldata for slot0() call
pub fn create_slot0_call(pool: &PoolConfig) -> PriceCall {
    let calldata = slot0Call {}.abi_encode();

    PriceCall {
        pool_name: pool.name.to_string(),
        pool_address: pool.address,
        calldata: Bytes::from(calldata),
        fee_bps: pool.fee_bps,
        call_type: CallType::V3Slot0,
    }
}

/// Decodes the slot0 response and extracts sqrtPriceX96
pub fn decode_slot0_response(data: &[u8]) -> Result<U160> {
    let decoded = slot0Call::abi_decode_returns(data)?;
    Ok(decoded.sqrtPriceX96)
}

/// Decodes slot0 response and converts to price
pub fn decode_slot0_to_price(data: &[u8]) -> Result<f64> {
    let sqrt_price_x96 = decode_slot0_response(data)?;
    Ok(sqrt_price_x96_to_price(sqrt_price_x96))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::get_v3_pools;

    #[test]
    fn test_create_slot0_call() {
        let pools = get_v3_pools();
        let call = create_slot0_call(&pools[0]);

        // slot0() selector is 0x3850c7bd
        assert_eq!(&call.calldata[..4], &[0x38, 0x50, 0xc7, 0xbd]);
    }
}
