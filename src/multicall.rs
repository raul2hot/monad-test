use alloy::primitives::Bytes;
use alloy::providers::Provider;
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;
use tracing::debug;

use crate::config::MULTICALL3_ADDRESS;
use crate::pools::{decode_slot0_to_price, PoolPrice, PriceCall};

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

    // The decoded result is the vector of MulticallResult directly
    for (i, res) in decoded.iter().enumerate() {
        if res.success {
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
                        "Failed to decode price for {}: {}",
                        price_calls[i].pool_name, e
                    );
                }
            }
        } else {
            debug!("Call failed for pool: {}", price_calls[i].pool_name);
        }
    }

    Ok((prices, elapsed_ms))
}
