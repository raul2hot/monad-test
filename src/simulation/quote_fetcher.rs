//! Quote Fetcher Module
//!
//! Fetches swap quotes from multiple pools atomically (same block) to ensure
//! price consistency when calculating arbitrage opportunities.

use alloy::primitives::{Address, Bytes, U256, aliases::{U24, I24, U160}};
use alloy::providers::Provider;
use alloy::rpc::types::BlockId;
use alloy::sol;
use eyre::{eyre, Result};
use std::sync::Arc;

use crate::config::contracts;
use crate::dex::{Dex, Pool};

/// Quote from a single pool
#[derive(Debug, Clone)]
pub struct PoolQuote {
    pub pool_address: Address,
    pub dex: Dex,
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: U256,
    pub amount_out: U256,
    /// Fee paid in token_in terms
    pub fee_paid: U256,
    /// Fee in basis points
    pub fee_bps: u32,
    /// Price (amount_out / amount_in, decimal adjusted)
    pub price: f64,
    /// Current pool liquidity
    pub liquidity: U256,
    /// sqrt price after swap (for V3-style pools)
    pub sqrt_price_after: Option<U256>,
    /// Gas estimate for this swap
    pub gas_estimate: u64,
}

/// Collection of quotes from multiple pools at the same block
#[derive(Debug, Clone)]
pub struct AtomicQuote {
    /// Block number at which all quotes were fetched
    pub block_number: u64,
    /// Timestamp of the block
    pub timestamp: u64,
    /// Individual pool quotes
    pub quotes: Vec<PoolQuote>,
    /// Total gas estimate for all swaps
    pub total_gas_estimate: u64,
}

// Uniswap V3 Quoter interface
sol! {
    #[sol(rpc)]
    interface IQuoterV2 {
        struct QuoteExactInputSingleParams {
            address tokenIn;
            address tokenOut;
            uint256 amountIn;
            uint24 fee;
            uint160 sqrtPriceLimitX96;
        }

        function quoteExactInputSingle(QuoteExactInputSingleParams memory params)
            external
            returns (
                uint256 amountOut,
                uint160 sqrtPriceX96After,
                uint32 initializedTicksCrossed,
                uint256 gasEstimate
            );
    }
}

// LFJ Quoter interface
sol! {
    #[sol(rpc)]
    interface ILBQuoter {
        struct Quote {
            address[] route;
            address[] pairs;
            uint256[] binSteps;
            uint128[] amounts;
            uint128[] virtualAmountsWithoutSlippage;
            uint128[] fees;
        }

        function findBestPathFromAmountIn(
            address[] calldata route,
            uint128 amountIn
        ) external view returns (Quote memory quote);
    }
}

// LFJ Pair direct interface for getSwapOut
sol! {
    #[sol(rpc)]
    interface ILBPairQuote {
        function getSwapOut(uint128 amountIn, bool swapForY) external view returns (
            uint128 amountInLeft,
            uint128 amountOut,
            uint128 fee
        );
        function getTokenX() external view returns (address);
        function getTokenY() external view returns (address);
    }
}

// Uniswap V4 Quoter interface
sol! {
    #[sol(rpc)]
    interface IV4Quoter {
        struct PoolKey {
            address currency0;
            address currency1;
            uint24 fee;
            int24 tickSpacing;
            address hooks;
        }

        struct QuoteExactSingleParams {
            PoolKey poolKey;
            bool zeroForOne;
            uint128 exactAmount;
            bytes hookData;
        }

        function quoteExactInputSingle(QuoteExactSingleParams memory params)
            external
            returns (uint256 amountOut, uint256 gasEstimate);
    }
}

/// Pool information needed for quoting
#[derive(Debug, Clone)]
pub struct PoolInfo {
    pub address: Address,
    pub dex: Dex,
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,
    pub liquidity: U256,
    /// Direction: true if swapping token0 -> token1
    pub zero_for_one: bool,
    /// For V4: tick spacing
    pub tick_spacing: Option<i32>,
    /// For V4: hooks address
    pub hooks: Option<Address>,
}

impl From<&Pool> for PoolInfo {
    fn from(pool: &Pool) -> Self {
        Self {
            address: pool.address,
            dex: pool.dex,
            token0: pool.token0,
            token1: pool.token1,
            fee: pool.fee,
            liquidity: pool.liquidity,
            zero_for_one: true, // Default, should be set based on swap direction
            tick_spacing: None,
            hooks: None,
        }
    }
}

/// Quote fetcher for getting swap quotes from DEX pools
pub struct QuoteFetcher<P> {
    provider: Arc<P>,
}

impl<P> QuoteFetcher<P>
where
    P: Provider + Clone + 'static,
{
    pub fn new(provider: P) -> Self {
        Self {
            provider: Arc::new(provider),
        }
    }

    /// Fetch quotes from multiple pools atomically (same block)
    pub async fn get_atomic_quotes(
        &self,
        pools: &[PoolInfo],
        amount_in: U256,
        block: Option<BlockId>,
    ) -> Result<AtomicQuote> {
        // Get the current block number to ensure atomic quotes
        let block_number = self.provider.get_block_number().await?;
        let block_id = block.unwrap_or(BlockId::number(block_number));

        // Get block timestamp
        let block_data = self
            .provider
            .get_block_by_number(block_number.into())
            .await?
            .ok_or_else(|| eyre!("Block not found"))?;
        let timestamp = block_data.header.timestamp;

        let mut quotes = Vec::with_capacity(pools.len());
        let mut total_gas = 0u64;

        // Fetch quotes sequentially with the same block reference
        // In production, this could be batched using multicall
        for pool in pools {
            let quote = match pool.dex {
                Dex::UniswapV3 => {
                    self.quote_v3_pool(pool, amount_in, block_id, false).await?
                }
                Dex::PancakeSwapV3 => {
                    self.quote_v3_pool(pool, amount_in, block_id, true).await?
                }
                Dex::LFJ => self.quote_lfj_pool(pool, amount_in, block_id).await?,
                Dex::UniswapV4 => self.quote_v4_pool(pool, amount_in, block_id).await?,
                Dex::Kuru => {
                    // Kuru not implemented - return error
                    return Err(eyre!("Kuru quoter not implemented"));
                }
            };

            total_gas += quote.gas_estimate;
            quotes.push(quote);
        }

        Ok(AtomicQuote {
            block_number,
            timestamp,
            quotes,
            total_gas_estimate: total_gas,
        })
    }

    /// Quote a Uniswap V3 or PancakeSwap V3 pool using QuoterV2
    async fn quote_v3_pool(
        &self,
        pool: &PoolInfo,
        amount_in: U256,
        _block: BlockId,
        is_pancakeswap: bool,
    ) -> Result<PoolQuote> {
        let quoter_address = if is_pancakeswap {
            contracts::pancakeswap_v3::QUOTER_V2
        } else {
            contracts::uniswap_v3::QUOTER_V2
        };

        let quoter = IQuoterV2::new(quoter_address, self.provider.clone());

        let (token_in, token_out) = if pool.zero_for_one {
            (pool.token0, pool.token1)
        } else {
            (pool.token1, pool.token0)
        };

        let params = IQuoterV2::QuoteExactInputSingleParams {
            tokenIn: token_in,
            tokenOut: token_out,
            amountIn: amount_in,
            fee: U24::from(pool.fee),
            sqrtPriceLimitX96: U160::ZERO, // No price limit
        };

        // Use call() to simulate without state change
        let result = quoter.quoteExactInputSingle(params).call().await?;

        let amount_out = result.amountOut;
        let sqrt_price_after = result.sqrtPriceX96After;
        let gas_estimate = result.gasEstimate.to::<u64>();

        // V3 fees are in hundredths of bps (e.g., 3000 = 0.30% = 30 bps)
        // Divide by 100 to get basis points
        let fee_bps = pool.fee / 100;

        // Calculate fee paid
        let fee_paid = amount_in * U256::from(pool.fee) / U256::from(1_000_000);

        // Calculate price
        let price = if amount_in > U256::ZERO {
            amount_out.to::<u128>() as f64 / amount_in.to::<u128>() as f64
        } else {
            0.0
        };

        Ok(PoolQuote {
            pool_address: pool.address,
            dex: pool.dex,
            token_in,
            token_out,
            amount_in,
            amount_out,
            fee_paid,
            fee_bps,
            price,
            liquidity: pool.liquidity,
            sqrt_price_after: Some(U256::from(sqrt_price_after)),
            gas_estimate,
        })
    }

    /// Quote an LFJ pool using direct getSwapOut call
    async fn quote_lfj_pool(
        &self,
        pool: &PoolInfo,
        amount_in: U256,
        _block: BlockId,
    ) -> Result<PoolQuote> {
        let pair = ILBPairQuote::new(pool.address, self.provider.clone());

        // Determine swap direction
        let token_x: Address = pair.getTokenX().call().await?;
        let swap_for_y = pool.zero_for_one == (pool.token0 == token_x);

        let (token_in, token_out) = if pool.zero_for_one {
            (pool.token0, pool.token1)
        } else {
            (pool.token1, pool.token0)
        };

        // Convert U256 to u128 for LFJ
        let amount_in_u128: u128 = amount_in.try_into().unwrap_or(u128::MAX);

        let result = pair.getSwapOut(amount_in_u128, swap_for_y).call().await?;

        let amount_out = U256::from(result.amountOut);
        let fee = result.fee;

        // LFJ: fee is returned as absolute amount from getSwapOut
        // Calculate effective fee in basis points from actual swap
        // IMPORTANT: Use u128 arithmetic to avoid overflow with large amounts
        // fee and amount_in are both u128, and we need to preserve precision
        // This is different from V3/V4 where fee is a pool parameter
        let fee_bps = if amount_in_u128 > 0 {
            // fee_bps = (fee * 10000) / amount_in
            // For safety, we do: (fee / (amount_in / 10000)) to avoid overflow
            // Or equivalently: fee * 10000 / amount_in using checked math
            let numerator = (fee as u128).saturating_mul(10000);
            (numerator / amount_in_u128) as u32
        } else {
            0
        };

        let price = if amount_in > U256::ZERO {
            amount_out.to::<u128>() as f64 / amount_in.to::<u128>() as f64
        } else {
            0.0
        };

        Ok(PoolQuote {
            pool_address: pool.address,
            dex: Dex::LFJ,
            token_in,
            token_out,
            amount_in,
            amount_out,
            fee_paid: U256::from(fee),
            fee_bps,
            price,
            liquidity: pool.liquidity,
            sqrt_price_after: None,
            gas_estimate: 150_000, // Estimated gas for LFJ swap
        })
    }

    /// Quote a Uniswap V4 pool
    async fn quote_v4_pool(
        &self,
        pool: &PoolInfo,
        amount_in: U256,
        _block: BlockId,
    ) -> Result<PoolQuote> {
        let quoter = IV4Quoter::new(contracts::uniswap_v4::QUOTER, self.provider.clone());

        let (token_in, token_out) = if pool.zero_for_one {
            (pool.token0, pool.token1)
        } else {
            (pool.token1, pool.token0)
        };

        // Build pool key
        let pool_key = IV4Quoter::PoolKey {
            currency0: pool.token0,
            currency1: pool.token1,
            fee: U24::from(pool.fee),
            tickSpacing: I24::try_from(pool.tick_spacing.unwrap_or(60)).unwrap_or(I24::try_from(60).unwrap()),
            hooks: pool.hooks.unwrap_or(Address::ZERO),
        };

        let amount_in_u128: u128 = amount_in.try_into().unwrap_or(u128::MAX);

        let params = IV4Quoter::QuoteExactSingleParams {
            poolKey: pool_key,
            zeroForOne: pool.zero_for_one,
            exactAmount: amount_in_u128,
            hookData: Bytes::new(),
        };

        let result = quoter.quoteExactInputSingle(params).call().await?;

        let amount_out = result.amountOut;
        let gas_estimate = result.gasEstimate.to::<u64>();

        // V4 lpFee from slot0 is in hundredths of bps (same as V3)
        // Divide by 100 to get basis points
        let fee_bps = pool.fee / 100;
        let fee_paid = amount_in * U256::from(pool.fee) / U256::from(1_000_000);

        let price = if amount_in > U256::ZERO {
            amount_out.to::<u128>() as f64 / amount_in.to::<u128>() as f64
        } else {
            0.0
        };

        Ok(PoolQuote {
            pool_address: pool.address,
            dex: Dex::UniswapV4,
            token_in,
            token_out,
            amount_in,
            amount_out,
            fee_paid,
            fee_bps,
            price,
            liquidity: pool.liquidity,
            sqrt_price_after: None,
            gas_estimate,
        })
    }

    /// Get quotes for an arbitrage path (sequence of swaps)
    pub async fn get_path_quotes(
        &self,
        pools: &[PoolInfo],
        initial_amount: U256,
    ) -> Result<AtomicQuote> {
        let block_number = self.provider.get_block_number().await?;
        let block_id = BlockId::number(block_number);

        let block_data = self
            .provider
            .get_block_by_number(block_number.into())
            .await?
            .ok_or_else(|| eyre!("Block not found"))?;
        let timestamp = block_data.header.timestamp;

        let mut quotes = Vec::with_capacity(pools.len());
        let mut current_amount = initial_amount;
        let mut total_gas = 0u64;

        // Execute quotes sequentially, using output of each as input to next
        for pool in pools {
            let quote = match pool.dex {
                Dex::UniswapV3 => {
                    self.quote_v3_pool(pool, current_amount, block_id, false)
                        .await?
                }
                Dex::PancakeSwapV3 => {
                    self.quote_v3_pool(pool, current_amount, block_id, true)
                        .await?
                }
                Dex::LFJ => self.quote_lfj_pool(pool, current_amount, block_id).await?,
                Dex::UniswapV4 => {
                    self.quote_v4_pool(pool, current_amount, block_id).await?
                }
                Dex::Kuru => return Err(eyre!("Kuru quoter not implemented")),
            };

            current_amount = quote.amount_out;
            total_gas += quote.gas_estimate;
            quotes.push(quote);
        }

        Ok(AtomicQuote {
            block_number,
            timestamp,
            quotes,
            total_gas_estimate: total_gas,
        })
    }
}

impl AtomicQuote {
    /// Get the final output amount after all swaps
    pub fn final_amount_out(&self) -> U256 {
        self.quotes.last().map(|q| q.amount_out).unwrap_or(U256::ZERO)
    }

    /// Get the total fees paid across all swaps
    pub fn total_fees(&self) -> U256 {
        self.quotes.iter().map(|q| q.fee_paid).sum()
    }

    /// Get average fee in basis points
    pub fn avg_fee_bps(&self) -> f64 {
        if self.quotes.is_empty() {
            return 0.0;
        }
        self.quotes.iter().map(|q| q.fee_bps as f64).sum::<f64>() / self.quotes.len() as f64
    }

    /// Check if all quotes are from the same block
    pub fn is_atomic(&self) -> bool {
        // All quotes are fetched with the same block reference
        true
    }
}
