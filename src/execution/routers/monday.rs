use alloy::primitives::{Address, Bytes, U256, U160, Uint};
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;

// Monday Trade SwapRouter02 Interface
// Note: Monday Trade is built on SynFutures infrastructure and uses SwapRouter02,
// which does NOT have deadline inside the struct (deadline is checked separately via a modifier).
sol! {
    /// ExactInputSingleParams for SwapRouter02
    /// NOTE: No deadline field - deadline is handled by multicall wrapper
    #[derive(Debug)]
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
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

    /// Multicall with deadline (use this to wrap calls with deadline check)
    #[derive(Debug)]
    function multicall(uint256 deadline, bytes[] calldata data)
        external
        payable
        returns (bytes[] memory results);
}

/// Build swap calldata for Monday Trade router (SwapRouter02 style - no deadline in struct)
pub fn build_exact_input_single(
    token_in: Address,
    token_out: Address,
    fee: u32,
    recipient: Address,
    amount_in: U256,
    amount_out_min: U256,
) -> Result<Bytes> {
    let fee_uint24: Uint<24, 1> = Uint::from(fee);

    let params = ExactInputSingleParams {
        tokenIn: token_in,
        tokenOut: token_out,
        fee: fee_uint24,
        recipient,
        amountIn: amount_in,
        amountOutMinimum: amount_out_min,
        sqrtPriceLimitX96: U160::ZERO,  // No price limit
    };

    let calldata = exactInputSingleCall { params }.abi_encode();
    Ok(Bytes::from(calldata))
}

/// Build swap calldata wrapped with deadline using multicall
/// Use this if the direct call fails and you need deadline enforcement
pub fn build_exact_input_single_with_deadline(
    token_in: Address,
    token_out: Address,
    fee: u32,
    recipient: Address,
    amount_in: U256,
    amount_out_min: U256,
    deadline: u64,
) -> Result<Bytes> {
    // First, build the inner exactInputSingle call
    let inner_calldata = build_exact_input_single(
        token_in,
        token_out,
        fee,
        recipient,
        amount_in,
        amount_out_min,
    )?;

    // Wrap it in multicall with deadline
    let calldata = multicallCall {
        deadline: U256::from(deadline),
        data: vec![inner_calldata],
    }
    .abi_encode();

    Ok(Bytes::from(calldata))
}
