# Monad Arbitrage Bot — Phase 1: Live Price Monitor

> **For:** Claude Code Opus  
> **Goal:** Display live prices from Uniswap V3 and Nad.fun DEX via Alchemy WebSockets

---

## Network

- **Chain:** Monad Mainnet
- **Chain ID:** 143
- **Block Time:** 400ms
- **RPC:** Alchemy WebSocket (from `.env`)

---

## Contract Addresses

```rust
// Nad.fun DEX
pub const NADFUN_DEX_ROUTER: &str = "0x0B79d71AE99528D1dB24A4148b5f4F865cc2b137";
pub const NADFUN_DEX_FACTORY: &str = "0x6B5F564339DbAD6b780249827f2198a841FEB7F3";

// Uniswap V3
pub const UNISWAP_FACTORY: &str = "0x204FAca1764B154221e35c0d20aBb3c525710498";
pub const UNISWAP_QUOTER_V2: &str = "0x661E93cca42AfacB172121EF892830cA3b70F08d";

// WMON (Wrapped MON)
pub const WMON: &str = "0x3bd359C1119dA7Da1D913D1C4D2B7c461115433A";
```

---

## Target Tokens (Graduated, Cross-Venue Liquidity)

These tokens have liquidity on both Nad.fun DEX and Uniswap V3:

```rust
// Top tokens from monadvision.com / dexscreener
pub const CHOG: &str = "0x350035555E10d9AfAF1566AaebfCeD5BA6C27777";      // Chog - Monad mascot
pub const MOLANDAK: &str = "0x7B2728c04aD436153285702e969e6EfAc3a97777";  // MOLANDAK
pub const GMONAD: &str = "0x7db552eeb6b77a6babe6e0a739b5382cd653cc3e";    // GMONAD

// Nad.fun DEX Pools (token/WMON pairs)
pub const CHOG_NADFUN_POOL: &str = "0x116e7d070f1888b81e1e0324f56d6746b2d7d8f1";

// Uniswap V3 Pools (token/WMON pairs)
pub const CHOG_UNISWAP_POOL: &str = "0x745355f47db8c57e7911ef3da2e989b16039d12f";  // 1% fee tier
```

---

## Cargo.toml

```toml
[package]
name = "monad-arb-monitor"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1", features = ["full"] }
alloy = { version = "0.9", features = ["full"] }
eyre = "0.6"
dotenv = "0.15"
```

---

## .env

```env
ALCHEMY_WS_URL=wss://monad-mainnet.g.alchemy.com/v2/YOUR_API_KEY
```

---

## Project Structure

```
src/
├── main.rs           # Entry point, tokio runtime, terminal output loop
├── config.rs         # All constants and addresses above
├── provider.rs       # WebSocket connection to Alchemy
├── uniswap.rs        # Subscribe to Uniswap V3 Swap events, calculate price
└── nadfun.rs         # Subscribe to Nad.fun DEX Swap events, calculate price
```

---

## Implementation

### 1. WebSocket Connection (provider.rs)

Connect to Alchemy WebSocket using `alloy::providers::WsConnect`. Handle auto-reconnect on disconnect.

### 2. Uniswap V3 Price (uniswap.rs)

Subscribe to `Swap` events on Uniswap V3 pools:

```solidity
event Swap(
    address indexed sender,
    address indexed recipient,
    int256 amount0,
    int256 amount1,
    uint160 sqrtPriceX96,
    uint128 liquidity,
    int24 tick
);
```

Price calculation:
```
price = (sqrtPriceX96 / 2^96)^2
```

### 3. Nad.fun DEX Price (nadfun.rs)

Subscribe to `Swap` events on Nad.fun DEX pools. The pools are created by `NADFUN_DEX_FACTORY`.

Parse `amount0` and `amount1` from swap events to derive price.

### 4. Terminal Output (main.rs)

Print prices to stdout in a simple format:

```
[Block 1234567] CHOG/WMON
  Nad.fun:   0.004230 MON
  Uniswap:   0.004180 MON
  Spread:    +1.20%

[Block 1234567] MOLANDAK/WMON
  Nad.fun:   0.000190 MON
  Uniswap:   0.000188 MON
  Spread:    +1.06%
```

Just `println!` — no fancy TUI needed.

---

## Event Signatures

```rust
// Uniswap V3 Swap
// topic0: 0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67

// Standard ERC20 Transfer (for tracking)
// topic0: 0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef
```

---

## Key Notes

1. **400ms blocks** — high event frequency, design for throughput
2. **Pool discovery** — use hardcoded pool addresses above to start
3. **Price from sqrtPriceX96** — standard Uniswap V3 math
4. **Reconnect on WS disconnect** — Alchemy connections can drop

---

## Out of Scope (Phase 2)

- Bonding curve mechanics
- Trade execution
- Flash loans
- MEV protection

---

## Reference

See `Monad_Arbitrage_Bot_Final_Research.md` for full contract addresses and bonding curve details.
