# Monad Arbitrage Bot - MVP Phase 1: Price Monitor

## Objective

Build a Rust application that monitors WMON/USDC prices across multiple DEXes on Monad mainnet, using Multicall for efficient batched RPC calls, polling every second, and displaying price spreads sorted from highest to lowest.

---

## Project Setup

- Use the existing Cargo.toml in the project (alloy, tokio, etc.)
- Create a `.env` file based on `.env.example` with `MONAD_RPC_URL` pointing to Alchemy HTTP endpoint

---

## Core Token Addresses (Monad Mainnet - Chain ID 143)

| Token | Address |
|-------|---------|
| WMON | `0x3bd359C1119dA7Da1D913D1C4D2B7c461115433A` |
| USDC | `0x754704Bc059F8C67012fEd69BC8A327a5aafb603` |
| Multicall3 | `0xcA11bde05977b3631167028862bE2a173976CA11` |

---

## Pool Configuration

### V3 Pools (Uniswap V3 / PancakeSwap V3)

These use `slot0()` to get sqrtPriceX96:

#### 1. Uniswap
- **Address:** `0x659bd0bc4167ba25c62e05656f78043e7ed4a9da`
- **Type:** UniswapV3Pool
- **Fee:** 3000 (0.3%)

#### 2. PancakeSwap1
- **Address:** `0x63e48B725540A3Db24ACF6682a29f877808C53F2`
- **Type:** PancakeV3Pool
- **Fee:** 500 (0.05%)

#### 3. PancakeSwap2
- **Address:** `0x85717A98d195c9306BBf7c9523Ba71F044Fea0f7`
- **Type:** PancakeV3Pool
- **Fee:** 2500 (0.25%)

### Other Pools (Need Investigation - Phase 2)

#### 4. LFJ
- **Address:** `0x5e60bc3f7a7303bc4dfe4dc2220bdc90bc04fe22`
- **Type:** Liquidity Book (DLMM) - uses different price mechanism
- **Method:** Likely `getActiveId()` and `getPriceFromId()`

#### 5. Monday Trade
- **Address:** `0x8f889ba499c0a176fb8f233d9d35b1c132eb868c`
- **Type:** Unknown - check contract for price method

---

## Implementation Requirements

### 1. Project Structure

```
src/
├── main.rs           # Entry point, polling loop
├── config.rs         # Pool configurations, addresses
├── pools/
│   ├── mod.rs
│   ├── v3_pool.rs    # UniswapV3/PancakeV3 slot0() price reading
│   └── traits.rs     # Common Pool trait
├── multicall.rs      # Batch RPC calls using Multicall3
├── price.rs          # Price calculation from sqrtPriceX96
└── display.rs        # Terminal output, spread sorting
```

### 2. V3 Price Reading (Priority)

For V3 pools, call `slot0()` which returns:

```solidity
function slot0() external view returns (
    uint160 sqrtPriceX96,    // We need this
    int24 tick,
    uint16 observationIndex,
    uint16 observationCardinality,
    uint16 observationCardinalityNext,
    uint8 feeProtocol,
    bool unlocked
);
```

### 3. Price Calculation from sqrtPriceX96

```
price = (sqrtPriceX96 / 2^96)^2

For WMON/USDC where:
- WMON (token0) has 18 decimals
- USDC (token1) has 6 decimals

Adjust for decimals:
price_adjusted = price * 10^(token0_decimals - token1_decimals)
price_adjusted = price * 10^(18 - 6) = price * 10^12

This gives: 1 WMON = X USDC
```

### 4. Multicall3 Batching

Use Multicall3 to batch all `slot0()` calls into a single RPC request:

```solidity
struct Call3 {
    address target;
    bool allowFailure;
    bytes callData;
}

function aggregate3(Call3[] calldata calls) 
    returns (Result[] memory results);
```

### 5. Polling Loop

- Poll every 1 second
- Use tokio for async runtime
- Clear terminal and redraw on each update
- Show timestamp with each update

### 6. Display Format

```
═══════════════════════════════════════════════════════════════
  WMON/USDC Price Monitor | Monad Mainnet | 2024-12-04 10:30:45
═══════════════════════════════════════════════════════════════

  DEX              │ Price (USDC) │ vs Best    │ Fee
  ─────────────────┼──────────────┼────────────┼────────
  PancakeSwap1     │ 0.03725      │ BEST       │ 0.05%
  Uniswap          │ 0.03720      │ -0.13%     │ 0.30%
  PancakeSwap2     │ 0.03718      │ -0.19%     │ 0.25%

═══════════════════════════════════════════════════════════════
  SPREAD OPPORTUNITIES (sorted by potential profit)
═══════════════════════════════════════════════════════════════

  Buy @ Uniswap (0.03720) → Sell @ PancakeSwap1 (0.03725)
  Gross Spread: 0.13% | Net (after fees): -0.22% ❌

  Buy @ PancakeSwap2 (0.03718) → Sell @ PancakeSwap1 (0.03725)  
  Gross Spread: 0.19% | Net (after fees): -0.11% ❌

═══════════════════════════════════════════════════════════════
  Polling: 1s | RPC Calls: 1 (batched) | Last update: 45ms
═══════════════════════════════════════════════════════════════
```

### 7. Error Handling

- Handle RPC timeouts gracefully
- If one pool fails, continue with others
- Show connection status

### 8. Start with V3 Pools Only

For MVP, implement only the 3 V3 pools (Uniswap, PancakeSwap1, PancakeSwap2).
Add placeholder for LFJ and Monday Trade - we'll implement those after V3 works.

---

## Technical Notes

### Token Order

- WMON (`0x3bd...`) < USDC (`0x754...`) by address
- Therefore: **token0 = WMON, token1 = USDC**
- sqrtPriceX96 represents: `sqrt(token1/token0)` = `sqrt(USDC/WMON)`

### Alloy Setup

Use alloy with these features for Monad:
- HTTP provider (not WebSocket for MVP)
- `sol!` macro for contract interfaces
- Multicall batching

### Environment

```env
MONAD_RPC_URL=https://monad-mainnet.g.alchemy.com/v2/YOUR_KEY
```

---

## Deliverables

1. ✅ Working price monitor for 3 V3 pools
2. ✅ Multicall batching (1 RPC call per poll)
3. ✅ 1-second polling interval
4. ✅ Sorted spread display
5. ✅ Net profit calculation (accounting for fees)

---

## Do NOT

- ❌ Implement trade execution (Phase 2)
- ❌ Use WebSocket (HTTP polling for MVP)
- ❌ Over-engineer - keep it simple and working

---

## Next Steps (After MVP)

1. Add LFJ pool (Liquidity Book contract interface)
2. Add Monday Trade pool (identify AMM type)
3. Phase 2: Trade execution implementation
