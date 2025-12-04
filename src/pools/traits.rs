use alloy::primitives::{Address, Bytes};

/// Represents the calldata needed to fetch price from a pool
#[derive(Debug, Clone)]
pub struct PriceCall {
    pub pool_name: String,
    pub pool_address: Address,
    pub calldata: Bytes,
    pub fee_bps: u32,
}

/// Represents a successfully fetched price
#[derive(Debug, Clone)]
pub struct PoolPrice {
    pub pool_name: String,
    pub price: f64, // Price in USDC per WMON
    pub fee_bps: u32,
}

impl PoolPrice {
    pub fn fee_percent(&self) -> f64 {
        self.fee_bps as f64 / 10000.0
    }
}
