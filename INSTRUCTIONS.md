# Monad Arbitrage Bot - Build Instructions

## Overview

Build a Rust arbitrage bot that detects price differences between **Monorail aggregator** and **direct DEX pools** (Uniswap V3 / PancakeSwap V3) on Monad mainnet.

**Strategy**: Monorail gives the "best aggregated price" across 16 DEXes. Compare this against direct pool prices. When Monorail price differs from a specific pool by >0.5%, there's an arbitrage opportunity.

---

## Architecture

```
┌─────────────────┐     ┌──────────────────┐
│  Monorail API   │     │  Direct Pools    │
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
USDC:  0xf817257fed379853cDe0fa4F97AB987181B1E5Ea
```

### Uniswap V3 (Verified)
```
Factory:         0x204FAca1764B154221e35c0d20aBb3c525710498
QuoterV2:        0x661E93cca42AfacB172121EF892830cA3b70F08d
UniversalRouter: 0x0D97Dc33264bfC1c226207428A79b26757fb9dc3
MON/USDC Pool:   0x659bd0bc4167ba25c62e05656f78043e7ed4a9da
```

### PancakeSwap V3 (Verified from MonadVision)
```
SmartRouter:     0x21114915Ac6d5A2e156931e20B20b038dEd0Be7C
Factory:         QUERY_FROM_SMARTROUTER (call factory() method)
MON/USDC Pool:   DISCOVER_VIA_FACTORY
```

### Monorail API
```
Base URL:  https://pathfinder.monorail.xyz
Endpoint:  /v4/quote
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
```

### Step 4: src/monorail.rs

```rust
//! Monorail API Client

use reqwest::Client;
use serde::{Deserialize, Serialize};
use eyre::Result;

#[derive(Debug, Deserialize)]
pub struct QuoteResponse {
    pub output: String,
    pub output_formatted: String,
    pub price: f64,
    pub route: Vec<RouteStep>,
}

#[derive(Debug, Deserialize)]
pub struct RouteStep {
    pub dex: String,
    pub pool: String,
}

pub struct MonorailClient {
    client: Client,
    base_url: String,
    app_id: String,
}

impl MonorailClient {
    pub fn new(app_id: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: crate::config::MONORAIL_API.to_string(),
            app_id: app_id.to_string(),
        }
    }

    /// Get quote for swapping tokens
    /// Returns price as tokenOut per tokenIn
    pub async fn get_quote(
        &self,
        token_in: &str,
        token_out: &str,
        amount_in: f64,
    ) -> Result<QuoteResponse> {
        let url = format!(
            "{}?source={}&from={}&to={}&amount={}",
            self.base_url,
            self.app_id,
            token_in,
            token_out,
            amount_in
        );

        let response = self.client
            .get(&url)
            .send()
            .await?
            .json::<QuoteResponse>()
            .await?;

        Ok(response)
    }

    /// Get MON price in USDC
    pub async fn get_mon_price(&self) -> Result<f64> {
        // Use native MON address (0x0...0)
        let quote = self.get_quote(
            "0x0000000000000000000000000000000000000000",
            crate::config::USDC,
            1.0, // 1 MON
        ).await?;

        Ok(quote.price)
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
//! Monad Arbitrage Bot - Monorail vs Direct Pool Strategy

mod config;
mod monorail;
mod pools;

use alloy::providers::{Provider, ProviderBuilder};
use eyre::Result;
use std::env;
use std::time::Duration;
use tokio::time::interval;
use tracing::{info, warn, Level};

#[derive(Debug)]
struct ArbOpportunity {
    monorail_price: f64,
    pool_price: f64,
    pool_name: String,
    spread_pct: f64,
    direction: String,
}

impl ArbOpportunity {
    fn print(&self) {
        println!("\n============ ARBITRAGE DETECTED ============");
        println!("  Monorail Price: ${:.6}", self.monorail_price);
        println!("  {} Price:  ${:.6}", self.pool_name, self.pool_price);
        println!("  Spread:         {:.3}%", self.spread_pct);
        println!("  Direction:      {}", self.direction);
        println!("  Est. Profit:    {:.3}% (before gas)", self.spread_pct - 0.3);
        println!("=============================================\n");
    }
}

fn check_arbitrage(
    monorail_price: f64,
    pool_price: f64,
    pool_name: &str,
) -> Option<ArbOpportunity> {
    // Validate prices
    if monorail_price <= 0.0 || pool_price <= 0.0 {
        return None;
    }

    let spread_pct = ((monorail_price - pool_price) / pool_price) * 100.0;

    // Sanity check
    if spread_pct.abs() > config::MAX_SPREAD_PCT {
        warn!("Unrealistic spread: {:.2}% - ignoring", spread_pct);
        return None;
    }

    // Check minimum spread
    if spread_pct.abs() > config::MIN_SPREAD_PCT {
        let direction = if spread_pct > 0.0 {
            format!("BUY on {} → SELL via Monorail", pool_name)
        } else {
            format!("BUY via Monorail → SELL on {}", pool_name)
        };

        Some(ArbOpportunity {
            monorail_price,
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
    println!("  Strategy: Monorail vs Direct Pools");
    println!("  Pair: MON/USDC");
    println!("==========================================\n");

    // HTTP RPC is sufficient (no WebSocket needed)
    let rpc_url = env::var("MONAD_RPC_URL")
        .unwrap_or_else(|_| "https://monad-mainnet.g.alchemy.com/v2/YOUR_KEY".to_string());

    let provider = ProviderBuilder::new()
        .on_http(rpc_url.parse()?);

    let chain_id = provider.get_chain_id().await?;
    info!("Connected to chain {}", chain_id);

    // Initialize Monorail client
    let monorail = monorail::MonorailClient::new(config::MONORAIL_APP_ID);

    // Determine token0 for price calculation
    let wmon = config::WMON.to_lowercase();
    let usdc = config::USDC.to_lowercase();
    let token0_is_mon = wmon < usdc;
    info!("Token0 is MON: {}", token0_is_mon);

    // Verify Uniswap pool has liquidity
    let uni_has_liq = pools::has_liquidity(&provider, config::UNISWAP_MON_USDC_POOL).await?;
    info!("Uniswap MON/USDC pool has liquidity: {}", uni_has_liq);

    if !uni_has_liq {
        return Err(eyre::eyre!("Uniswap pool has no liquidity"));
    }

    // Main loop - poll every 2 seconds
    let mut poll_interval = interval(Duration::from_secs(2));

    println!("\nStarting price monitoring...\n");

    loop {
        poll_interval.tick().await;

        // Get Monorail aggregated price
        let monorail_price = match monorail.get_mon_price().await {
            Ok(p) => p,
            Err(e) => {
                warn!("Monorail API error: {}", e);
                continue;
            }
        };

        // Get Uniswap direct pool price
        let uniswap_price = match pools::get_pool_price(
            &provider,
            config::UNISWAP_MON_USDC_POOL,
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
            "[{}] MON/USDC | Monorail: ${:.6} | Uniswap: ${:.6} | Spread: {:.3}%",
            chrono::Local::now().format("%H:%M:%S"),
            monorail_price,
            uniswap_price,
            ((monorail_price - uniswap_price) / uniswap_price * 100.0)
        );

        // Check for arbitrage
        if let Some(arb) = check_arbitrage(monorail_price, uniswap_price, "Uniswap") {
            arb.print();
        }

        // TODO: Add PancakeSwap pool comparison here
        // let pancake_price = pools::get_pool_price(...).await?;
        // if let Some(arb) = check_arbitrage(monorail_price, pancake_price, "PancakeSwap") {
        //     arb.print();
        // }
    }
}
```

### Step 7: .env file

```env
# Use HTTP RPC (cheaper than WebSocket for polling)
MONAD_RPC_URL=https://monad-mainnet.g.alchemy.com/v2/YOUR_API_KEY

# Optional: For event subscriptions (not needed for this strategy)
# ALCHEMY_WS_URL=wss://monad-mainnet.g.alchemy.com/v2/YOUR_API_KEY
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

### Task 3: Verify Monorail API Endpoint

Test the API manually first:

```bash
curl "https://pathfinder.monorail.xyz/v4/quote?source=0&from=0x0000000000000000000000000000000000000000&to=0xf817257fed379853cDe0fa4F97AB987181B1E5Ea&amount=1"
```

If the mainnet endpoint differs, check:
- https://monorail.xyz/developers
- Contact @donorail on Telegram

---

## Key Implementation Notes

1. **Use HTTP RPC, not WebSocket** - Polling every 2 seconds is sufficient for 400ms blocks. This reduces Alchemy costs significantly.

2. **Monorail gives aggregated best price** - It routes through 16 DEXes. If a direct pool differs significantly, that's the arb.

3. **Price validation is critical** - Always check prices > 0 and spread < 10% before alerting.

4. **MON/USDC is the target pair** - Highest volume, most liquidity, real arbitrage opportunities.

5. **Start with monitoring only** - Don't execute trades until you've verified the bot correctly detects real opportunities.

---

## File Structure

```
monad-arb-bot/
├── Cargo.toml
├── .env
└── src/
    ├── main.rs      # Entry point, main loop
    ├── config.rs    # Addresses and constants
    ├── monorail.rs  # Monorail API client
    └── pools.rs     # Direct pool queries
```

---

## Success Criteria

The bot is working correctly when:
1. Monorail API returns valid MON/USDC prices
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
