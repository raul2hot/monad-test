# Monad Arbitrage Bot - Build Instructions

## Overview

Build a Rust arbitrage bot that detects price differences between **0x Swap API** (aggregator) and **direct DEX pools** (Uniswap V3 / PancakeSwap V3) on Monad mainnet.

**Strategy**: 0x aggregates the best price across 10+ DEXes (Kuru, Crystal, Clober, OctoSwap, Atlantis, IziSwap, Intro, Morpheus, LFJ, Uniswap). Compare this against direct pool prices. When 0x price differs from a specific pool by >0.5%, there's an arbitrage opportunity.

**IMPORTANT**: 0x API requires a free API key from https://dashboard.0x.org/create-account

---

## Architecture

```
┌─────────────────┐     ┌──────────────────┐
│   0x Swap API   │     │  Direct Pools    │
│  (aggregated)   │     │  (Uniswap/PCS)   │
└────────┬────────┘     └────────┬─────────┘
         │                       │
         ▼                       ▼
    ┌────────────────────────────────┐
    │      Price Comparator          │
    │  (detect spread > 0.5%)        │
    └────────────────────────────────┘
                   │
                   ▼
    ┌────────────────────────────────┐
    │      Alert / Execute           │
    └────────────────────────────────┘
```

---

## Verified Contract Addresses (Monad Mainnet - Chain ID 143)

### Core Tokens
```
WMON:  0x3bd359C1119dA7Da1D913D1C4D2B7c461115433A
USDC:  0x754704Bc059F8C67012fEd69BC8A327a5aafb603  (Circle native USDC - MAINNET)
```

> ⚠️ **WARNING**: `0xf817257fed379853cDe0fa4F97AB987181B1E5Ea` is TESTNET USDC - do NOT use on mainnet!

### Uniswap V3 (Verified)
```
Factory:         0x204FAca1764B154221e35c0d20aBb3c525710498
QuoterV2:        0x661E93cca42AfacB172121EF892830cA3b70F08d
UniversalRouter: 0x0D97Dc33264bfC1c226207428A79b26757fb9dc3
MON/USDC Pool:   DISCOVER_VIA_FACTORY  (use factory.getPool with new USDC address)
```

> ⚠️ **NOTE**: Old pool `0x659bd0bc...` was for testnet USDC. Must discover new pool for mainnet USDC!

### PancakeSwap V3 (Verified from MonadVision)
```
SmartRouter:     0x21114915Ac6d5A2e156931e20B20b038dEd0Be7C
Factory:         QUERY_FROM_SMARTROUTER (call factory() method)
MON/USDC Pool:   DISCOVER_VIA_FACTORY
```

### 0x Swap API (Verified - Primary Aggregator)
```
Base URL:        https://api.0x.org
Price Endpoint:  /swap/allowance-holder/price
Quote Endpoint:  /swap/allowance-holder/quote
Chain ID:        143
Required Header: 0x-api-key: YOUR_API_KEY
Required Header: 0x-version: v2
API Key:         FREE from https://dashboard.0x.org/create-account
```

---

## Implementation Steps

### Step 1: Create New Rust Project

```bash
cargo new monad-arb-bot
cd monad-arb-bot
```

### Step 2: Cargo.toml

```toml
[package]
name = "monad-arb-bot"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1.0", features = ["full"] }
reqwest = { version = "0.11", features = ["json"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
alloy = { version = "0.9", features = ["full", "provider-ws"] }
eyre = "0.6"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
dotenvy = "0.15"
```

### Step 3: src/config.rs

```rust
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

// Thresholds
pub const MIN_SPREAD_PCT: f64 = 0.5;
pub const MAX_SPREAD_PCT: f64 = 10.0;
```

### Step 4: src/zrx.rs (0x Swap API Client)

```rust
//! 0x Swap API Client for Monad

use eyre::Result;
use reqwest::Client;
use serde::Deserialize;
use std::env;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceResponse {
    pub buy_amount: String,
    pub sell_amount: String,
    pub buy_token: String,
    pub sell_token: String,
    pub liquidity_available: bool,
    pub gas: String,
    pub gas_price: String,
    #[serde(default)]
    pub route: Option<RouteInfo>,
}

#[derive(Debug, Deserialize)]
pub struct RouteInfo {
    pub fills: Vec<Fill>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fill {
    pub source: String,
    pub proportion_bps: String,
}

pub struct ZrxClient {
    client: Client,
    api_key: String,
}

impl ZrxClient {
    pub fn new() -> Result<Self> {
        let api_key = env::var("ZRX_API_KEY")
            .map_err(|_| eyre::eyre!("ZRX_API_KEY not set. Get free key at https://dashboard.0x.org"))?;
        
        Ok(Self {
            client: Client::new(),
            api_key,
        })
    }

    /// Get price for selling tokens
    pub async fn get_price(
        &self,
        sell_token: &str,
        buy_token: &str,
        sell_amount: &str,
    ) -> Result<PriceResponse> {
        let url = format!(
            "{}{}?chainId={}&sellToken={}&buyToken={}&sellAmount={}",
            crate::config::ZRX_API_BASE,
            crate::config::ZRX_PRICE_ENDPOINT,
            crate::config::CHAIN_ID,
            sell_token,
            buy_token,
            sell_amount
        );

        tracing::debug!("0x API URL: {}", url);

        let response = self.client
            .get(&url)
            .header("0x-api-key", &self.api_key)
            .header("0x-version", "v2")
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        tracing::debug!("0x API status: {}, body length: {}", status, body.len());

        if !status.is_success() {
            return Err(eyre::eyre!("0x API error: {} - {}", status, body));
        }

        let price: PriceResponse = serde_json::from_str(&body)
            .map_err(|e| eyre::eyre!("Failed to parse 0x response: {}. Body: {}", e, body))?;

        if !price.liquidity_available {
            return Err(eyre::eyre!("No liquidity available for this pair"));
        }

        Ok(price)
    }

    /// Get MON price in USDC
    /// Returns price as USDC per 1 MON
    pub async fn get_mon_usdc_price(&self) -> Result<f64> {
        // Sell 1 WMON (18 decimals)
        let sell_amount = "1000000000000000000"; // 1e18
        
        let response = self.get_price(
            crate::config::WMON,
            crate::config::USDC,
            sell_amount,
        ).await?;

        // buyAmount is USDC (6 decimals)
        // Example: buyAmount="29132" means $0.029132
        let buy_amount: f64 = response.buy_amount.parse().unwrap_or(0.0);
        let usdc_price = buy_amount / 1_000_000.0; // Convert from 6 decimals

        // Log which DEXes 0x is routing through
        if let Some(route) = &response.route {
            let sources: Vec<&str> = route.fills.iter()
                .map(|f| f.source.as_str())
                .collect();
            tracing::info!("0x routing through: {:?}", sources);
        }

        Ok(usdc_price)
    }
}
```

### Step 5: src/pools.rs

```rust
//! Direct Pool Price Queries

use alloy::primitives::{Address, U160, U256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;
use std::str::FromStr;

sol! {
    function slot0() external view returns (
        uint160 sqrtPriceX96,
        int24 tick,
        uint16 observationIndex,
        uint16 observationCardinality,
        uint16 observationCardinalityNext,
        uint8 feeProtocol,
        bool unlocked
    );

    function getPool(
        address tokenA,
        address tokenB,
        uint24 fee
    ) external view returns (address pool);

    function liquidity() external view returns (uint128);

    // For PancakeSwap SmartRouter -> get factory address
    function factory() external view returns (address);
}

/// Convert sqrtPriceX96 to human readable price
/// MON (18 decimals) / USDC (6 decimals)
pub fn sqrt_price_to_mon_usdc(sqrt_price_x96: U160, token0_is_mon: bool) -> f64 {
    let sqrt_price: f64 = sqrt_price_x96.to_string().parse().unwrap_or(0.0);
    let q96: f64 = 2f64.powi(96);

    let ratio = sqrt_price / q96;
    let raw_price = ratio * ratio;

    // Adjust for decimals: MON=18, USDC=6, diff=12
    let decimal_adj = 10f64.powi(12);

    if token0_is_mon {
        // price = USDC/MON, we want MON in USDC terms
        raw_price * decimal_adj
    } else {
        // price = MON/USDC, invert
        if raw_price > 0.0 {
            decimal_adj / raw_price
        } else {
            0.0
        }
    }
}

/// Query pool price via slot0
pub async fn get_pool_price<P: Provider>(
    provider: &P,
    pool_address: &str,
    token0_is_mon: bool,
) -> Result<f64> {
    let pool = Address::from_str(pool_address)?;

    let call = slot0Call {};
    let tx = TransactionRequest::default()
        .to(pool)
        .input(call.abi_encode().into());

    let result = provider.call(tx).await?;
    let decoded = slot0Call::abi_decode_returns(&result)?;

    let price = sqrt_price_to_mon_usdc(decoded.sqrtPriceX96, token0_is_mon);
    Ok(price)
}

/// Discover pool address from factory
pub async fn discover_pool<P: Provider>(
    provider: &P,
    factory: &str,
    token_a: &str,
    token_b: &str,
    fee: u32,
) -> Result<Option<Address>> {
    let factory_addr = Address::from_str(factory)?;
    let token_a_addr = Address::from_str(token_a)?;
    let token_b_addr = Address::from_str(token_b)?;

    let call = getPoolCall {
        tokenA: token_a_addr,
        tokenB: token_b_addr,
        fee: fee.try_into()?,
    };

    let tx = TransactionRequest::default()
        .to(factory_addr)
        .input(call.abi_encode().into());

    let result = provider.call(tx).await?;
    let pool = getPoolCall::abi_decode_returns(&result)?;

    if pool == Address::ZERO {
        Ok(None)
    } else {
        Ok(Some(pool))
    }
}

/// Check if pool has liquidity
pub async fn has_liquidity<P: Provider>(provider: &P, pool: &str) -> Result<bool> {
    let pool_addr = Address::from_str(pool)?;

    let call = liquidityCall {};
    let tx = TransactionRequest::default()
        .to(pool_addr)
        .input(call.abi_encode().into());

    match provider.call(tx).await {
        Ok(result) => {
            let liq = liquidityCall::abi_decode_returns(&result)?;
            Ok(liq > 0)
        }
        Err(_) => Ok(false),
    }
}

/// Get PancakeSwap factory address from SmartRouter
pub async fn get_pancake_factory<P: Provider>(provider: &P) -> Result<Address> {
    let router = Address::from_str("0x21114915Ac6d5A2e156931e20B20b038dEd0Be7C")?;
    
    let call = factoryCall {};
    let tx = TransactionRequest::default()
        .to(router)
        .input(call.abi_encode().into());
    
    let result = provider.call(tx).await?;
    let factory = factoryCall::abi_decode_returns(&result)?;
    
    Ok(factory)
}
```

### Step 6: src/main.rs

```rust
//! Monad Arbitrage Bot - 0x vs Direct Pool Strategy

mod config;
mod zrx;
mod pools;

use alloy::providers::{Provider, ProviderBuilder};
use eyre::Result;
use std::env;
use std::time::Duration;
use tokio::time::interval;
use tracing::{info, warn, Level};

#[derive(Debug)]
struct ArbOpportunity {
    aggregator_price: f64,
    pool_price: f64,
    pool_name: String,
    spread_pct: f64,
    direction: String,
}

impl ArbOpportunity {
    fn print(&self) {
        println!("\n============ ARBITRAGE DETECTED ============");
        println!("  0x Price:       ${:.6}", self.aggregator_price);
        println!("  {} Price:  ${:.6}", self.pool_name, self.pool_price);
        println!("  Spread:         {:.3}%", self.spread_pct);
        println!("  Direction:      {}", self.direction);
        println!("  Est. Profit:    {:.3}% (before gas)", self.spread_pct - 0.3);
        println!("=============================================\n");
    }
}

fn check_arbitrage(
    aggregator_price: f64,
    pool_price: f64,
    pool_name: &str,
) -> Option<ArbOpportunity> {
    // Validate prices
    if aggregator_price <= 0.0 || pool_price <= 0.0 {
        return None;
    }

    let spread_pct = ((aggregator_price - pool_price) / pool_price) * 100.0;

    // Sanity check
    if spread_pct.abs() > config::MAX_SPREAD_PCT {
        warn!("Unrealistic spread: {:.2}% - ignoring", spread_pct);
        return None;
    }

    // Check minimum spread
    if spread_pct.abs() > config::MIN_SPREAD_PCT {
        let direction = if spread_pct > 0.0 {
            format!("BUY on {} → SELL via 0x", pool_name)
        } else {
            format!("BUY via 0x → SELL on {}", pool_name)
        };

        Some(ArbOpportunity {
            aggregator_price,
            pool_price,
            pool_name: pool_name.to_string(),
            spread_pct: spread_pct.abs(),
            direction,
        })
    } else {
        None
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .init();

    dotenvy::dotenv().ok();

    println!("==========================================");
    println!("  Monad Arbitrage Bot");
    println!("  Strategy: 0x API vs Direct Pools");
    println!("  Pair: MON/USDC");
    println!("==========================================\n");

    // HTTP RPC is sufficient (no WebSocket needed)
    let rpc_url = env::var("MONAD_RPC_URL")
        .unwrap_or_else(|_| "https://monad-mainnet.g.alchemy.com/v2/YOUR_KEY".to_string());

    let provider = ProviderBuilder::new()
        .on_http(rpc_url.parse()?);

    let chain_id = provider.get_chain_id().await?;
    info!("Connected to chain {}", chain_id);

    // Initialize 0x API client (requires ZRX_API_KEY env var)
    let zrx = zrx::ZrxClient::new()?;
    info!("0x API client initialized");

    // Determine token0 for price calculation
    let wmon = config::WMON.to_lowercase();
    let usdc = config::USDC.to_lowercase();
    let token0_is_mon = wmon < usdc;
    info!("Token0 is MON: {}", token0_is_mon);

    // Discover Uniswap MON/USDC pool (try different fee tiers)
    println!("Discovering Uniswap MON/USDC pool...");
    let mut uniswap_pool: Option<String> = None;
    for fee in [500u32, 3000, 10000] {  // 0.05%, 0.3%, 1%
        if let Some(pool) = pools::discover_pool(
            &provider,
            config::UNISWAP_FACTORY,
            config::WMON,
            config::USDC,
            fee,
        ).await? {
            let pool_str = format!("{:?}", pool);
            if pools::has_liquidity(&provider, &pool_str).await? {
                info!("Found Uniswap MON/USDC pool: {} (fee: {})", pool_str, fee);
                uniswap_pool = Some(pool_str);
                break;
            }
        }
    }

    let uniswap_pool = uniswap_pool.ok_or_else(|| {
        eyre::eyre!("No Uniswap MON/USDC pool found with liquidity. Check USDC address.")
    })?;

    // Main loop - poll every 2 seconds
    let mut poll_interval = interval(Duration::from_secs(2));

    println!("\nStarting price monitoring...\n");

    loop {
        poll_interval.tick().await;

        // Get 0x aggregated price
        let zrx_price = match zrx.get_mon_usdc_price().await {
            Ok(p) => p,
            Err(e) => {
                warn!("0x API error: {}", e);
                continue;
            }
        };

        // Get Uniswap direct pool price
        let uniswap_price = match pools::get_pool_price(
            &provider,
            &uniswap_pool,  // Use discovered pool
            token0_is_mon,
        ).await {
            Ok(p) => p,
            Err(e) => {
                warn!("Uniswap pool error: {}", e);
                continue;
            }
        };

        // Print current prices
        println!(
            "[{}] MON/USDC | 0x: ${:.6} | Uniswap: ${:.6} | Spread: {:.3}%",
            chrono::Local::now().format("%H:%M:%S"),
            zrx_price,
            uniswap_price,
            ((zrx_price - uniswap_price) / uniswap_price * 100.0)
        );

        // Check for arbitrage
        if let Some(arb) = check_arbitrage(zrx_price, uniswap_price, "Uniswap") {
            arb.print();
        }

        // TODO: Add PancakeSwap pool comparison here
        // let pancake_price = pools::get_pool_price(...).await?;
        // if let Some(arb) = check_arbitrage(zrx_price, pancake_price, "PancakeSwap") {
        //     arb.print();
        // }
    }
}
```

### Step 7: .env file

```env
# 0x API Key (REQUIRED - Get free key at https://dashboard.0x.org/create-account)
ZRX_API_KEY=your_0x_api_key_here

# Monad RPC - Use HTTP (cheaper than WebSocket for polling)
MONAD_RPC_URL=https://monad-mainnet.g.alchemy.com/v2/YOUR_ALCHEMY_KEY

# Optional: For event subscriptions (not needed for this strategy)
# ALCHEMY_WS_URL=wss://monad-mainnet.g.alchemy.com/v2/YOUR_ALCHEMY_KEY
```

---

## First Run Tasks

### Task 1: Discover PancakeSwap Factory from SmartRouter

The SmartRouter has a `factory()` method. Add this to pools.rs:

```rust
sol! {
    function factory() external view returns (address);
}

pub async fn get_pancake_factory<P: Provider>(provider: &P) -> Result<Address> {
    let router = Address::from_str("0x21114915Ac6d5A2e156931e20B20b038dEd0Be7C")?;
    
    let call = factoryCall {};
    let tx = TransactionRequest::default()
        .to(router)
        .input(call.abi_encode().into());
    
    let result = provider.call(tx).await?;
    let factory = factoryCall::abi_decode_returns(&result)?;
    
    println!("PancakeSwap Factory: {:?}", factory);
    Ok(factory)
}
```

Run this once at startup to get the factory address, then use it to discover pools.

### Task 2: Discover PancakeSwap MON/USDC Pool

After getting the factory, discover the pool:

```rust
// In main.rs startup
let pancake_factory = pools::get_pancake_factory(&provider).await?;
println!("PancakeSwap Factory: {:?}", pancake_factory);

// Fee tiers to try: 100 (0.01%), 500 (0.05%), 2500 (0.25%), 10000 (1%)
for fee in [500u32, 2500, 10000] {
    if let Some(pool) = pools::discover_pool(
        &provider,
        &format!("{:?}", pancake_factory),
        config::WMON,
        config::USDC,
        fee,
    ).await? {
        println!("Found PancakeSwap MON/USDC pool at {:?} (fee: {})", pool, fee);
        // Save this address and use it for price queries
    }
}
```

### Task 3: Verify 0x API Access

1. Get your free API key from https://dashboard.0x.org/create-account

2. Test the API manually:

```bash
# Replace YOUR_API_KEY with your actual key
curl --request GET \
  --url "https://api.0x.org/swap/allowance-holder/price?chainId=143&sellToken=0x3bd359C1119dA7Da1D913D1C4D2B7c461115433A&buyToken=0x754704Bc059F8C67012fEd69BC8A327a5aafb603&sellAmount=1000000000000000000" \
  --header "0x-api-key: YOUR_API_KEY" \
  --header "0x-version: v2"
```

3. Expected response includes:
   - `liquidityAvailable: true`
   - `buyAmount`: USDC amount for 1 WMON
   - `route.fills`: Which DEXes are being used

---

## Key Implementation Notes

1. **Use HTTP RPC, not WebSocket** - Polling every 2 seconds is sufficient for 400ms blocks. This reduces Alchemy costs significantly.

2. **0x aggregates best price** - It routes through 10+ DEXes (Kuru, Crystal, Clober, etc.). If a direct pool differs significantly, that's the arb.

3. **Price validation is critical** - Always check prices > 0 and spread < 10% before alerting.

4. **MON/USDC is the target pair** - Highest volume, most liquidity, real arbitrage opportunities.

5. **Start with monitoring only** - Don't execute trades until you've verified the bot correctly detects real opportunities.

6. **0x API Key Required** - Get free key at https://dashboard.0x.org/create-account

---

## File Structure

```
monad-arb-bot/
├── Cargo.toml
├── .env              # ZRX_API_KEY and MONAD_RPC_URL
└── src/
    ├── main.rs       # Entry point, main loop
    ├── config.rs     # Addresses and constants
    ├── zrx.rs        # 0x Swap API client
    └── pools.rs      # Direct pool queries
```

---

## Success Criteria

The bot is working correctly when:
1. 0x API returns valid MON/USDC prices (liquidityAvailable: true)
2. Uniswap pool slot0() returns valid sqrtPriceX96
3. Both prices are non-zero and similar (within 10%)
4. Spread calculation is realistic (typically 0-2%)
5. Arbitrage alerts only trigger on genuine spread > 0.5%

---

## Do NOT

- Do NOT use WebSocket for this strategy (HTTP polling is cheaper and sufficient)
- Do NOT monitor CHOG or other low-liquidity tokens
- Do NOT use Nad.fun DEX (it's a launchpad, not relevant for this strategy)
- Do NOT trust spreads > 10% (it means bad price data)
- Do NOT execute trades until monitoring is verified working

---

## Dependencies Note

Add `chrono` for timestamps:
```toml
chrono = "0.4"
```
