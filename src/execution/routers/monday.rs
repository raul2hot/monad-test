use alloy::primitives::{Address, Bytes, U256, U160, Uint};
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;

// Monday Trade SwapRouter Interface
// IMPORTANT: Monday Trade uses the ORIGINAL ISwapRouter (from v3-periphery),
// NOT SwapRouter02. The key difference is that deadline is INSIDE the struct.
sol! {
    /// ExactInputSingleParams for original ISwapRouter
    /// NOTE: deadline IS in the struct (unlike SwapRouter02)
    #[derive(Debug)]
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 deadline;           // <-- CRITICAL: deadline is HERE
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    /// Swaps exact input for maximum output
    #[derive(Debug)]
    function exactInputSingle(ExactInputSingleParams calldata params)
        external
        payable
        returns (uint256 amountOut);
}

/// Build swap calldata for Monday Trade router (original ISwapRouter style - deadline IN struct)
pub fn build_exact_input_single(
    token_in: Address,
    token_out: Address,
    fee: u32,
    recipient: Address,
    amount_in: U256,
    amount_out_min: U256,
    deadline: u64,
) -> Result<Bytes> {
    let fee_uint24: Uint<24, 1> = Uint::from(fee);

    let params = ExactInputSingleParams {
        tokenIn: token_in,
        tokenOut: token_out,
        fee: fee_uint24,
        recipient,
        deadline: U256::from(deadline),
        amountIn: amount_in,
        amountOutMinimum: amount_out_min,
        sqrtPriceLimitX96: U160::ZERO,  // No price limit
    };

    let calldata = exactInputSingleCall { params }.abi_encode();
    Ok(Bytes::from(calldata))
}
