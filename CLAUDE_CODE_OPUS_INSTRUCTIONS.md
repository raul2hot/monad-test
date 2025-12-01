# Claude Code Opus: Monad Arbitrage MVP Development Instructions

**Date:** December 1, 2025  
**Status:** MVP Running - Needs DEX Address Fixes & Feature Improvements  
**Project Path:** `monad-arb-mvp/`

---

## 📊 Current State Analysis

### What's Working
- ✅ RPC Connection to Monad Mainnet (Chain ID: 143)
- ✅ Successfully connecting to Alchemy RPC
- ✅ PancakeSwap V3 pool discovery (finding 3 pools)
- ✅ Graph construction and Bellman-Ford cycle detection
- ✅ Price calculation with decimal adjustment
- ✅ Basic logging and output

### What's NOT Working
- ❌ **Uniswap V3**: Returns 0 pools (factory address likely incorrect for Monad)
- ❌ **LFJ (TraderJoe)**: Returns 0 pools (factory address likely incorrect for Monad)
- ⚠️ Only 3 pools found = limited arbitrage opportunities
- ⚠️ Graph has only 4 nodes, 6 edges - too sparse for meaningful arbitrage

### Current Output (Sample)
```
Found 3 PancakeSwap V3 pools
[Iteration 1] Graph: 4 nodes, 6 edges (3 pools)
No arbitrage opportunities found above threshold
```

---

## 🔴 CRITICAL ISSUE: DEX Contract Addresses

The project is using **Ethereum mainnet CREATE2 addresses** for Uniswap V3, which may NOT be correct for Monad Mainnet.

### Current (Incorrect) Addresses in `src/config.rs`:
```rust
// These are Ethereum mainnet addresses - NEED VERIFICATION FOR MONAD
pub mod uniswap_v3 {
    pub const FACTORY: Address = address!("1F98431c8aD98523631AE4a59f267346ea31F984");  // ❌ Ethereum
    // ...
}

pub mod pancakeswap_v3 {
    pub const FACTORY: Address = address!("0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865");  // ⚠️ Needs verification
    // ...
}

pub mod lfj {
    pub const LB_FACTORY: Address = address!("8e42f2F4101563bF679975178e880FD87d3eFd4e");  // ❌ Likely wrong
    // ...
}
```

---

## 🎯 Priority Tasks

### Phase 1: Fix Contract Addresses (CRITICAL)

#### Task 1.1: Verify DEX Contract Addresses on Monad

**Steps:**
1. Go to MonadVision Explorer: https://monadvision.com (or https://gmonads.com)
2. Search for verified DEX contracts
3. Check the monad-crypto/protocols GitHub repo: https://github.com/monad-crypto/protocols/tree/main/mainnet
4. Look for:
   - Uniswap V3 Factory
   - Uniswap V3 QuoterV2
   - PancakeSwap V3 Factory (verify current address)
   - LFJ (TraderJoe) LB Factory

**Expected Monad-specific addresses to find:**
- PancakeSwap is confirmed LIVE on Monad - verify factory at `0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865`
- Uniswap is confirmed LIVE on Monad (per Uniswap Labs blog) - need to find correct factory address
- LFJ/TraderJoe status needs verification

#### Task 1.2: Update `src/config.rs` with Verified Addresses

```rust
// TODO: Replace with verified Monad Mainnet addresses
pub mod uniswap_v3 {
    use super::*;
    
    // VERIFY ON MONAD EXPLORER
    pub const FACTORY: Address = address!("REPLACE_WITH_VERIFIED_ADDRESS");
    pub const SWAP_ROUTER: Address = address!("REPLACE_WITH_VERIFIED_ADDRESS");
    pub const QUOTER_V2: Address = address!("REPLACE_WITH_VERIFIED_ADDRESS");
    
    pub const FEE_TIERS: [u32; 4] = [100, 500, 3000, 10000];
}
```

### Phase 2: Add More DEXes (HIGH PRIORITY)

#### Task 2.1: Add Kuru Exchange Support

Kuru is Monad's hybrid CLOB+AMM DEX with significant volume ($11M+ daily).

**Create `src/dex/kuru.rs`:**
```rust
// Kuru uses a different architecture - hybrid CLOB+AMM
// May require different integration approach

// Key points:
// - Orderbook for price discovery
// - AMM for execution
// - Need to find contract addresses on MonadVision

// TODO: Research Kuru's smart contract interface
// Check: https://kuru.io or https://docs.kuru.io
```

#### Task 2.2: Add DEX Aggregator Support (Azaar, Bebop)

DEX aggregators can help find best routes across multiple DEXes.

#### Task 2.3: Verify and Fix LFJ Integration

LFJ uses Liquidity Book (LB) pools with bin-based pricing. Current implementation may have issues with:
- Factory address verification
- Bin step handling
- Price conversion from Q128.128 format

---

## 🛠️ Technical Improvements

### Task 3.1: Pool Discovery Improvements

**Problem:** Only finding 3 pools across all DEXes

**Solutions:**
1. **Query all existing pools from factory events:**
```rust
// Instead of checking specific token pairs, query PoolCreated events
// from factory contracts to discover all available pools

async fn discover_pools_from_events(&self) -> eyre::Result<Vec<Address>> {
    // Query PoolCreated events from block 0 to latest
    // This gives us ALL pools that exist, not just assumed pairs
}
```

2. **Add dynamic token discovery:**
```rust
// Start with known tokens, but also discover tokens from existing pools
async fn discover_tokens(&self) -> Vec<Address> {
    // Query pools to find all traded tokens
}
```

### Task 3.2: Add Pool Subgraph Integration (Optional)

Some DEXes have subgraphs that provide easier pool discovery:
- Check if Uniswap/PancakeSwap have Monad subgraphs
- Query TheGraph for pool data

### Task 3.3: Improve Logging

Add debug output for:
- Which factory addresses are being queried
- RPC call results (success/failure)
- Individual pool discovery attempts

```rust
// Example improved logging
tracing::debug!(
    "Querying {} factory at {} for pair {}/{} with fee {}",
    self.dex(),
    self.factory,
    tokens::symbol(token0),
    tokens::symbol(token1),
    fee
);
```

---

## 📝 Code Changes Required

### File: `src/config.rs`

**Add address verification helper:**
```rust
/// Helper to verify a contract exists at an address
pub async fn verify_contract<P: Provider>(provider: &P, address: Address) -> bool {
    match provider.get_code(address).await {
        Ok(code) => !code.is_empty(),
        Err(_) => false,
    }
}
```

### File: `src/main.rs`

**Add startup verification:**
```rust
// At startup, verify DEX factory contracts exist
async fn verify_dex_contracts(provider: &impl Provider) -> eyre::Result<()> {
    info!("Verifying DEX factory contracts...");
    
    let factories = [
        ("Uniswap V3", contracts::uniswap_v3::FACTORY),
        ("PancakeSwap V3", contracts::pancakeswap_v3::FACTORY),
        ("LFJ", contracts::lfj::LB_FACTORY),
    ];
    
    for (name, address) in factories {
        let has_code = config::verify_contract(provider, address).await;
        if has_code {
            info!("✅ {} factory verified at {}", name, address);
        } else {
            warn!("❌ {} factory NOT FOUND at {} - check address!", name, address);
        }
    }
    
    Ok(())
}
```

### File: `src/dex/mod.rs`

**Add pool discovery trait method:**
```rust
#[async_trait]
pub trait DexClient: Send + Sync {
    /// Get all pools for given token pairs
    async fn get_pools(&self, tokens: &[Address]) -> eyre::Result<Vec<Pool>>;
    
    /// Get the DEX identifier
    fn dex(&self) -> Dex;
    
    /// NEW: Discover all pools from factory events
    async fn discover_all_pools(&self) -> eyre::Result<Vec<Address>> {
        // Default implementation returns empty - override in implementations
        Ok(Vec::new())
    }
    
    /// NEW: Verify factory contract is deployed
    async fn verify_factory(&self) -> bool;
}
```

---

## 🔍 Debugging Steps

### Step 1: Verify RPC Connection & Contract Bytecode

Add this test at startup:
```rust
// Check if factory contracts have bytecode
let factory_code = provider.get_code(contracts::uniswap_v3::FACTORY).await?;
if factory_code.is_empty() {
    error!("Uniswap V3 factory has no code at specified address!");
} else {
    info!("Uniswap V3 factory code size: {} bytes", factory_code.len());
}
```

### Step 2: Test Pool Query Directly

```rust
// Direct test of getPool call
let factory = IUniswapV3Factory::new(FACTORY, &provider);
let pool = factory
    .getPool(tokens::WMON, tokens::USDC, 3000)
    .call()
    .await?;
info!("WMON/USDC 0.3% pool address: {}", pool._0);
```

### Step 3: Enable Trace Logging

```bash
# Run with trace logging to see all RPC calls
RUST_LOG=monad_arb_mvp=trace cargo run --release
```

---

## 📋 Research Tasks

### Task R1: Find Correct Monad Contract Addresses

1. **MonadVision Explorer:**
   - Go to https://monadvision.com
   - Search for "Uniswap" or "PancakeSwap"
   - Find verified factory contracts

2. **Monad Protocols Repo:**
   - Check https://github.com/monad-crypto/protocols/tree/main/mainnet
   - Look for Uniswap.json, PancakeSwap.json, etc.
   - These contain official contract addresses

3. **DEX Documentation:**
   - Check Uniswap docs for Monad deployment addresses
   - Check PancakeSwap docs
   - Check TraderJoe/LFJ docs

### Task R2: Identify Active DEXes on Monad

Based on research, these DEXes are confirmed on Monad Mainnet:
- **Uniswap V3** - Confirmed live (Uniswap Labs blog, Nov 24, 2025)
- **PancakeSwap V3** - Confirmed live
- **Kuru Exchange** - Native Monad DEX
- **LFJ (TraderJoe)** - Needs verification
- **Ambient Finance** - Concentrated liquidity
- **Monday Trade** - Hybrid DEX

### Task R3: Check Pool Liquidity

Even if pools exist, they may have insufficient liquidity. Check:
- TVL on each pool
- Adjust MIN_LIQUIDITY threshold if needed
- Consider querying DeFiLlama for Monad TVL data

---

## 🏗️ Architecture Improvements

### Current Flow
```
Main Loop → Fetch Pools → Build Graph → Detect Cycles → Log
                 ↓
         (Only PancakeSwap working)
```

### Improved Flow
```
Startup:
1. Verify all factory contracts exist
2. Discover ALL pools from factory events (not just token pairs)
3. Cache pool addresses

Main Loop:
1. Refresh pool states (prices, liquidity)
2. Build graph from cached pools
3. Detect cycles
4. Filter by confidence score
5. Log opportunities
```

---

## 📁 New Files to Create

### `src/discovery.rs` - Pool Discovery Module
```rust
//! Pool discovery from factory events and DEX APIs

use alloy::primitives::Address;

pub struct PoolDiscovery {
    discovered_pools: Vec<Address>,
    last_discovery_block: u64,
}

impl PoolDiscovery {
    /// Discover pools from factory PoolCreated events
    pub async fn discover_from_events<P: Provider>(
        &mut self,
        provider: &P,
        factory: Address,
        from_block: u64,
    ) -> eyre::Result<Vec<Address>> {
        // Query PoolCreated events
        // Parse pool addresses from event logs
        // Return new pools
        todo!()
    }
}
```

### `src/dex/kuru.rs` - Kuru DEX Client
```rust
//! Kuru Exchange client - Monad's native hybrid CLOB+AMM DEX

use alloy::{primitives::Address, providers::Provider};
use async_trait::async_trait;

use super::{Dex, DexClient, Pool};

// TODO: Define Kuru-specific contract interfaces
// Kuru uses orderbook + AMM hybrid

pub struct KuruClient<P> {
    provider: P,
    router: Address,  // TODO: Find correct address
}

#[async_trait]
impl<P: Provider + Clone + Send + Sync> DexClient for KuruClient<P> {
    async fn get_pools(&self, _tokens: &[Address]) -> eyre::Result<Vec<Pool>> {
        // TODO: Implement Kuru pool discovery
        Ok(Vec::new())
    }

    fn dex(&self) -> Dex {
        Dex::Kuru
    }
}
```

---

## 🧪 Testing Commands

```bash
# Build the project
cd monad-arb-mvp
cargo build --release

# Run with info logging
RUST_LOG=monad_arb_mvp=info cargo run --release

# Run with debug logging (more verbose)
RUST_LOG=monad_arb_mvp=debug cargo run --release

# Run with trace logging (all details)
RUST_LOG=monad_arb_mvp=trace cargo run --release

# Check for compilation errors
cargo check

# Run clippy lints
cargo clippy
```

---

## 📚 Reference Links

| Resource | URL |
|----------|-----|
| Monad Protocols Repo | https://github.com/monad-crypto/protocols |
| Monad Token List | https://github.com/monad-crypto/token-list |
| MonadVision Explorer | https://monadvision.com |
| Monad Docs | https://docs.monad.xyz |
| Uniswap V3 Docs | https://docs.uniswap.org/contracts/v3 |
| PancakeSwap V3 Docs | https://developer.pancakeswap.finance/contracts/v3 |
| Kuru Exchange | https://kuru.io |

---

## ✅ Success Criteria

The MVP is working correctly when:

1. **Pool Discovery:**
   - [ ] Finding pools from Uniswap V3 (not just PancakeSwap)
   - [ ] Finding pools from LFJ
   - [ ] Total pools discovered > 10

2. **Graph Construction:**
   - [ ] Graph has > 10 edges
   - [ ] All major token pairs represented

3. **Arbitrage Detection:**
   - [ ] Detecting actual profitable cycles (even small ones)
   - [ ] Cross-DEX opportunities identified

4. **Logging:**
   - [ ] Clear indication when opportunities found
   - [ ] Pool addresses and DEXes clearly identified

---

## 🚨 Important Notes

1. **Monad is 1 week old** - Ecosystem is still developing, liquidity is limited
2. **Probabilistic Execution** - Monad's deferred execution means simulated profits may not match actual
3. **No MEV Protection** - aPriori's searcher API is not public yet
4. **Flash Loan Limits** - Neverland has ~$1M TVL only

---

## 🔄 Next Session Checklist

When resuming work on this project:

1. [ ] Verify current state: `cargo run --release`
2. [ ] Check how many pools are being found
3. [ ] Research correct contract addresses on MonadVision
4. [ ] Update `src/config.rs` with verified addresses
5. [ ] Test each DEX individually
6. [ ] Add startup contract verification
7. [ ] Consider adding Kuru DEX support

---

*Last Updated: December 1, 2025*
