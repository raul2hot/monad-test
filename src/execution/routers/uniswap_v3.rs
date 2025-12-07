use alloy::primitives::{Address, Bytes, U256, U160, Uint};
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;

// Uniswap V3 SwapRouter02 interface
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
        sqrtPriceLimitX96: U160::ZERO,  // No price limit
    };

    let calldata = exactInputSingleCall { params }.abi_encode();
    Ok(Bytes::from(calldata))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_uniswap_selector() {
        let calldata = build_exact_input_single(
            Address::ZERO, Address::ZERO, 3000, Address::ZERO,
            U256::from(1000000u64), U256::from(900000u64)
        ).unwrap();
        println!("Uniswap Selector: 0x{:02x}{:02x}{:02x}{:02x}",
            calldata[0], calldata[1], calldata[2], calldata[3]);
    }
}
