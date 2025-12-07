use alloy::primitives::Address;

// ============== MONAD MAINNET CONFIGURATION ==============
// Chain ID: 143
// RPC Port: 8080 (NOT 8545!)
// WS Port: 8081 (NOT 8546!)

// Core Token Addresses (Monad Mainnet - Chain ID 143)
pub const WMON_ADDRESS: Address = alloy::primitives::address!("3bd359C1119dA7Da1D913D1C4D2B7c461115433A");
pub const USDC_ADDRESS: Address = alloy::primitives::address!("754704Bc059F8C67012fEd69BC8A327a5aafb603");
pub const MULTICALL3_ADDRESS: Address = alloy::primitives::address!("cA11bde05977b3631167028862bE2a173976CA11");

// TODO: Update this address after deploying MonadAtomicArb contract
// Atomic Arbitrage Contract (deployed by user - UPDATE THIS AFTER DEPLOYMENT)
pub const ATOMIC_ARB_CONTRACT: Address = alloy::primitives::address!("7299daB2965c0A6ce471a8284a1D05bB483e05b2");

// Token decimals
pub const WMON_DECIMALS: u8 = 18;
pub const USDC_DECIMALS: u8 = 6;

// Default polling interval in milliseconds
// NOTE: For local node, use NodeConfig.poll_interval instead (100ms)
pub const POLL_INTERVAL_MS: u64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolType {
    UniswapV3,
    PancakeV3,
    LiquidityBook, // LFJ - TraderJoe style
    MondayTrade,   // V3-style (inspired by Uniswap V3, uses slot0())
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

// LFJ Pool (Liquidity Book / DLMM)
pub fn get_lfj_pool() -> PoolConfig {
    PoolConfig {
        name: "LFJ",
        address: alloy::primitives::address!("5e60bc3f7a7303bc4dfe4dc2220bdc90bc04fe22"),
        pool_type: PoolType::LiquidityBook,
        fee_bps: 10, // Bin step 10 = ~0.10% base fee (verified from pool contract)
    }
}

// Monday Trade Pool
pub fn get_monday_trade_pool() -> PoolConfig {
    PoolConfig {
        name: "MondayTrade",
        address: alloy::primitives::address!("8f889ba499c0a176fb8f233d9d35b1c132eb868c"),
        pool_type: PoolType::MondayTrade,
        fee_bps: 5, // 0.05% fee (NOT 30!)
    }
}

// Get all pools
pub fn get_all_pools() -> Vec<PoolConfig> {
    let mut pools = get_v3_pools();
    pools.push(get_lfj_pool());
    pools.push(get_monday_trade_pool());
    pools
}

// ============== ROUTER ADDRESSES ==============

pub const UNISWAP_SWAP_ROUTER: Address = alloy::primitives::address!("fE31F71C1b106EAc32F1A19239c9a9A72ddfb900");
pub const PANCAKE_SMART_ROUTER: Address = alloy::primitives::address!("21114915Ac6d5A2e156931e20B20b038dEd0Be7C");
pub const LFJ_LB_ROUTER: Address = alloy::primitives::address!("18556DA13313f3532c54711497A8FedAC273220E");
pub const MONDAY_SWAP_ROUTER: Address = alloy::primitives::address!("FE951b693A2FE54BE5148614B109E316B567632F");

// ============== ROUTER CONFIG ==============

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouterType {
    UniswapV3,
    PancakeV3,
    LfjLB,
    MondayTrade,
}

#[derive(Debug, Clone)]
pub struct RouterConfig {
    pub name: &'static str,
    pub address: Address,
    pub router_type: RouterType,
    pub pool_address: Address,  // The specific pool to use
    pub pool_fee: u32,          // Fee tier for V3 pools (in hundredths of bps, e.g., 3000 = 0.3%)
}

pub fn get_routers() -> Vec<RouterConfig> {
    vec![
        RouterConfig {
            name: "Uniswap",
            address: UNISWAP_SWAP_ROUTER,
            router_type: RouterType::UniswapV3,
            pool_address: alloy::primitives::address!("659bd0bc4167ba25c62e05656f78043e7ed4a9da"),
            pool_fee: 3000,  // 0.30%
        },
        RouterConfig {
            name: "PancakeSwap1",
            address: PANCAKE_SMART_ROUTER,
            router_type: RouterType::PancakeV3,
            pool_address: alloy::primitives::address!("63e48B725540A3Db24ACF6682a29f877808C53F2"),
            pool_fee: 500,  // 0.05%
        },
        RouterConfig {
            name: "PancakeSwap2",
            address: PANCAKE_SMART_ROUTER,
            router_type: RouterType::PancakeV3,
            pool_address: alloy::primitives::address!("85717A98d195c9306BBf7c9523Ba71F044Fea0f7"),
            pool_fee: 2500,  // 0.25%
        },
        RouterConfig {
            name: "LFJ",
            address: LFJ_LB_ROUTER,
            router_type: RouterType::LfjLB,
            pool_address: alloy::primitives::address!("5e60bc3f7a7303bc4dfe4dc2220bdc90bc04fe22"),
            pool_fee: 10,  // Bin step (verified from pool contract)
        },
        RouterConfig {
            name: "MondayTrade",
            address: MONDAY_SWAP_ROUTER,
            router_type: RouterType::MondayTrade,
            pool_address: alloy::primitives::address!("8f889ba499c0a176fb8f233d9d35b1c132eb868c"),
            pool_fee: 500,  // 0.05% fee tier (NOT 3000!)
        },
    ]
}

pub fn get_router_by_name(name: &str) -> Option<RouterConfig> {
    get_routers().into_iter().find(|r| r.name.to_lowercase() == name.to_lowercase())
}
