//! Monad Arbitrage Bot Configuration

pub const CHAIN_ID: u64 = 143;
pub const BLOCK_TIME_MS: u64 = 400;

// Tokens
pub const WMON: &str = "0x3bd359C1119dA7Da1D913D1C4D2B7c461115433A";
pub const USDC: &str = "0x754704Bc059F8C67012fEd69BC8A327a5aafb603"; // Circle native USDC (MAINNET)

// Uniswap V3
pub const UNISWAP_FACTORY: &str = "0x204FAca1764B154221e35c0d20aBb3c525710498";
pub const UNISWAP_MON_USDC_POOL: &str = "DISCOVER_AT_RUNTIME"; // Must discover for mainnet USDC

// PancakeSwap V3 (Verified from MonadVision)
pub const PANCAKE_SMART_ROUTER: &str = "0x21114915Ac6d5A2e156931e20B20b038dEd0Be7C";
pub const PANCAKE_FACTORY: &str = "DISCOVER_AT_RUNTIME"; // Query from SmartRouter
pub const PANCAKE_MON_USDC_POOL: &str = "DISCOVER_AT_RUNTIME"; // Query from Factory

// 0x Swap API (Primary Aggregator)
pub const ZRX_API_BASE: &str = "https://api.0x.org";
pub const ZRX_PRICE_ENDPOINT: &str = "/swap/allowance-holder/price";
pub const ZRX_QUOTE_ENDPOINT: &str = "/swap/allowance-holder/quote";

// Uniswap V3 SwapRouter02 - VERIFIED from MonadVision
pub const UNISWAP_SWAP_ROUTER: &str = "0xfE31F71C1b106EAc32F1A19239c9a9A72ddfb900";

// 0x AllowanceHolder - VERIFIED (same address on all EVM chains)
pub const ALLOWANCE_HOLDER: &str = "0x0000000000001fF3684f28c67538d4D072C22734";

// Thresholds
pub const MIN_SPREAD_PCT: f64 = 0.5;
pub const MAX_SPREAD_PCT: f64 = 10.0;

// Execution settings
pub const DEFAULT_SLIPPAGE_BPS: u32 = 100;  // 1%
pub const DEFAULT_POOL_FEE: u32 = 500;      // 0.05% fee tier (most common for stables)
pub const GAS_BUFFER: u64 = 5_000;
pub const GAS_PRICE_BUMP_PCT: u64 = 110;    // 10% bump

// Profitability thresholds
pub const MIN_WMON_TRADE_AMOUNT: f64 = 30.0;     // Minimum 30 WMON per trade
pub const MIN_SPREAD_FOR_SMALL_TRADE: f64 = 2.0; // If trade < 50 WMON, require 2%+ spread
pub const RECOMMENDED_WMON_AMOUNT: f64 = 50.0;   // Recommended trade size for profitability

// Gas guard limits - reject 0x routes that are too expensive
pub const MAX_0X_GAS: u64 = 400_000;             // Max gas for 0x leg (reject if higher)
pub const MAX_TOTAL_GAS: u64 = 850_000;          // Max total gas for both legs combined (400k Uni + 400k 0x + buffer)
