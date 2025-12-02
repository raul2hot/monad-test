//! Configuration constants for Monad Arbitrage Monitor

use alloy::primitives::Address;
use std::str::FromStr;

// Network configuration
pub const CHAIN_ID: u64 = 143;
pub const BLOCK_TIME_MS: u64 = 400;

// Nad.fun DEX addresses
pub const NADFUN_DEX_ROUTER: &str = "0x0B79d71AE99528D1dB24A4148b5f4F865cc2b137";
pub const NADFUN_DEX_FACTORY: &str = "0x6B5F564339DbAD6b780249827f2198a841FEB7F3";

// Uniswap V3 addresses
pub const UNISWAP_FACTORY: &str = "0x204FAca1764B154221e35c0d20aBb3c525710498";
pub const UNISWAP_QUOTER_V2: &str = "0x661E93cca42AfacB172121EF892830cA3b70F08d";

// WMON (Wrapped MON)
pub const WMON: &str = "0x3bd359C1119dA7Da1D913D1C4D2B7c461115433A";

// Target tokens with cross-venue liquidity
pub const CHOG: &str = "0x350035555E10d9AfAF1566AaebfCeD5BA6C27777";
pub const MOLANDAK: &str = "0x7B2728c04aD436153285702e969e6EfAc3a97777";
pub const GMONAD: &str = "0x7db552eeb6b77a6babe6e0a739b5382cd653cc3e";

// Pool addresses
pub const CHOG_NADFUN_POOL: &str = "0x116e7d070f1888b81e1e0324f56d6746b2d7d8f1";
pub const CHOG_UNISWAP_POOL: &str = "0x745355f47db8c57e7911ef3da2e989b16039d12f";

// Event signatures (topic0)
pub const UNISWAP_V3_SWAP_TOPIC: &str = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67";
pub const ERC20_TRANSFER_TOPIC: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

/// Token information for tracking
#[derive(Debug, Clone)]
pub struct TokenInfo {
    pub symbol: &'static str,
    pub address: Address,
    pub nadfun_pool: Option<Address>,
    pub uniswap_pool: Option<Address>,
}

/// Get the list of tracked tokens
pub fn get_tracked_tokens() -> Vec<TokenInfo> {
    vec![
        TokenInfo {
            symbol: "CHOG",
            address: Address::from_str(CHOG).unwrap(),
            nadfun_pool: Some(Address::from_str(CHOG_NADFUN_POOL).unwrap()),
            uniswap_pool: Some(Address::from_str(CHOG_UNISWAP_POOL).unwrap()),
        },
        TokenInfo {
            symbol: "MOLANDAK",
            address: Address::from_str(MOLANDAK).unwrap(),
            nadfun_pool: None, // Pool address not specified
            uniswap_pool: None,
        },
        TokenInfo {
            symbol: "GMONAD",
            address: Address::from_str(GMONAD).unwrap(),
            nadfun_pool: None,
            uniswap_pool: None,
        },
    ]
}

/// Get WMON address
pub fn wmon_address() -> Address {
    Address::from_str(WMON).unwrap()
}

/// Parse address from string
pub fn parse_address(addr: &str) -> eyre::Result<Address> {
    Address::from_str(addr).map_err(|e| eyre::eyre!("Invalid address: {}", e))
}
