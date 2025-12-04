use alloy::primitives::{Address, Bytes};

/// Type of call for decoding purposes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallType {
    V3Slot0, // Used for Uniswap V3, PancakeSwap V3, and Monday Trade (all V3-style)
    LfjActiveId,
    LfjBinStep,
}

/// Represents the calldata needed to fetch price from a pool
#[derive(Debug, Clone)]
pub struct PriceCall {
    pub pool_name: String,
    pub pool_address: Address,
    pub calldata: Bytes,
    pub fee_bps: u32,
    pub call_type: CallType,
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
