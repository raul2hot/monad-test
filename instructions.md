# Monad Mainnet Arbitrage Bot - Claude Code Instructions

## Project Overview

You are working on a **Monad mainnet DEX-to-DEX arbitrage bot** written in Rust. The goal is to detect price discrepancies between DEXes (Uniswap, PancakeSwap, LFJ, MondayTrade) and execute profitable atomic arbitrage trades.

**Mission Statement**: We will succeed in this - no matter what it takes!

## Current Architecture

### Key Files
- `src/execution/atomic_arb.rs` - Atomic arbitrage execution via smart contract (single TX)
- `src/main.rs` - CLI commands and orchestration
- `src/nonce.rs` - Global nonce management for fast TX submission
- `src/config.rs` - Router configs, pool addresses, contract addresses
- `src/multicall.rs` - Batched price fetching
- `src/execution/routers.rs` - DEX-specific swap calldata builders

### Execution Flow
1. Fetch prices from all DEXes via multicall
2. Calculate spreads and identify opportunities
3. Build atomic arb calldata (sell on DEX A, buy on DEX B)
4. Execute via `AtomicArb` smart contract (single TX, MEV-resistant)
5. Verify profit via balance delta

### Smart Contract Interface
```solidity
function executeArbUnchecked(
    uint8 sellRouter,
    bytes calldata sellRouterData,
    uint8 buyRouter,
    uint24 buyPoolFee,
    uint256 minWmonOut
) external returns (int256 profit);

function getBalances() external view returns (uint256 wmon, uint256 usdc);
```

## Critical Problem: Execution Latency

Current execution takes **600-800ms**. We need **sub-300ms** to be competitive.

### Timing Breakdown (Current)
| Operation | Time | Notes |
|-----------|------|-------|
| Pre-balance query | 50-100ms | 1 RPC call |
| Gas estimation | 200-300ms | Simulation RPC |
| TX send | 10ms | Fast |
| Receipt polling | 200-300ms | 20ms intervals |
| Post-balance query | 50-100ms | 1 RPC call |
| **Total** | **600-800ms** | Too slow |

---

## CRITICAL: Failed Optimization Attempts

⚠️ **DO NOT** implement these naive optimizations - they have been tested and fail:

### 1. Hardcoded Gas Limit (80% failure rate)
- **Why it fails**: Gas varies significantly by route. LFJ bin traversal, UniV3 tick crossings, and pool liquidity state all affect gas consumption.
- **Symptom**: Transactions revert with "out of gas"

### 2. Remove Balance Queries / Use Estimated Profit (100% loss rate)
- **Why it fails**: Estimated profit uses static prices, but actual execution has slippage and price impact. The price moves during the 500ms+ execution window.
- **Symptom**: Bot reports "profit" but actual balance delta is negative

---

## Validated Safe Optimizations

### 1. Adaptive Gas Cache with Spread-Aware Invalidation ⭐ CRITICAL

The naive approach of "just cache gas estimates" fails because **high spread = volatile pool state = stale gas estimates**.

#### The Problem
| Pool State | Gas Consumption | Risk |
|------------|-----------------|------|
| Normal (2 ticks traversed) | ~350k gas | Low |
| Volatile (5 ticks traversed) | ~500k gas | High |
| Cached estimate + 8% buffer | ~378k gas | **REVERTS** |

#### The Solution: Spread-Aware Cache Rules

```rust
/// Gas cache entry with spread context
struct GasCacheEntry {
    gas_estimate: u64,
    timestamp_ms: u128,
    spread_bps_at_cache: i32,  // Spread when this estimate was captured
}

/// Check if cache is valid for current market conditions
fn is_cache_valid(entry: &GasCacheEntry, current_spread_bps: i32) -> bool {
    let now = now_ms();
    
    // TTL check (base: 30 seconds)
    if now - entry.timestamp_ms > GAS_CACHE_TTL_MS {
        return false;
    }
    
    // CRITICAL: Invalidate if spread increased significantly
    // High spread = volatile state = gas estimate is stale
    let spread_delta = current_spread_bps - entry.spread_bps_at_cache;
    if spread_delta > 20 {  // Spread increased by >20 bps since cache
        return false;  // Force fresh estimate
    }
    
    true
}
```

#### Gas Strategy by Spread Level

| Spread Level | Cache Behavior | Gas Buffer | TTL | Rationale |
|--------------|----------------|------------|-----|-----------|
| **Low (<15 bps)** | Use cache aggressively | 8% | 30s | Stable state, low profit anyway |
| **Medium (15-30 bps)** | Use cache with caution | 15% | 10s | Some volatility |
| **High (>30 bps)** | **ALWAYS fresh estimate** | 20% | N/A | This is where profit is - don't lose to gas errors |

```rust
/// Determine gas strategy based on current spread
fn gas_strategy(spread_bps: i32, cached_gas: Option<u64>) -> GasDecision {
    match spread_bps {
        // Low spread: use cache aggressively (profit is low anyway)
        s if s < 15 => {
            if let Some(cached) = cached_gas {
                GasDecision::UseCached { 
                    gas_limit: cached * 108 / 100,  // 8% buffer
                    source: GasSource::Cached 
                }
            } else {
                GasDecision::FetchFresh { buffer_percent: 10 }
            }
        }
        
        // Medium spread: use cache but with larger buffer
        s if s < 30 => {
            if let Some(cached) = cached_gas {
                GasDecision::UseCached { 
                    gas_limit: cached * 115 / 100,  // 15% buffer
                    source: GasSource::CachedWithBuffer 
                }
            } else {
                GasDecision::FetchFresh { buffer_percent: 15 }
            }
        }
        
        // High spread: ALWAYS fresh estimate - this is where the money is
        _ => GasDecision::FetchFresh { buffer_percent: 20 },
    }
}

enum GasDecision {
    UseCached { gas_limit: u64, source: GasSource },
    FetchFresh { buffer_percent: u64 },
}
```

#### Gas Price Bidding During Competition

When spreads are high, other bots are competing. Adjust priority fee accordingly:

```rust
fn calculate_gas_price(base_gas_price: u128, spread_bps: i32) -> (u128, u128) {
    // Base priority fee
    let base_priority = base_gas_price / 10;
    
    // Boost priority fee based on spread (more competition = higher spread)
    // Add 1 gwei per 10 bps of spread
    let priority_boost = (spread_bps as u128 / 10) * 1_000_000_000;  // gwei to wei
    
    let max_fee = base_gas_price + base_priority + priority_boost;
    let priority_fee = base_priority + priority_boost;
    
    (max_fee, priority_fee)
}
```

### 2. Parallel Operations

Already implemented but ensure these run concurrently:

```rust
// GOOD: Parallel initialization
let (gas_result, nonce_result, prices_result) = tokio::join!(
    provider.get_gas_price(),
    init_nonce(&provider, signer_address),
    get_current_prices(&provider)
);

// BAD: Sequential (adds 300ms+)
let gas = provider.get_gas_price().await?;
init_nonce(&provider, signer_address).await?;
let prices = get_current_prices(&provider).await?;
```

### 3. Aggressive Receipt Polling

Monad has fast blocks (~500ms). Poll aggressively:

```rust
const RECEIPT_POLL_MS: u64 = 5;      // Was 20ms - saves 50-100ms average
const RECEIPT_TIMEOUT_MS: u64 = 10_000;

async fn wait_for_receipt_fast<P: Provider>(
    provider: &P,
    tx_hash: TxHash,
) -> Result<TransactionReceipt> {
    let mut poll = interval(Duration::from_millis(RECEIPT_POLL_MS));
    let deadline = Instant::now() + Duration::from_millis(RECEIPT_TIMEOUT_MS);
    
    while Instant::now() < deadline {
        poll.tick().await;
        if let Some(receipt) = provider.get_transaction_receipt(tx_hash).await? {
            return Ok(receipt);
        }
    }
    
    Err(eyre!("Receipt timeout"))
}
```

### 4. Deferred Post-Balance Query

Return immediately after TX confirmation. Query actual profit async for logging:

```rust
pub struct TurboArbResult {
    pub tx_hash: String,
    pub success: bool,
    pub estimated_profit_wmon: f64,  // Returned immediately
    pub gas_used: u64,
    pub execution_time_ms: u128,
    // ... other fields
    
    // Populated async after return (for logging only)
    pub actual_profit_wmon: Option<f64>,
}

// In execution:
let result = TurboArbResult {
    tx_hash: format!("{:?}", tx_hash),
    success: receipt.status(),
    estimated_profit_wmon: expected_wmon_back - amount,
    // ...
    actual_profit_wmon: None,  // Will be filled by background task
};

// Spawn background task to query actual profit (doesn't block return)
let provider_clone = provider.clone();
tokio::spawn(async move {
    if let Ok((wmon_after, _)) = query_balances_fast(&provider_clone).await {
        let actual = wmon_after - wmon_before;
        // Log actual vs estimated for analysis
        tracing::info!(
            estimated = %estimated_profit,
            actual = %actual,
            delta_bps = %(actual - estimated_profit) / amount * 10000.0,
            "Profit verification"
        );
    }
});

return Ok(result);  // Return immediately
```

### 5. Pre-Built Calldata Templates

For frequently used routes, pre-build calldata templates:

```rust
/// Pre-built calldata template (amounts are placeholders)
struct CalldataTemplate {
    sell_router: u8,
    buy_router: u8,
    buy_pool_fee: u32,
    // Template with placeholder amounts (substituted at execution time)
}

impl CalldataTemplate {
    /// Finalize template with actual amounts (CPU-only, <1ms)
    fn finalize(&self, wmon_in: U256, min_usdc_out: U256, min_wmon_out: U256) -> Bytes {
        // Build final calldata without RPC calls
    }
}

// Pre-build templates at startup
lazy_static! {
    static ref TEMPLATES: HashMap<(u8, u8), CalldataTemplate> = {
        let mut m = HashMap::new();
        // Uniswap -> PancakeSwap
        m.insert((0, 1), CalldataTemplate::new(0, 1, 500));
        // PancakeSwap -> Uniswap
        m.insert((1, 0), CalldataTemplate::new(1, 0, 500));
        // ... other common routes
        m
    };
}
```

---

## Code Patterns to Follow

### Nonce Management
```rust
use crate::nonce::{init_nonce, next_nonce};

// Initialize once at startup
init_nonce(&provider, signer_address).await?;

// Get next nonce atomically (never fetch from RPC during execution)
let nonce = next_nonce();
```

### Transaction Building (Monad-specific)
```rust
const MONAD_CHAIN_ID: u64 = 143;

let (max_fee, priority_fee) = calculate_gas_price(gas_price, spread_bps);

let tx = alloy::rpc::types::TransactionRequest::default()
    .to(contract_address)
    .from(signer_address)
    .input(alloy::rpc::types::TransactionInput::new(calldata))
    .gas_limit(gas_limit)
    .nonce(next_nonce())
    .max_fee_per_gas(max_fee)
    .max_priority_fee_per_gas(priority_fee)
    .with_chain_id(MONAD_CHAIN_ID);
```

### Router Type Mapping
```rust
#[repr(u8)]
pub enum ContractRouter {
    Uniswap = 0,
    PancakeSwap = 1,
    MondayTrade = 2,
    LFJ = 3,
}
```

### Wei Conversion
```rust
fn to_wei(amount: f64, decimals: u8) -> U256 {
    let multiplier = U256::from(10u64).pow(U256::from(decimals));
    let amount_scaled = (amount * 1e18) as u128;
    U256::from(amount_scaled) * multiplier / U256::from(10u64).pow(U256::from(18u8))
}

fn from_wei(amount: U256, decimals: u8) -> f64 {
    let divisor = 10u64.pow(decimals as u32) as f64;
    let amount_u128: u128 = amount.try_into().unwrap_or(0);
    amount_u128 as f64 / divisor
}
```

---

## Testing Commands

```bash
# Test atomic arb (manual)
cargo run -- atomic-arb --sell-dex uniswap --buy-dex pancakeswap1 --amount 0.1 --slippage 150

# Auto arb with monitoring
cargo run -- auto-arb --min-spread-bps 10 --amount 0.1 --slippage 150 --max-executions 5

# Check contract balances
cargo run -- contract-balance

# Fund contract with WMON
cargo run -- fund-contract --amount 10.0

# Price monitor dashboard
cargo run -- dashboard --min-spread 5 --refresh-ms 100

# MEV Ultra (WebSocket triggered)
cargo run -- mev-ultra --amount 0.1 --slippage 150 --min-spread 10
```

## Environment Variables Required

```bash
MONAD_RPC_URL=https://your-monad-rpc-endpoint
MONAD_WS_URL=wss://your-monad-ws-endpoint  # For WebSocket triggers
PRIVATE_KEY=0x...  # Wallet private key
```

---

## Success Metrics

| Metric | Current | Target |
|--------|---------|--------|
| Execution time | 600-800ms | <300ms |
| Success rate | ~20% | >80% |
| Profit accuracy | -40 bps error | <5 bps error |
| Gas revert rate | High on spikes | <5% |

---

## Key Constraints

### Monad Characteristics
- **Fast block times**: ~500ms
- **Optimistic execution**: Use `monadNewHeads` WebSocket for triggers
- **Standard gas model**: EVM-compatible

### DEX Pool Fees (basis points × 100)
| DEX | Pool Fee | Percentage |
|-----|----------|------------|
| Uniswap | 500 | 0.05% |
| PancakeSwap | 500 | 0.05% |
| LFJ | Variable | Bin-based |
| MondayTrade | 3000 | 0.3% |

### Token Addresses (Monad Mainnet)
- WMON: See `config.rs` → `WMON_ADDRESS`
- USDC: See `config.rs` → `USDC_ADDRESS`
- Atomic Arb Contract: See `config.rs` → `ATOMIC_ARB_CONTRACT`

---

## When Making Changes

1. **Always test on small amounts first** (0.01-0.1 WMON)
2. **Check gas estimates** before removing gas estimation
3. **Verify actual balance delta** matches expected profit
4. **Log timing breakdowns** to identify bottlenecks
5. **Use `--force` flag** for testing execution path without profit requirements
6. **Monitor spread level** when testing gas caching - high spreads need fresh estimates

---

## Common Issues & Solutions

### "Nonce too low"
- Nonce manager got out of sync
- Solution: Restart bot, or call `reset_nonce()`

### "Gas estimation failed"
- Usually means the arb would be unprofitable (contract reverts)
- This is expected behavior - skip this opportunity

### "Transaction reverted" / "Out of gas"
- **During high spread**: Cached gas estimate was stale
- Solution: Implement spread-aware cache invalidation (see above)
- Force fresh gas estimate when spread > 30 bps

### "Transaction reverted" / "Slippage"
- Price moved during execution (MEV or natural)
- Slippage protection triggered
- Solution: Increase slippage or reduce execution time

### Actual profit differs from estimated
- Price impact not accounted for
- Slippage during execution
- Solution: Use actual balance queries for profit calculation (but don't block on them)

---

## Implementation Priority

### Phase 1: Adaptive Gas Cache (Highest Impact)
1. Implement `GasCacheEntry` with spread context
2. Add `is_cache_valid()` with spread-aware invalidation
3. Implement `gas_strategy()` function
4. Test on various spread levels

### Phase 2: Aggressive Polling
1. Reduce receipt poll interval to 5ms
2. Test for any RPC rate limiting issues

### Phase 3: Deferred Balance Queries
1. Implement async post-balance query
2. Return estimated profit immediately
3. Log actual vs estimated for calibration

### Phase 4: Calldata Templates
1. Pre-build templates for common routes
2. Measure template finalization time
3. Integrate into execution path

---

## Architecture Decision Record

### ADR-001: Why Not Hardcode Gas?
**Decision**: Always estimate gas (with smart caching)
**Rationale**: Gas varies by 30-50% based on pool state. Hardcoded limits cause 80% revert rate.
**Trade-off**: 200-300ms latency vs reliability

### ADR-002: Why Not Skip Balance Queries?
**Decision**: Query actual balances for profit verification
**Rationale**: Estimated profit is based on static prices. Actual slippage causes 100% false profit reports.
**Trade-off**: 100-200ms latency vs accuracy

### ADR-003: Spread-Aware Gas Caching
**Decision**: Cache gas only when spreads are low/stable
**Rationale**: High spread = volatile state = stale cache. Best opportunities (high spread) need fresh estimates.
**Trade-off**: Slower on best opportunities, but actually profitable

---

## Appendix: Full Turbo Execution Flow

```
┌─────────────────────────────────────────────────────────────────┐
│                    TURBO EXECUTION FLOW                         │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  1. SPREAD DETECTED (from price monitor)                        │
│     └─ spread_bps = 45 (high)                                  │
│                                                                 │
│  2. GAS STRATEGY DECISION                                       │
│     └─ spread > 30 bps → FRESH ESTIMATE (don't use cache)      │
│                                                                 │
│  3. PARALLEL OPERATIONS                                         │
│     ├─ estimate_gas() ──────────────────┐                       │
│     └─ build_calldata() (CPU) ──────────┤ ~200ms               │
│                                         ▼                       │
│  4. CALCULATE GAS PRICE                                         │
│     └─ priority_boost = 45/10 = 4 gwei extra                   │
│                                                                 │
│  5. BUILD & SEND TX ────────────────────────── ~10ms           │
│                                                                 │
│  6. AGGRESSIVE RECEIPT POLL (5ms intervals) ─── ~100-200ms     │
│                                                                 │
│  7. RETURN IMMEDIATELY WITH ESTIMATED PROFIT                    │
│     └─ Spawn async task for actual profit logging              │
│                                                                 │
│  TOTAL: ~310-410ms (vs 600-800ms before)                       │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

---

*Last updated: Adaptive Cache Invalidation v2*