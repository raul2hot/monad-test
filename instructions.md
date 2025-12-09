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

### CRITICAL: Failed Optimization Attempts

⚠️ **DO NOT** implement these naive optimizations - they have been tested and fail:

1. **Hardcoded gas limit (80% failure rate)**
   - Why it fails: Gas varies significantly by route. LFJ bin traversal, UniV3 tick crossings, and pool liquidity state all affect gas consumption.
   - Symptom: Transactions revert with "out of gas"

2. **Remove balance queries / use estimated profit (100% loss rate)**
   - Why it fails: Estimated profit uses static prices, but actual execution has slippage and price impact. The price moves during the 500ms+ execution window.
   - Symptom: Bot reports "profit" but actual balance delta is negative

### Validated Safe Optimizations

✅ These approaches maintain safety while reducing latency:

1. **Gas Cache per Route**
   - Cache gas estimates per (sell_router, buy_router) pair
   - TTL: 30 seconds (routes don't change gas profile often)
   - Add small buffer (8-12%) on cached values
   - Still estimate on cache miss

2. **Parallel Operations**
   - Fetch gas price + init nonce + fetch prices in parallel (already done)
   - Build calldata (CPU) while RPC calls are in flight

3. **Aggressive Receipt Polling**
   - Monad has fast blocks (~500ms)
   - Poll at 5ms intervals instead of 20ms
   - Can save 50-100ms on average

4. **Deferred Post-Balance Query**
   - Return immediately after TX confirmation
   - Query actual profit async for logging (don't block execution)
   - Use estimated profit for decision-making

5. **Pre-Built Calldata Templates**
   - For common routes, pre-build calldata templates
   - Only substitute amounts at execution time

## Code Patterns to Follow

### Nonce Management
```rust
// Always use the global nonce manager
use crate::nonce::{init_nonce, next_nonce};

// Initialize once at startup
init_nonce(&provider, signer_address).await?;

// Get next nonce atomically (never fetch from RPC during execution)
let nonce = next_nonce();
```

### Transaction Building (Monad-specific)
```rust
const MONAD_CHAIN_ID: u64 = 143;

let tx = alloy::rpc::types::TransactionRequest::default()
    .to(contract_address)
    .from(signer_address)
    .input(alloy::rpc::types::TransactionInput::new(calldata))
    .gas_limit(gas_estimate)
    .nonce(next_nonce())  // Use cached nonce
    .max_fee_per_gas(gas_price + (gas_price / 10))
    .max_priority_fee_per_gas(gas_price / 10)
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
```

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
```

## Environment Variables Required

```bash
MONAD_RPC_URL=https://your-monad-rpc-endpoint
MONAD_WS_URL=wss://your-monad-ws-endpoint  # Optional, for WebSocket triggers
PRIVATE_KEY=0x...  # Wallet private key
```

## Success Metrics

| Metric | Current | Target |
|--------|---------|--------|
| Execution time | 600-800ms | <300ms |
| Success rate | ~20% | >80% |
| Profit accuracy | -40 bps error | <5 bps error |

## Key Constraints

1. **Monad is EVM-compatible** but has unique characteristics:
   - Fast block times (~500ms)
   - Optimistic execution (use `monadNewHeads` WebSocket for triggers)
   - Standard gas model

2. **DEX Pool Fees** (in basis points × 100):
   - Uniswap: 500 (0.05%)
   - PancakeSwap: 500 (0.05%)
   - LFJ: Variable (bin-based)
   - MondayTrade: 3000 (0.3%)

3. **Token Addresses** (Monad Mainnet):
   - WMON: Check `config.rs` for `WMON_ADDRESS`
   - USDC: Check `config.rs` for `USDC_ADDRESS`
   - Atomic Arb Contract: Check `config.rs` for `ATOMIC_ARB_CONTRACT`

## When Making Changes

1. **Always test on small amounts first** (0.01-0.1 WMON)
2. **Check gas estimates** before removing gas estimation
3. **Verify actual balance delta** matches expected profit
4. **Log timing breakdowns** to identify bottlenecks
5. **Use `--force` flag** for testing execution path without profit requirements

## Common Issues & Solutions

### "Nonce too low"
- Nonce manager got out of sync
- Solution: Restart bot, or call `reset_nonce()`

### "Gas estimation failed"
- Usually means the arb would be unprofitable (contract reverts)
- This is expected behavior - skip this opportunity

### "Transaction reverted"
- Price moved during execution (MEV or natural)
- Slippage protection triggered
- Consider increasing slippage or reducing execution time

### Actual profit differs from estimated
- Price impact not accounted for
- Slippage during execution
- Solution: Use actual balance queries for profit calculation

## Next Steps for Optimization

1. Implement gas caching per route pair
2. Add aggressive 5ms receipt polling
3. Parallelize remaining sequential operations
4. Consider WebSocket-triggered execution (`mev-ultra` command)
5. Profile each RPC call to find remaining bottlenecks