use alloy::primitives::Address;

use crate::dex::Dex;

/// Edge data representing a swap between two tokens through a pool
#[derive(Debug, Clone)]
pub struct EdgeData {
    pub pool_address: Address,
    pub dex: Dex,
    pub price: f64,     // Effective price (after fees)
    pub fee: u32,       // Fee in hundredths of bip
    pub weight: f64,    // -ln(price) for Bellman-Ford
    pub liquidity: f64, // Normalized liquidity
}

impl EdgeData {
    /// Create new edge data from pool information
    pub fn new(pool_address: Address, dex: Dex, price: f64, fee: u32, liquidity: f64) -> Self {
        Self {
            pool_address,
            dex,
            price,
            fee,
            weight: -price.ln(), // Negative log for cycle detection
            liquidity,
        }
    }
}
