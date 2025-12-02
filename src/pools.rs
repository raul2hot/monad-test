//! Pool discovery utilities

use alloy::primitives::Address;
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;

// Uniswap V3 Factory getPool function
sol! {
    function getPool(
        address tokenA,
        address tokenB,
        uint24 fee
    ) external view returns (address pool);
}

// Uniswap V3 pool liquidity function
sol! {
    function liquidity() external view returns (uint128);
}

/// Fee tiers for Uniswap V3
pub const FEE_TIERS: [u32; 4] = [100, 500, 3000, 10000];

/// Discover Uniswap V3 pool address for a token pair
pub async fn find_uniswap_v3_pool<P: Provider>(
    provider: &P,
    factory: Address,
    token_a: Address,
    token_b: Address,
) -> Result<Option<(Address, u32)>> {
    for fee in FEE_TIERS {
        let call = getPoolCall {
            tokenA: token_a,
            tokenB: token_b,
            fee: fee.try_into().unwrap(),
        };

        let tx = TransactionRequest::default()
            .to(factory)
            .input(call.abi_encode().into());

        if let Ok(result) = provider.call(tx).await {
            if let Ok(decoded) = getPoolCall::abi_decode_returns(&result) {
                // decoded is the return value (address pool)
                if decoded != Address::ZERO {
                    return Ok(Some((decoded, fee)));
                }
            }
        }
    }
    Ok(None)
}

/// Check if a pool has liquidity (non-zero reserves)
pub async fn pool_has_liquidity<P: Provider>(
    provider: &P,
    pool: Address,
) -> Result<bool> {
    let call = liquidityCall {};
    let tx = TransactionRequest::default()
        .to(pool)
        .input(call.abi_encode().into());

    match provider.call(tx).await {
        Ok(result) => {
            if let Ok(decoded) = liquidityCall::abi_decode_returns(&result) {
                // decoded is u128
                Ok(decoded > 0)
            } else {
                Ok(false)
            }
        }
        Err(_) => Ok(false),
    }
}

/// Discover all pools for a token pair and return the one with most liquidity
pub async fn find_best_uniswap_v3_pool<P: Provider>(
    provider: &P,
    factory: Address,
    token_a: Address,
    token_b: Address,
) -> Result<Option<(Address, u32, u128)>> {
    let mut best_pool: Option<(Address, u32, u128)> = None;

    for fee in FEE_TIERS {
        let call = getPoolCall {
            tokenA: token_a,
            tokenB: token_b,
            fee: fee.try_into().unwrap(),
        };

        let tx = TransactionRequest::default()
            .to(factory)
            .input(call.abi_encode().into());

        if let Ok(result) = provider.call(tx).await {
            if let Ok(pool_address) = getPoolCall::abi_decode_returns(&result) {
                if pool_address != Address::ZERO {
                    // Check liquidity
                    let liq_call = liquidityCall {};
                    let liq_tx = TransactionRequest::default()
                        .to(pool_address)
                        .input(liq_call.abi_encode().into());

                    if let Ok(liq_result) = provider.call(liq_tx).await {
                        if let Ok(liquidity) = liquidityCall::abi_decode_returns(&liq_result) {
                            if best_pool.is_none() || liquidity > best_pool.unwrap().2 {
                                best_pool = Some((pool_address, fee, liquidity));
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(best_pool)
}
