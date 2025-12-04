use alloy::primitives::Bytes;
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;

use crate::config::PoolConfig;
use crate::pools::traits::{CallType, PriceCall};

// LFJ Liquidity Book interface
sol! {
    #[derive(Debug)]
    function getActiveId() external view returns (uint24 activeId);

    #[derive(Debug)]
    function getBinStep() external view returns (uint16 binStep);
}

/// Creates the calldata for getActiveId() call
pub fn create_lfj_active_id_call(pool: &PoolConfig) -> PriceCall {
    let calldata = getActiveIdCall {}.abi_encode();

    PriceCall {
        pool_name: pool.name.to_string(),
        pool_address: pool.address,
        calldata: Bytes::from(calldata),
        fee_bps: pool.fee_bps,
        call_type: CallType::LfjActiveId,
    }
}

/// Creates the calldata for getBinStep() call
pub fn create_lfj_bin_step_call(pool: &PoolConfig) -> PriceCall {
    let calldata = getBinStepCall {}.abi_encode();

    PriceCall {
        pool_name: format!("{}_binStep", pool.name),
        pool_address: pool.address,
        calldata: Bytes::from(calldata),
        fee_bps: pool.fee_bps,
        call_type: CallType::LfjBinStep,
    }
}

/// Decodes the getActiveId response
pub fn decode_active_id_response(data: &[u8]) -> Result<u32> {
    let decoded = getActiveIdCall::abi_decode_returns(data)?;
    // The decoded result is the Uint<24> directly for single return value
    let active_id: u32 = decoded.to::<u32>();
    Ok(active_id)
}

/// Decodes the getBinStep response
pub fn decode_bin_step_response(data: &[u8]) -> Result<u16> {
    let decoded = getBinStepCall::abi_decode_returns(data)?;
    // For u16, the result is the value directly
    Ok(decoded)
}

/// Calculate price from active bin ID and bin step
/// Formula: price = (1 + binStep/10_000)^(activeId - 8388608)
/// The result is in terms of token1/token0
/// For WMON/USDC: gives USDC per WMON
pub fn calculate_lfj_price(active_id: u32, bin_step: u16) -> f64 {
    // 8388608 is 2^23, the "zero" bin (price = 1)
    const ZERO_BIN: i64 = 8388608;

    let exponent = (active_id as i64) - ZERO_BIN;
    let base = 1.0 + (bin_step as f64 / 10_000.0);

    let raw_price = base.powi(exponent as i32);

    // Adjust for decimals: WMON(18) - USDC(6) = 12
    // For LFJ, if token0 is the lower address (WMON < USDC), price is token1/token0
    raw_price * 1e12
}
