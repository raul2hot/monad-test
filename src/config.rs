use alloy::primitives::Address;

// Core Token Addresses (Monad Mainnet - Chain ID 143)
pub const WMON_ADDRESS: Address = alloy::primitives::address!("3bd359C1119dA7Da1D913D1C4D2B7c461115433A");
pub const USDC_ADDRESS: Address = alloy::primitives::address!("754704Bc059F8C67012fEd69BC8A327a5aafb603");
pub const MULTICALL3_ADDRESS: Address = alloy::primitives::address!("cA11bde05977b3631167028862bE2a173976CA11");

// Token decimals
pub const WMON_DECIMALS: u8 = 18;
pub const USDC_DECIMALS: u8 = 6;

// Polling interval in milliseconds
pub const POLL_INTERVAL_MS: u64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolType {
    UniswapV3,
    PancakeV3,
}

#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub name: &'static str,
    pub address: Address,
    pub pool_type: PoolType,
    pub fee_bps: u32, // Fee in basis points (100 = 1%)
}

impl PoolConfig {
    pub fn fee_percent(&self) -> f64 {
        self.fee_bps as f64 / 10000.0
    }
}

// V3 Pool Configurations
pub fn get_v3_pools() -> Vec<PoolConfig> {
    vec![
        PoolConfig {
            name: "Uniswap",
            address: alloy::primitives::address!("659bd0bc4167ba25c62e05656f78043e7ed4a9da"),
            pool_type: PoolType::UniswapV3,
            fee_bps: 30, // 0.30%
        },
        PoolConfig {
            name: "PancakeSwap1",
            address: alloy::primitives::address!("63e48B725540A3Db24ACF6682a29f877808C53F2"),
            pool_type: PoolType::PancakeV3,
            fee_bps: 5, // 0.05%
        },
        PoolConfig {
            name: "PancakeSwap2",
            address: alloy::primitives::address!("85717A98d195c9306BBf7c9523Ba71F044Fea0f7"),
            pool_type: PoolType::PancakeV3,
            fee_bps: 25, // 0.25%
        },
    ]
}

// Placeholder for future pools
pub fn get_other_pools() -> Vec<PoolConfig> {
    // LFJ and Monday Trade - to be implemented in Phase 2
    vec![]
}
