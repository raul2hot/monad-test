# Monad Arbitrage Bot - Implementation Instructions

> **For Claude Code Opus** - Follow these instructions exactly to upgrade the arbitrage monitor.

## Problem Statement

The current bot only listens for swap events, but:
1. CHOG/WMON Uniswap V3 pool hasn't traded in 23+ hours (low liquidity)
2. Bot sits idle waiting for events that never come
3. Missing opportunities on more active pairs

## Task 1: Add Periodic Price Polling

**File to modify:** `src/main.rs`

Add a polling mechanism that fetches prices every 10 seconds regardless of swap events.

### Implementation

Add this import at the top:
```rust
use tokio::time::{interval, Duration};
```

Replace the current event-only loop with a combined polling + events approach. After the subscription setup, add:

```rust
// Create polling interval
let mut poll_interval = interval(Duration::from_secs(10));

// Clone providers and addresses for polling task
let poll_provider = provider.clone();
let poll_state = price_state.clone();
let poll_lens = lens_address;
let poll_chog = chog;
let poll_uniswap_pool = chog_uniswap_pool;
let poll_token0_is_wmon = token0_is_wmon;

// Spawn polling task
let polling_task = tokio::spawn(async move {
    loop {
        poll_interval.tick().await;
        
        // Fetch Uniswap price
        let slot0_call = slot0Call {};
        let tx = TransactionRequest::default()
            .to(poll_uniswap_pool)
            .input(slot0_call.abi_encode().into());

        if let Ok(result) = poll_provider.call(tx.clone()).await {
            if let Ok(decoded) = slot0Call::abi_decode_returns(&result) {
                let price = calculate_price_from_sqrt_price_x96(
                    decoded.sqrtPriceX96, 
                    poll_token0_is_wmon
                );
                poll_state.update_uniswap("CHOG", price);
            }
        }

        // Fetch Nad.fun price via LENS
        let lens_call = getAmountOutCall {
            token: poll_chog,
            amountIn: U256::from(1_000_000_000_000_000_000u128),
            isBuy: true,
        };
        let tx = TransactionRequest::default()
            .to(poll_lens)
            .input(lens_call.abi_encode().into());

        if let Ok(result) = poll_provider.call(tx).await {
            if let Ok(decoded) = getAmountOutCall::abi_decode_returns(&result) {
                let tokens_out = decoded.amountOut.to_string().parse::<f64>().unwrap_or(0.0);
                if tokens_out > 0.0 {
                    let price = 1e18 / tokens_out;
                    poll_state.update_nadfun("CHOG", price);
                }
            }
        }

        // Get current block for display
        if let Ok(block) = poll_provider.get_block_number().await {
            poll_state.print_prices("CHOG", block, "POLL");
        }
    }
});
```

Update the final `tokio::select!` to include polling:
```rust
tokio::select! {
    _ = uniswap_task => warn!("Uniswap subscription ended"),
    _ = nadfun_task => warn!("Nad.fun subscription ended"),
    _ = polling_task => warn!("Polling task ended"),
}
```

---

## Task 2: Add More Active Pairs

**File to modify:** `src/config.rs`

Add these verified active tokens with cross-venue potential:

```rust
// Additional tokens to monitor
pub const MOLANDAK: &str = "0x7B2728c04aD436153285702e969e6EfAc3a97777";
pub const USDC: &str = "0xf817257fed379853cDe0fa4F97AB987181B1E5Ea";  // Circle USDC
pub const WETH: &str = "0xB5a30b0FDc5EA94A52fDc42e3E9760Cb8449Fb37";  // Wrapped ETH

// High-activity Uniswap pools (from user's logs)
pub const WMON_WETH_V4_POOL: &str = "0x378393c9fAcf16c5bfB3f1cF829c37A1d0F7d28e"; // Need to verify
pub const USDC_WMON_POOL: &str = "0x..."; // Need to find from factory
```

---

## Task 3: Add Pool Discovery Function

**File to create:** `src/pools.rs`

Create a utility to discover pools from factory contracts:

```rust
//! Pool discovery utilities

use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;

// Uniswap V3 Factory getPool function
sol! {
    function getPool(
        address tokenA,
        address tokenB,
        uint24 fee
    ) external view returns (address pool);
}

// Nad.fun Factory getPool (similar interface)
sol! {
    #[allow(non_camel_case_types)]
    function getNadfunPool(
        address tokenA,
        address tokenB
    ) external view returns (address pool);
}

/// Fee tiers for Uniswap V3
pub const FEE_TIERS: [u32; 4] = [100, 500, 3000, 10000];

/// Discover Uniswap V3 pool address for a token pair
pub async fn find_uniswap_v3_pool<P: Provider>(
    provider: &P,
    factory: Address,
    token_a: Address,
    token_b: Address,
) -> Result<Option<(Address, u32)>> {
    for fee in FEE_TIERS {
        let call = getPoolCall {
            tokenA: token_a,
            tokenB: token_b,
            fee: fee.try_into().unwrap(),
        };
        
        let tx = TransactionRequest::default()
            .to(factory)
            .input(call.abi_encode().into());

        if let Ok(result) = provider.call(tx).await {
            if let Ok(decoded) = getPoolCall::abi_decode_returns(&result) {
                if decoded.pool != Address::ZERO {
                    return Ok(Some((decoded.pool, fee)));
                }
            }
        }
    }
    Ok(None)
}

/// Check if a pool has liquidity (non-zero reserves)
pub async fn pool_has_liquidity<P: Provider>(
    provider: &P,
    pool: Address,
) -> Result<bool> {
    // Try to call slot0 - if it fails or returns 0 liquidity, pool is empty
    sol! {
        function liquidity() external view returns (uint128);
    }
    
    let call = liquidityCall {};
    let tx = TransactionRequest::default()
        .to(pool)
        .input(call.abi_encode().into());

    match provider.call(tx).await {
        Ok(result) => {
            if let Ok(decoded) = liquidityCall::abi_decode_returns(&result) {
                Ok(decoded._0 > 0)
            } else {
                Ok(false)
            }
        }
        Err(_) => Ok(false),
    }
}
```

---

## Task 4: Add Arbitrage Detection with Thresholds

**File to modify:** `src/main.rs`

Enhance `PriceState` to detect and alert on actionable arbitrage:

```rust
impl PriceState {
    // ... existing methods ...

    /// Check for arbitrage opportunity and return details
    fn check_arbitrage(&self, symbol: &str) -> Option<ArbOpportunity> {
        let uniswap_price = self.uniswap_prices.get(symbol).map(|p| *p)?;
        let nadfun_price = self.nadfun_prices.get(symbol).map(|p| *p)?;
        
        if uniswap_price <= 0.0 || nadfun_price <= 0.0 {
            return None;
        }

        let spread_pct = ((nadfun_price - uniswap_price) / uniswap_price) * 100.0;
        
        // Minimum 1.5% spread to cover fees (1% each side)
        const MIN_SPREAD: f64 = 1.5;
        
        if spread_pct.abs() > MIN_SPREAD {
            Some(ArbOpportunity {
                symbol: symbol.to_string(),
                buy_venue: if spread_pct > 0.0 { "Uniswap" } else { "Nad.fun" },
                sell_venue: if spread_pct > 0.0 { "Nad.fun" } else { "Uniswap" },
                spread_pct: spread_pct.abs(),
                buy_price: if spread_pct > 0.0 { uniswap_price } else { nadfun_price },
                sell_price: if spread_pct > 0.0 { nadfun_price } else { uniswap_price },
            })
        } else {
            None
        }
    }
}

#[derive(Debug)]
struct ArbOpportunity {
    symbol: String,
    buy_venue: &'static str,
    sell_venue: &'static str,
    spread_pct: f64,
    buy_price: f64,
    sell_price: f64,
}

impl ArbOpportunity {
    fn print(&self) {
        println!("\n🔥🔥🔥 ARBITRAGE DETECTED 🔥🔥🔥");
        println!("  Token:  {}", self.symbol);
        println!("  Spread: {:.2}%", self.spread_pct);
        println!("  Action: BUY on {} @ {:.10}", self.buy_venue, self.buy_price);
        println!("  Action: SELL on {} @ {:.10}", self.sell_venue, self.sell_price);
        println!("  Est. Profit: {:.2}% (before gas)", self.spread_pct - 2.0);
        println!("🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥\n");
    }
}
```

---

## Task 5: Update Cargo.toml (if needed)

Ensure these dependencies are present:
```toml
[dependencies]
tokio = { version = "1", features = ["full", "time"] }
```

---

## Task 6: Multi-Token Support Structure

Refactor to support multiple tokens. Create a generic token monitor:

**File to create:** `src/monitor.rs`

```rust
//! Token price monitor

use crate::config::TokenInfo;
use alloy::primitives::{Address, U256};
use dashmap::DashMap;
use std::sync::Arc;

pub struct TokenMonitor {
    pub token: TokenInfo,
    pub uniswap_price: Option<f64>,
    pub nadfun_price: Option<f64>,
    pub last_uniswap_update: Option<u64>,  // block number
    pub last_nadfun_update: Option<u64>,
}

pub struct MultiTokenState {
    tokens: DashMap<String, TokenMonitor>,
}

impl MultiTokenState {
    pub fn new(tokens: Vec<TokenInfo>) -> Self {
        let state = Self {
            tokens: DashMap::new(),
        };
        
        for token in tokens {
            state.tokens.insert(token.symbol.to_string(), TokenMonitor {
                token,
                uniswap_price: None,
                nadfun_price: None,
                last_uniswap_update: None,
                last_nadfun_update: None,
            });
        }
        
        state
    }

    pub fn update_price(&self, symbol: &str, venue: Venue, price: f64, block: u64) {
        if let Some(mut monitor) = self.tokens.get_mut(symbol) {
            match venue {
                Venue::Uniswap => {
                    monitor.uniswap_price = Some(price);
                    monitor.last_uniswap_update = Some(block);
                }
                Venue::Nadfun => {
                    monitor.nadfun_price = Some(price);
                    monitor.last_nadfun_update = Some(block);
                }
            }
        }
    }

    pub fn scan_all_for_arb(&self) -> Vec<(String, f64)> {
        let mut opportunities = vec![];
        
        for entry in self.tokens.iter() {
            let monitor = entry.value();
            if let (Some(up), Some(np)) = (monitor.uniswap_price, monitor.nadfun_price) {
                let spread = ((np - up) / up * 100.0).abs();
                if spread > 1.5 {
                    opportunities.push((entry.key().clone(), spread));
                }
            }
        }
        
        opportunities.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        opportunities
    }
}

#[derive(Clone, Copy)]
pub enum Venue {
    Uniswap,
    Nadfun,
}
```

---

## Execution Order

1. **First**: Add periodic polling to `main.rs` (Task 1) - This alone will fix the immediate issue
2. **Second**: Add arbitrage detection (Task 4) - Better alerting
3. **Third**: Create `pools.rs` for discovery (Task 3)
4. **Fourth**: Add multi-token support (Task 6)
5. **Fifth**: Add more token configs (Task 2)

---

## Testing

After implementing Task 1, run the bot and verify:
1. Prices update every 10 seconds even without swap events
2. Both Uniswap and Nad.fun prices are fetched
3. Spread is calculated and displayed

Expected output:
```
[Block 12345678] CHOG/WMON (updated by POLL)
  Nad.fun:   0.1503248842 MON
  Uniswap:   0.1863896311 MON
  Spread:    -19.35%
  >>> ARBITRAGE: Buy on Nad.fun, Sell on Uniswap <<<
```

---

## Important Notes

1. **The 24% spread you're seeing IS REAL** but the V3 pool has only ~$5K liquidity
2. **Trading that arb would move the V3 price significantly** - size your trades accordingly
3. **Consider V4 pools** - They have more liquidity (WMON/WETH has $2.2M on V4)
4. **MON volatility** - 33% daily move means more opportunities but also more risk

---

## Future Enhancements (Not in Scope)

- Add Uniswap V4 pool monitoring (different ABI)
- Add execution logic (swap contracts)
- Add MEV protection (private RPC)
- Add position sizing based on liquidity depth
