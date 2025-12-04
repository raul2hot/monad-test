use alloy::primitives::{Address, Bytes, U256};
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;

// LFJ LBRouter interface
// Note: LFJ uses a path-based routing system
sol! {
    #[derive(Debug)]
    struct Version {
        uint256 v;
    }

    #[derive(Debug)]
    struct Path {
        uint256[] pairBinSteps;
        uint8[] versions;
        address[] tokenPath;
    }

    #[derive(Debug)]
    function swapExactTokensForTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        Path memory path,
        address to,
        uint256 deadline
    ) external returns (uint256 amountOut);
}

pub fn build_swap_exact_tokens_for_tokens(
    token_in: Address,
    token_out: Address,
    amount_in: U256,
    amount_out_min: U256,
    recipient: Address,
    deadline: u64,
) -> Result<Bytes> {
    // For LFJ, we need to specify the path
    // binStep for WMON/USDC pool is typically 15 or 20 - this may need adjustment
    // Version 2 = V2.1 (Liquidity Book)
    let path = Path {
        pairBinSteps: vec![U256::from(15)],  // Bin step - may need to query from pool
        versions: vec![2],  // V2.1
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
