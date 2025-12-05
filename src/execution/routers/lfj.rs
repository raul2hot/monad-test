use alloy::primitives::{Address, Bytes, U256};
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;

// LFJ Liquidity Book Router V2.2 Interface
// Note: LFJ uses a path-based routing system (NOT Uniswap V3 style)
//
// Version enum for LFJ pairs:
// - V1 = 0 (JoeV1 - legacy AMM)
// - V2 = 1 (Liquidity Book V2 - legacy)
// - V2_1 = 2 (Liquidity Book V2.1)
// - V2_2 = 3 (Liquidity Book V2.2 - CURRENT) <-- USE THIS
sol! {
    /// Path struct for routing swaps
    /// @param pairBinSteps The bin step for each pair (determines fee tier)
    /// @param versions The version for each pair (use 3 for V2_2)
    /// @param tokenPath The tokens to swap through [tokenIn, tokenOut]
    #[derive(Debug)]
    struct Path {
        uint256[] pairBinSteps;
        uint8[] versions;      // Use uint8 for Version enum
        address[] tokenPath;
    }

    /// Swaps exact tokens for tokens using Liquidity Book
    /// @param amountIn The amount of input tokens to swap
    /// @param amountOutMin The minimum amount of output tokens to receive
    /// @param path The Path struct containing routing information
    /// @param to The recipient address
    /// @param deadline Transaction deadline timestamp
    /// @return amountOut The amount of output tokens received
    #[derive(Debug)]
    function swapExactTokensForTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        Path memory path,
        address to,
        uint256 deadline
    ) external returns (uint256 amountOut);
}

// LFJ Bin Steps (Fee Tiers):
// | Bin Step | Fee    | Use Case             |
// |----------|--------|----------------------|
// | 1        | 0.01%  | Stablecoins          |
// | 5        | 0.05%  | Correlated assets    |
// | 10       | 0.10%  | Low volatility       |
// | 15       | 0.15%  | Medium volatility    |
// | 20       | 0.20%  | LIKELY WMON/USDC     |
// | 25       | 0.25%  | High volatility      |
// | 50       | 0.50%  | Very high volatility |
// | 100      | 1.00%  | Exotic pairs         |

/// Default bin step for WMON/USDC pool (try 15, 20, 25 if this fails)
pub const DEFAULT_BIN_STEP: u64 = 20;

pub fn build_swap_exact_tokens_for_tokens(
    token_in: Address,
    token_out: Address,
    amount_in: U256,
    amount_out_min: U256,
    recipient: Address,
    deadline: u64,
    bin_step: u32,  // Bin step from pool config (e.g., 15 for WMON/USDC)
) -> Result<Bytes> {
    build_swap_with_bin_step(token_in, token_out, amount_in, amount_out_min, recipient, deadline, bin_step as u64)
}

/// Build swap calldata with custom bin step
pub fn build_swap_with_bin_step(
    token_in: Address,
    token_out: Address,
    amount_in: U256,
    amount_out_min: U256,
    recipient: Address,
    deadline: u64,
    bin_step: u64,
) -> Result<Bytes> {
    // For LFJ, we need to specify the path with proper version and bin step
    // Version 3 = V2_2 (Liquidity Book V2.2 - current version)
    let path = Path {
        pairBinSteps: vec![U256::from(bin_step)],
        versions: vec![3],  // V2_2 = 3 (current Liquidity Book version)
        tokenPath: vec![token_in, token_out],
    };

    let calldata = swapExactTokensForTokensCall {
        amountIn: amount_in,
        amountOutMin: amount_out_min,
        path,
        to: recipient,
        deadline: U256::from(deadline),
    }.abi_encode();

    Ok(Bytes::from(calldata))
}
