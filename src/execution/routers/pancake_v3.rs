use alloy::primitives::{Address, Bytes, U256, U160, Uint};
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;

sol! {
    // Inner swap params (no deadline in struct - SwapRouter02 style)
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

    // REQUIRED: Multicall wrapper with deadline
    #[derive(Debug)]
    function multicall(uint256 deadline, bytes[] calldata data)
        external
        payable
        returns (bytes[] memory results);
}

pub fn build_exact_input_single(
    token_in: Address,
    token_out: Address,
    fee: u32,
    recipient: Address,
    amount_in: U256,
    amount_out_min: U256,
    deadline: u64,  // ADD this parameter
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

    // Encode inner exactInputSingle call
    let inner_calldata = exactInputSingleCall { params }.abi_encode();
    
    // WRAP in multicall with deadline - THIS IS REQUIRED!
    let calldata = multicallCall {
        deadline: U256::from(deadline),
        data: vec![Bytes::from(inner_calldata)],
    }.abi_encode();

    Ok(Bytes::from(calldata))
}