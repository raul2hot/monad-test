pub mod uniswap_v3;
pub mod pancake_v3;
pub mod lfj;
pub mod monday;

use alloy::primitives::{Address, Bytes, U256};
use eyre::Result;

use crate::config::RouterType;

/// Build swap calldata for the appropriate router
pub fn build_swap_calldata(
    router_type: RouterType,
    token_in: Address,
    token_out: Address,
    amount_in: U256,
    amount_out_min: U256,
    recipient: Address,
    pool_fee: u32,
    deadline: u64,
) -> Result<Bytes> {
    match router_type {
        RouterType::UniswapV3 => {
            uniswap_v3::build_exact_input_single(
                token_in, token_out, pool_fee, recipient, amount_in, amount_out_min
            )
        }
        RouterType::PancakeV3 => {
            pancake_v3::build_exact_input_single(
                token_in, token_out, pool_fee, recipient, amount_in, amount_out_min
            )
        }
        RouterType::LfjLB => {
            lfj::build_swap_exact_tokens_for_tokens(
                token_in, token_out, amount_in, amount_out_min, recipient, deadline,
                pool_fee,  // Pass pool_fee as bin_step for LFJ
            )
        }
        RouterType::MondayTrade => {
            monday::build_exact_input_single(
                token_in, token_out, pool_fee, recipient, amount_in, amount_out_min
            )
        }
    }
}
