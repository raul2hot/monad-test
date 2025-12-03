//! Monad Arbitrage Bot Configuration

pub const CHAIN_ID: u64 = 143;
pub const BLOCK_TIME_MS: u64 = 400;

// Tokens
pub const WMON: &str = "0x3bd359C1119dA7Da1D913D1C4D2B7c461115433A";
pub const USDC: &str = "0xf817257fed379853cDe0fa4F97AB987181B1E5Ea";

// Uniswap V3
pub const UNISWAP_FACTORY: &str = "0x204FAca1764B154221e35c0d20aBb3c525710498";
pub const UNISWAP_MON_USDC_POOL: &str = "0x659bd0bc4167ba25c62e05656f78043e7ed4a9da";

// PancakeSwap V3 (Verified from MonadVision)
pub const PANCAKE_SMART_ROUTER: &str = "0x21114915Ac6d5A2e156931e20B20b038dEd0Be7C";
pub const PANCAKE_FACTORY: &str = "DISCOVER_AT_RUNTIME"; // Query from SmartRouter
pub const PANCAKE_MON_USDC_POOL: &str = "DISCOVER_AT_RUNTIME"; // Query from Factory

// Monorail API
pub const MONORAIL_API: &str = "https://pathfinder.monorail.xyz/v4/quote";
pub const MONORAIL_APP_ID: &str = "0";

// Thresholds
pub const MIN_SPREAD_PCT: f64 = 0.5;
pub const MAX_SPREAD_PCT: f64 = 10.0;
