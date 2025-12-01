use alloy::primitives::{Address, address};
use std::env;

/// Main configuration for the Monad Arbitrage MVP
pub struct Config {
    pub rpc_url: String,
    pub chain_id: u64,
    pub poll_interval_ms: u64,
    pub max_hops: usize,
    pub min_profit_bps: u32, // Minimum profit in basis points (100 = 1%)
    pub min_liquidity_usd: f64,      // Minimum liquidity in USD
    pub min_liquidity_native: u128,  // Minimum liquidity in native token units
}

impl Config {
    pub fn from_env() -> eyre::Result<Self> {
        dotenvy::dotenv().ok();

        Ok(Self {
            rpc_url: env::var("ALCHEMY_RPC_URL")
                .unwrap_or_else(|_| "https://rpc.monad.xyz".to_string()),
            chain_id: 143,
            poll_interval_ms: 1000, // 1 second (matches block time)
            max_hops: 4,            // Max 4 swaps in an arb cycle
            min_profit_bps: 10,     // 0.1% minimum to log
            min_liquidity_usd: 1000.0,                    // $1000 minimum
            min_liquidity_native: 1000 * 10u128.pow(18),  // 1000 MON minimum
        })
    }
}

/// Canonical token addresses on Monad Mainnet
pub mod tokens {
    use super::*;

    // Native Wrapped Token
    pub const WMON: Address = address!("3bd359C1119dA7Da1D913D1C4D2B7c461115433A");

    // Stablecoins
    pub const USDC: Address = address!("754704Bc059F8C67012fEd69BC8A327a5aafb603");
    pub const USDT: Address = address!("e7cd86e13AC4309349F30B3435a9d337750fC82D");

    // Major Tokens
    pub const WETH: Address = address!("EE8c0E9f1BFFb4Eb878d8f15f368A02a35481242");
    pub const WBTC: Address = address!("0555E30da8f98308EdB960aa94C0Db47230d2B9c");

    // Liquid Staking Tokens (LSTs)
    pub const SMON: Address = address!("A3227C5969757783154C60bF0bC1944180ed81B9"); // Kintsu
    pub const GMON: Address = address!("8498312A6B3CbD158bf0c93AbdCF29E6e4F55081"); // Magma

    // Base tokens for starting arbitrage cycles
    pub const BASE_TOKENS: [Address; 4] = [WMON, USDC, USDT, WETH];

    /// Get the symbol for a token address
    pub fn symbol(addr: Address) -> &'static str {
        match addr {
            a if a == WMON => "WMON",
            a if a == USDC => "USDC",
            a if a == USDT => "USDT",
            a if a == WETH => "WETH",
            a if a == WBTC => "WBTC",
            a if a == SMON => "sMON",
            a if a == GMON => "gMON",
            _ => "???",
        }
    }

    /// Get the decimals for a token address
    /// USDC/USDT use 6 decimals, WBTC uses 8, most others use 18
    pub fn decimals(addr: Address) -> u8 {
        match addr {
            a if a == WMON => 18,
            a if a == USDC => 6,
            a if a == USDT => 6,
            a if a == WETH => 18,
            a if a == WBTC => 8,
            a if a == SMON => 18,
            a if a == GMON => 18,
            _ => 18, // Default assumption
        }
    }
}

/// DEX contract addresses
pub mod contracts {
    use super::*;

    /// Uniswap V3 contracts on Monad Mainnet
    /// Source: https://github.com/monad-crypto/protocols/blob/main/mainnet/Uniswap.json
    pub mod uniswap_v3 {
        use super::*;

        pub const FACTORY: Address = address!("204faca1764b154221e35c0d20abb3c525710498");
        pub const SWAP_ROUTER: Address = address!("d6145b2d3f379919e8cdeda7b97e37c4b2ca9c40");
        pub const QUOTER_V2: Address = address!("661e93cca42afacb172121ef892830ca3b70f08d");
        pub const NFT_POSITION_MANAGER: Address =
            address!("7197e214c0b767cfb76fb734ab638e2c192f4e53");

        // Fee tiers in hundredths of a bip: 100 = 0.01%, 500 = 0.05%, 3000 = 0.3%, 10000 = 1%
        pub const FEE_TIERS: [u32; 4] = [100, 500, 3000, 10000];
    }

    /// PancakeSwap V3 contracts on Monad Mainnet
    /// Source: https://github.com/monad-crypto/protocols/blob/main/mainnet/PancakeSwap.json
    pub mod pancakeswap_v3 {
        use super::*;

        pub const FACTORY: Address = address!("0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865");
        pub const SWAP_ROUTER: Address = address!("1b81D678ffb9C0263b24A97847620C99d213eB14");
        pub const QUOTER_V2: Address = address!("B048Bbc1Ee6b733FFfCFb9e9CeF7375518e25997");

        // Fee tiers (slightly different from Uniswap)
        pub const FEE_TIERS: [u32; 4] = [100, 500, 2500, 10000];
    }

    /// LFJ (TraderJoe V2.1) contracts - Liquidity Book on Monad Mainnet
    /// Source: https://github.com/monad-crypto/protocols/blob/main/mainnet/LFJ.json
    pub mod lfj {
        use super::*;

        pub const LB_FACTORY: Address = address!("b43120c4745967fa9b93E79C149E66B0f2D6Fe0c");
        pub const LB_ROUTER: Address = address!("18556DA13313f3532c54711497A8FedAC273220E");
        pub const LB_QUOTER: Address = address!("9A550a522BBaDFB69019b0432800Ed17855A51C3");
    }
}
