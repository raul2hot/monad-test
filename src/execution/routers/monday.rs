use alloy::primitives::{Address, Bytes, U256, U160, Uint};
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;

// Monday Trade uses V3-style interface
sol! {
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

    #[derive(Debug)]
    function exactInputSingle(ExactInputSingleParams calldata params)
        external
        payable
        returns (uint256 amountOut);
}

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
        sqrtPriceLimitX96: U160::ZERO,
    };

    let calldata = exactInputSingleCall { params }.abi_encode();
    Ok(Bytes::from(calldata))
}
