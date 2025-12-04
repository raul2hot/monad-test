use alloy::primitives::Bytes;
use alloy::providers::Provider;
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;
use std::collections::HashMap;
use tracing::debug;

use crate::config::MULTICALL3_ADDRESS;
use crate::pools::{
    calculate_lfj_price, decode_active_id_response, decode_bin_step_response,
    decode_slot0_to_price, CallType, PoolPrice, PriceCall,
};

// Multicall3 interface
sol! {
    #[derive(Debug)]
    struct Call3 {
        address target;
        bool allowFailure;
        bytes callData;
    }

    #[derive(Debug)]
    struct MulticallResult {
        bool success;
        bytes returnData;
    }

    #[derive(Debug)]
    function aggregate3(Call3[] calldata calls) external payable returns (MulticallResult[] memory returnData);
}

/// Executes batched price calls via Multicall3
pub async fn fetch_prices_batched<P: Provider>(
    provider: &P,
    price_calls: Vec<PriceCall>,
) -> Result<(Vec<PoolPrice>, u128)> {
    let start = std::time::Instant::now();

    // Build multicall calls
    let calls: Vec<Call3> = price_calls
        .iter()
        .map(|pc| Call3 {
            target: pc.pool_address,
            allowFailure: true,
            callData: pc.calldata.clone(),
        })
        .collect();

    // Encode the aggregate3 call
    let calldata = aggregate3Call { calls }.abi_encode();

    // Execute the call
    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(MULTICALL3_ADDRESS)
        .input(alloy::rpc::types::TransactionInput::new(Bytes::from(
            calldata,
        )));

    let result = provider.call(tx).await?;

    // Decode the results
    let decoded = aggregate3Call::abi_decode_returns(&result)?;

    let elapsed_ms = start.elapsed().as_millis();
    debug!("Multicall completed in {}ms", elapsed_ms);

    // Process results
    let mut prices = Vec::new();

    // For LFJ, we need to collect activeId and binStep separately
    let mut lfj_active_ids: HashMap<String, u32> = HashMap::new();
    let mut lfj_bin_steps: HashMap<String, u16> = HashMap::new();
    let mut lfj_fee_bps: HashMap<String, u32> = HashMap::new();

    // The decoded result is the vector of MulticallResult directly
    for (i, res) in decoded.iter().enumerate() {
        if !res.success {
            debug!("Call failed for: {}", price_calls[i].pool_name);
            continue;
        }

        match price_calls[i].call_type {
            CallType::V3Slot0 => {
                match decode_slot0_to_price(&res.returnData) {
                    Ok(price) => {
                        prices.push(PoolPrice {
                            pool_name: price_calls[i].pool_name.clone(),
                            price,
                            fee_bps: price_calls[i].fee_bps,
                        });
                    }
                    Err(e) => {
                        debug!(
                            "Failed to decode V3 price for {}: {}",
                            price_calls[i].pool_name, e
                        );
                    }
                }
            }
            CallType::LfjActiveId => {
                match decode_active_id_response(&res.returnData) {
                    Ok(active_id) => {
                        let pool_name = price_calls[i].pool_name.clone();
                        lfj_active_ids.insert(pool_name.clone(), active_id);
                        lfj_fee_bps.insert(pool_name, price_calls[i].fee_bps);
                    }
                    Err(e) => {
                        debug!(
                            "Failed to decode LFJ activeId for {}: {}",
                            price_calls[i].pool_name, e
                        );
                    }
                }
            }
            CallType::LfjBinStep => {
                match decode_bin_step_response(&res.returnData) {
                    Ok(bin_step) => {
                        // Remove "_binStep" suffix to get pool name
                        let pool_name = price_calls[i]
                            .pool_name
                            .strip_suffix("_binStep")
                            .unwrap_or(&price_calls[i].pool_name)
                            .to_string();
                        lfj_bin_steps.insert(pool_name, bin_step);
                    }
                    Err(e) => {
                        debug!(
                            "Failed to decode LFJ binStep for {}: {}",
                            price_calls[i].pool_name, e
                        );
                    }
                }
            }
        }
    }

    // Calculate LFJ prices from collected activeId and binStep
    for (pool_name, active_id) in lfj_active_ids.iter() {
        if let Some(bin_step) = lfj_bin_steps.get(pool_name) {
            let price = calculate_lfj_price(*active_id, *bin_step);
            let fee_bps = lfj_fee_bps.get(pool_name).copied().unwrap_or(15);
            prices.push(PoolPrice {
                pool_name: pool_name.clone(),
                price,
                fee_bps,
            });
        }
    }

    Ok((prices, elapsed_ms))
}
