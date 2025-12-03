//! Direct Pool Price Queries

use alloy::primitives::{Address, U160};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;
use std::str::FromStr;

sol! {
    function slot0() external view returns (
        uint160 sqrtPriceX96,
        int24 tick,
        uint16 observationIndex,
        uint16 observationCardinality,
        uint16 observationCardinalityNext,
        uint8 feeProtocol,
        bool unlocked
    );

    function getPool(
        address tokenA,
        address tokenB,
        uint24 fee
    ) external view returns (address pool);

    function liquidity() external view returns (uint128);

    // For PancakeSwap SmartRouter -> get factory address
    function factory() external view returns (address);
}

/// Convert sqrtPriceX96 to human readable price
/// MON (18 decimals) / USDC (6 decimals)
pub fn sqrt_price_to_mon_usdc(sqrt_price_x96: U160, token0_is_mon: bool) -> f64 {
    let sqrt_price: f64 = sqrt_price_x96.to_string().parse().unwrap_or(0.0);
    let q96: f64 = 2f64.powi(96);

    let ratio = sqrt_price / q96;
    let raw_price = ratio * ratio;

    // Adjust for decimals: MON=18, USDC=6, diff=12
    let decimal_adj = 10f64.powi(12);

    if token0_is_mon {
        // price = USDC/MON, we want MON in USDC terms
        raw_price * decimal_adj
    } else {
        // price = MON/USDC, invert
        if raw_price > 0.0 {
            decimal_adj / raw_price
        } else {
            0.0
        }
    }
}

/// Query pool price via slot0
pub async fn get_pool_price<P: Provider>(
    provider: &P,
    pool_address: &str,
    token0_is_mon: bool,
) -> Result<f64> {
    let pool = Address::from_str(pool_address)?;

    let call = slot0Call {};
    let tx = TransactionRequest::default()
        .to(pool)
        .input(call.abi_encode().into());

    let result = provider.call(tx).await?;
    let decoded = slot0Call::abi_decode_returns(&result)?;

    let price = sqrt_price_to_mon_usdc(decoded.sqrtPriceX96, token0_is_mon);
    Ok(price)
}

/// Discover pool address from factory
pub async fn discover_pool<P: Provider>(
    provider: &P,
    factory: &str,
    token_a: &str,
    token_b: &str,
    fee: u32,
) -> Result<Option<Address>> {
    let factory_addr = Address::from_str(factory)?;
    let token_a_addr = Address::from_str(token_a)?;
    let token_b_addr = Address::from_str(token_b)?;

    let call = getPoolCall {
        tokenA: token_a_addr,
        tokenB: token_b_addr,
        fee: fee.try_into()?,
    };

    let tx = TransactionRequest::default()
        .to(factory_addr)
        .input(call.abi_encode().into());

    let result = provider.call(tx).await?;
    let pool = getPoolCall::abi_decode_returns(&result)?;

    if pool == Address::ZERO {
        Ok(None)
    } else {
        Ok(Some(pool))
    }
}

/// Check if pool has liquidity
pub async fn has_liquidity<P: Provider>(provider: &P, pool: &str) -> Result<bool> {
    let pool_addr = Address::from_str(pool)?;

    let call = liquidityCall {};
    let tx = TransactionRequest::default()
        .to(pool_addr)
        .input(call.abi_encode().into());

    match provider.call(tx).await {
        Ok(result) => {
            let liq = liquidityCall::abi_decode_returns(&result)?;
            Ok(liq > 0)
        }
        Err(_) => Ok(false),
    }
}

/// Get PancakeSwap factory address from SmartRouter
pub async fn get_pancake_factory<P: Provider>(provider: &P) -> Result<Address> {
    let router = Address::from_str("0x21114915Ac6d5A2e156931e20B20b038dEd0Be7C")?;

    let call = factoryCall {};
    let tx = TransactionRequest::default()
        .to(router)
        .input(call.abi_encode().into());

    let result = provider.call(tx).await?;
    let factory = factoryCall::abi_decode_returns(&result)?;

    Ok(factory)
}
