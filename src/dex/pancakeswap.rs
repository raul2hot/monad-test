use alloy::{
    primitives::{Address, U256},
    providers::Provider,
    sol,
};
use async_trait::async_trait;

use super::{Dex, DexClient, Pool};
use crate::config::contracts::pancakeswap_v3::{FACTORY, FEE_TIERS};
use crate::config::thresholds;
use crate::config::tokens;

// PancakeSwap V3 Factory ABI (same interface as Uniswap V3)
sol! {
    #[sol(rpc)]
    interface IPancakeV3Factory {
        function getPool(address tokenA, address tokenB, uint24 fee) external view returns (address pool);
    }
}

// PancakeSwap V3 Pool ABI (same interface as Uniswap V3)
sol! {
    #[sol(rpc)]
    interface IPancakeV3Pool {
        function token0() external view returns (address);
        function token1() external view returns (address);
        function fee() external view returns (uint24);
        function liquidity() external view returns (uint128);
        function slot0() external view returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint16 observationIndex,
            uint16 observationCardinality,
            uint16 observationCardinalityNext,
            uint32 feeProtocol,
            bool unlocked
        );
    }
}

/// PancakeSwap V3 DEX client
pub struct PancakeSwapClient<P> {
    provider: P,
    factory: Address,
    fee_tiers: Vec<u32>,
}

impl<P: Provider + Clone> PancakeSwapClient<P> {
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            factory: FACTORY,
            fee_tiers: FEE_TIERS.to_vec(),
        }
    }

    async fn get_pool_address(
        &self,
        token0: Address,
        token1: Address,
        fee: u32,
    ) -> eyre::Result<Option<Address>> {
        let factory = IPancakeV3Factory::new(self.factory, &self.provider);
        let pool_addr: Address = factory
            .getPool(token0, token1, fee.try_into()?)
            .call()
            .await?;

        if pool_addr == Address::ZERO {
            Ok(None)
        } else {
            Ok(Some(pool_addr))
        }
    }

    async fn get_pool_state(
        &self,
        pool_address: Address,
        token0: Address,
        token1: Address,
        fee: u32,
    ) -> eyre::Result<Pool> {
        let pool_contract = IPancakeV3Pool::new(pool_address, &self.provider);

        let liquidity: u128 = pool_contract.liquidity().call().await?;
        let slot0 = pool_contract.slot0().call().await?;

        Ok(Pool {
            address: pool_address,
            token0,
            token1,
            fee,
            dex: Dex::PancakeSwapV3,
            liquidity: U256::from(liquidity),
            sqrt_price_x96: U256::from(slot0.sqrtPriceX96),
            decimals0: tokens::decimals(token0),
            decimals1: tokens::decimals(token1),
        })
    }
}

#[async_trait]
impl<P: Provider + Clone + Send + Sync> DexClient for PancakeSwapClient<P> {
    async fn get_pools(&self, tokens: &[Address]) -> eyre::Result<Vec<Pool>> {
        let mut pools = Vec::new();

        // Check all token pairs across all fee tiers
        for i in 0..tokens.len() {
            for j in (i + 1)..tokens.len() {
                let (token0, token1) = if tokens[i] < tokens[j] {
                    (tokens[i], tokens[j])
                } else {
                    (tokens[j], tokens[i])
                };

                for &fee in &self.fee_tiers {
                    if let Ok(Some(pool_addr)) = self.get_pool_address(token0, token1, fee).await {
                        match self.get_pool_state(pool_addr, token0, token1, fee).await {
                            Ok(pool) => {
                                // CRITICAL FIX: V3 liquidity value (L) is NOT comparable to reserve amounts
                                // The L parameter represents concentrated liquidity, not actual token reserves
                                // A low L doesn't necessarily mean a swap can't execute - the quoter is the
                                // only reliable way to validate swap feasibility for V3 pools.
                                //
                                // We use has_sufficient_liquidity_normalized which adjusts thresholds
                                // appropriately for V3's L value (divides by 1000).
                                if pool.liquidity > U256::ZERO
                                    && pool.is_price_valid()
                                    && pool.has_sufficient_liquidity_normalized(thresholds::MIN_NORMALIZED_LIQUIDITY)
                                {
                                    tracing::debug!(
                                        "Found valid PancakeSwap V3 pool: {} (fee: {}, L: {}, price: {:.8})",
                                        pool_addr,
                                        fee,
                                        pool.liquidity,
                                        pool.price_0_to_1()
                                    );
                                    pools.push(pool);
                                } else {
                                    tracing::trace!(
                                        "Skipping PancakeSwap V3 pool {} - L={} below threshold or invalid price (note: L is NOT reserves)",
                                        pool_addr,
                                        pool.liquidity
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::debug!(
                                    "Failed to get pool state for {}: {}",
                                    pool_addr,
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
        Dex::PancakeSwapV3
    }
}
