# Monad Mainnet Arbitrage Opportunity Logger MVP

## Implementation Instructions for Claude Code Opus

**Date:** December 1, 2025  
**Target Platform:** Windows (Development), Monad Mainnet  
**Language:** Rust (Cargo)  
**Purpose:** Log arbitrage opportunities between DEXes to console

---

## 1. Project Overview

Build a Rust MVP that:
1. Connects to Monad Mainnet via Alchemy RPC
2. Discovers liquidity pools from multiple DEXes
3. Builds a token price graph
4. Uses Bellman-Ford algorithm to detect arbitrage cycles
5. Logs opportunities to console with profit percentages

**This is a READ-ONLY bot** - no trades will be executed. Focus on detection and logging.

---

## 2. Network Configuration

### Monad Mainnet Details

| Parameter | Value |
|-----------|-------|
| **Chain ID** | `143` |
| **Currency Symbol** | `MON` |
| **Block Time** | ~1 second (400ms blocks) |
| **Finality** | Single-slot (~800ms) |
| **EVM Compatibility** | 100% bytecode compatible |

### RPC Endpoints

```rust
// Primary - Alchemy (use your API key)
const ALCHEMY_RPC: &str = "https://monad-mainnet.g.alchemy.com/v2/<YOUR_API_KEY>";

// Alternative Public RPC (rate-limited)
const PUBLIC_RPC: &str = "https://rpc.monad.xyz";

// WebSocket (for real-time updates - Phase 2)
const ALCHEMY_WS: &str = "wss://monad-mainnet.g.alchemy.com/v2/<YOUR_API_KEY>";
```

### Block Explorer
- MonadVision: https://monadvision.com
- Alternative: https://gmonads.com

---

## 3. Token Addresses (Canonical - VERIFIED)

These are the official canonical token addresses on Monad Mainnet:

```rust
pub mod tokens {
    use alloy_primitives::address;
    
    // Native Wrapped Token
    pub const WMON: Address = address!("3bd359C1119dA7Da1D913D1C4D2B7c461115433A");
    
    // Stablecoins
    pub const USDC: Address = address!("754704Bc059F8C67012fEd69BC8A327a5aafb603");
    pub const USDT: Address = address!("e7cd86e13AC4309349F30B3435a9d337750fC82D");
    
    // Major Tokens
    pub const WETH: Address = address!("EE8c0E9f1BFFb4Eb878d8f15f368A02a35481242");
    pub const WBTC: Address = address!("0555E30da8f98308EdB960aa94C0Db47230d2B9c");
    
    // Liquid Staking Tokens (LSTs)
    pub const SMON: Address = address!("A3227C5969757783154C60bF0bC1944180ed81B9");  // Kintsu
    pub const GMON: Address = address!("8498312A6B3CbD158bf0c93AbdCF29E6e4F55081");  // Magma
    
    // Token Symbols for Display
    pub fn symbol(addr: &Address) -> &'static str {
        match *addr {
            _ if *addr == WMON => "WMON",
            _ if *addr == USDC => "USDC",
            _ if *addr == USDT => "USDT",
            _ if *addr == WETH => "WETH",
            _ if *addr == WBTC => "WBTC",
            _ if *addr == SMON => "sMON",
            _ if *addr == GMON => "gMON",
            _ => "UNKNOWN",
        }
    }
}
```

### Base Tokens for Arbitrage (Start cycles from these)
```rust
pub const BASE_TOKENS: [Address; 4] = [
    tokens::WMON,
    tokens::USDC,
    tokens::USDT,
    tokens::WETH,
];
```

---

## 4. DEX Ecosystem & Contract Addresses

### DEX Overview

| DEX | Type | Status | Priority |
|-----|------|--------|----------|
| **Uniswap V3** | AMM (Concentrated Liquidity) | ✅ Live | HIGH |
| **PancakeSwap** | AMM | ✅ Live | HIGH |
| **LFJ (Formerly TraderJoe)** | AMM | ✅ Live | MEDIUM |
| **Kuru Exchange** | Hybrid CLOB+AMM | ✅ Live | MEDIUM |
| **Monday Trade** | Hybrid | ✅ Live | LOW |
| **Curve** | StableSwap | ✅ Live | LOW |

### Uniswap V3 Contracts (PRIMARY)

Uniswap V3 uses **deterministic deployment addresses** based on CREATE2. For Monad, use:

```rust
pub mod uniswap_v3 {
    use alloy_primitives::address;
    
    // Core Contracts
    pub const FACTORY: Address = address!("1F98431c8aD98523631AE4a59f267346ea31F984");
    pub const SWAP_ROUTER: Address = address!("E592427A0AEce92De3Edee1F18E0157C05861564");
    pub const SWAP_ROUTER_02: Address = address!("68b3465833fb72A70ecDF485E0e4C7bD8665Fc45");
    pub const QUOTER_V2: Address = address!("61fFE014bA17989E743c5F6cB21bF9697530B21e");
    pub const NFT_POSITION_MANAGER: Address = address!("C36442b4a4522E871399CD717aBDD847Ab11FE88");
    
    // Pool Init Code Hash (for computing pool addresses)
    pub const POOL_INIT_CODE_HASH: [u8; 32] = hex!("e34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54");
    
    // Fee Tiers (in hundredths of a bip: 100 = 0.01%, 500 = 0.05%, 3000 = 0.3%, 10000 = 1%)
    pub const FEE_TIERS: [u32; 4] = [100, 500, 3000, 10000];
}
```

### PancakeSwap V3 Contracts

```rust
pub mod pancakeswap_v3 {
    use alloy_primitives::address;
    
    // PancakeSwap uses different deployment addresses
    pub const FACTORY: Address = address!("0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865");
    pub const SWAP_ROUTER: Address = address!("13f4EA83D0bd40E75C8222255bc855a974568Dd4");
    pub const QUOTER_V2: Address = address!("B048Bbc1Ee6b733FFfCFb9e9CeF7375518e25997");
    
    // Same init code hash as Uniswap V3
    pub const POOL_INIT_CODE_HASH: [u8; 32] = hex!("6ce8eb472fa82df5469c6ab6d485f17c3ad13c8cd7af59b3d4a8026c5ce0f7e2");
    
    pub const FEE_TIERS: [u32; 4] = [100, 500, 2500, 10000];
}
```

### LFJ (TraderJoe V2.1) Contracts

```rust
pub mod lfj {
    use alloy_primitives::address;
    
    // LFJ uses Liquidity Book (LB) pools
    pub const LB_FACTORY: Address = address!("8e42f2F4101563bF679975178e880FD87d3eFd4e");
    pub const LB_ROUTER: Address = address!("b4315e873dBcf96Ffd0acd8EA43f689D8c20fB30");
    pub const LB_QUOTER: Address = address!("64b57F4249aA99a812212cee7DAEFEDC93b02E14");
}
```

### Kuru Exchange Contracts

```rust
pub mod kuru {
    use alloy_primitives::address;
    
    // Kuru is a hybrid CLOB+AMM - orderbook for price discovery, AMM for execution
    // Note: Contract addresses need verification on MonadVision
    pub const ROUTER: Address = address!("TBD"); // Verify on monadvision.com
    pub const ORDERBOOK: Address = address!("TBD"); // Verify on monadvision.com
}
```

---

## 5. Project Structure

```
monad-arb-mvp/
├── Cargo.toml
├── .env                          # API keys (DO NOT COMMIT)
├── .gitignore
├── src/
│   ├── main.rs                   # Entry point, main loop
│   ├── config.rs                 # Network & token configuration
│   ├── provider.rs               # RPC connection management
│   ├── dex/
│   │   ├── mod.rs                # DEX trait definition
│   │   ├── uniswap_v3.rs         # Uniswap V3 pool discovery
│   │   ├── pancakeswap.rs        # PancakeSwap pool discovery
│   │   └── lfj.rs                # LFJ pool discovery
│   ├── graph/
│   │   ├── mod.rs                # Graph module
│   │   ├── types.rs              # Graph types (EdgeData, Dex enum)
│   │   ├── builder.rs            # Graph construction from pools
│   │   └── bellman_ford.rs       # Cycle detection algorithm
│   └── logger.rs                 # Console output formatting
```

---

## 6. Cargo.toml Dependencies

```toml
[package]
name = "monad-arb-mvp"
version = "0.1.0"
edition = "2021"

[dependencies]
# Async Runtime
tokio = { version = "1.35", features = ["full"] }

# Ethereum/EVM Interaction (Alloy is the modern Rust library)
alloy = { version = "0.9", features = [
    "full",
    "provider-http",
    "network",
    "contract",
    "sol-types",
    "json-rpc"
]}
alloy-primitives = "0.9"
alloy-sol-types = "0.9"

# Graph algorithms
petgraph = "0.6"

# Logging
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# Configuration
dotenvy = "0.15"

# Serialization
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

# Error handling
eyre = "0.6"
thiserror = "1.0"

# Utilities
hex = "0.4"
```

---

## 7. Core Implementation

### 7.1 Configuration (src/config.rs)

```rust
use alloy_primitives::{Address, address};
use std::env;

pub struct Config {
    pub rpc_url: String,
    pub chain_id: u64,
    pub poll_interval_ms: u64,
    pub max_hops: usize,
    pub min_profit_bps: u32,  // Minimum profit in basis points (100 = 1%)
}

impl Config {
    pub fn from_env() -> eyre::Result<Self> {
        dotenvy::dotenv().ok();
        
        Ok(Self {
            rpc_url: env::var("ALCHEMY_RPC_URL")
                .unwrap_or_else(|_| "https://rpc.monad.xyz".to_string()),
            chain_id: 143,
            poll_interval_ms: 1000,  // 1 second (matches block time)
            max_hops: 4,             // Max 4 swaps in an arb cycle
            min_profit_bps: 10,      // 0.1% minimum to log
        })
    }
}

// Token addresses
pub mod tokens {
    use super::*;
    
    pub const WMON: Address = address!("3bd359C1119dA7Da1D913D1C4D2B7c461115433A");
    pub const USDC: Address = address!("754704Bc059F8C67012fEd69BC8A327a5aafb603");
    pub const USDT: Address = address!("e7cd86e13AC4309349F30B3435a9d337750fC82D");
    pub const WETH: Address = address!("EE8c0E9f1BFFb4Eb878d8f15f368A02a35481242");
    pub const WBTC: Address = address!("0555E30da8f98308EdB960aa94C0Db47230d2B9c");
    pub const SMON: Address = address!("A3227C5969757783154C60bF0bC1944180ed81B9");
    pub const GMON: Address = address!("8498312A6B3CbD158bf0c93AbdCF29E6e4F55081");
    
    pub const BASE_TOKENS: [Address; 4] = [WMON, USDC, USDT, WETH];
    
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
}
```

### 7.2 DEX Trait & Types (src/dex/mod.rs)

```rust
use alloy_primitives::{Address, U256};
use async_trait::async_trait;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dex {
    UniswapV3,
    PancakeSwapV3,
    LFJ,
    Kuru,
}

impl std::fmt::Display for Dex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Dex::UniswapV3 => write!(f, "Uniswap V3"),
            Dex::PancakeSwapV3 => write!(f, "PancakeSwap V3"),
            Dex::LFJ => write!(f, "LFJ"),
            Dex::Kuru => write!(f, "Kuru"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Pool {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,              // Fee in hundredths of bip
    pub dex: Dex,
    pub liquidity: U256,       // Current liquidity
    pub sqrt_price_x96: U256,  // Current price (sqrt(price) * 2^96)
}

impl Pool {
    /// Calculate the price of token0 in terms of token1
    pub fn price_0_to_1(&self) -> f64 {
        let sqrt_price = self.sqrt_price_x96.to::<u128>() as f64 / (2_f64.powi(96));
        sqrt_price * sqrt_price
    }
    
    /// Calculate the price of token1 in terms of token0
    pub fn price_1_to_0(&self) -> f64 {
        1.0 / self.price_0_to_1()
    }
    
    /// Get effective price after fees (token0 -> token1)
    pub fn effective_price_0_to_1(&self) -> f64 {
        let fee_factor = 1.0 - (self.fee as f64 / 1_000_000.0);
        self.price_0_to_1() * fee_factor
    }
    
    /// Get effective price after fees (token1 -> token0)
    pub fn effective_price_1_to_0(&self) -> f64 {
        let fee_factor = 1.0 - (self.fee as f64 / 1_000_000.0);
        self.price_1_to_0() * fee_factor
    }
}

#[async_trait]
pub trait DexClient: Send + Sync {
    /// Get all pools for given token pairs
    async fn get_pools(&self, tokens: &[Address]) -> eyre::Result<Vec<Pool>>;
    
    /// Get the DEX identifier
    fn dex(&self) -> Dex;
}
```

### 7.3 Uniswap V3 Client (src/dex/uniswap_v3.rs)

```rust
use alloy::{
    primitives::{Address, U256, address},
    providers::Provider,
    sol,
};
use async_trait::async_trait;
use super::{Dex, DexClient, Pool};

// Uniswap V3 Factory ABI (minimal)
sol! {
    #[sol(rpc)]
    interface IUniswapV3Factory {
        function getPool(address tokenA, address tokenB, uint24 fee) external view returns (address pool);
    }
}

// Uniswap V3 Pool ABI (minimal)
sol! {
    #[sol(rpc)]
    interface IUniswapV3Pool {
        function token0() external view returns (address);
        function token1() external view returns (address);
        function fee() external view returns (uint24);
        function liquidity() external view returns (uint128);
        function slot0() external view returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint16 observationIndex,
            uint16 observationCardinality,
            uint16 observationCardinalityNext,
            uint8 feeProtocol,
            bool unlocked
        );
    }
}

pub struct UniswapV3Client<P> {
    provider: P,
    factory: Address,
    fee_tiers: Vec<u32>,
}

impl<P: Provider + Clone> UniswapV3Client<P> {
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            factory: address!("1F98431c8aD98523631AE4a59f267346ea31F984"),
            fee_tiers: vec![100, 500, 3000, 10000],
        }
    }
    
    async fn get_pool_address(&self, token0: Address, token1: Address, fee: u32) -> eyre::Result<Option<Address>> {
        let factory = IUniswapV3Factory::new(self.factory, &self.provider);
        let pool_addr = factory.getPool(token0, token1, fee.try_into()?).call().await?;
        
        if pool_addr._0 == Address::ZERO {
            Ok(None)
        } else {
            Ok(Some(pool_addr._0))
        }
    }
    
    async fn get_pool_state(&self, pool_address: Address, token0: Address, token1: Address, fee: u32) -> eyre::Result<Pool> {
        let pool_contract = IUniswapV3Pool::new(pool_address, &self.provider);
        
        let liquidity = pool_contract.liquidity().call().await?;
        let slot0 = pool_contract.slot0().call().await?;
        
        Ok(Pool {
            address: pool_address,
            token0,
            token1,
            fee,
            dex: Dex::UniswapV3,
            liquidity: U256::from(liquidity._0),
            sqrt_price_x96: U256::from(slot0.sqrtPriceX96),
        })
    }
}

#[async_trait]
impl<P: Provider + Clone + Send + Sync> DexClient for UniswapV3Client<P> {
    async fn get_pools(&self, tokens: &[Address]) -> eyre::Result<Vec<Pool>> {
        let mut pools = Vec::new();
        
        // Check all token pairs across all fee tiers
        for i in 0..tokens.len() {
            for j in (i + 1)..tokens.len() {
                let (token0, token1) = if tokens[i] < tokens[j] {
                    (tokens[i], tokens[j])
                } else {
                    (tokens[j], tokens[i])
                };
                
                for &fee in &self.fee_tiers {
                    if let Ok(Some(pool_addr)) = self.get_pool_address(token0, token1, fee).await {
                        match self.get_pool_state(pool_addr, token0, token1, fee).await {
                            Ok(pool) => {
                                // Only add pools with liquidity
                                if pool.liquidity > U256::ZERO {
                                    pools.push(pool);
                                }
                            }
                            Err(e) => {
                                tracing::debug!("Failed to get pool state for {}: {}", pool_addr, e);
                            }
                        }
                    }
                }
            }
        }
        
        Ok(pools)
    }
    
    fn dex(&self) -> Dex {
        Dex::UniswapV3
    }
}
```

### 7.4 Graph Types (src/graph/types.rs)

```rust
use alloy_primitives::Address;
use crate::dex::Dex;

#[derive(Debug, Clone)]
pub struct EdgeData {
    pub pool_address: Address,
    pub dex: Dex,
    pub price: f64,        // Effective price (after fees)
    pub fee: u32,          // Fee in hundredths of bip
    pub weight: f64,       // -ln(price) for Bellman-Ford
    pub liquidity: f64,    // Normalized liquidity
}

impl EdgeData {
    pub fn new(pool_address: Address, dex: Dex, price: f64, fee: u32, liquidity: f64) -> Self {
        Self {
            pool_address,
            dex,
            price,
            fee,
            weight: -price.ln(),  // Negative log for cycle detection
            liquidity,
        }
    }
}
```

### 7.5 Graph Builder (src/graph/builder.rs)

```rust
use alloy_primitives::Address;
use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::HashMap;
use super::types::EdgeData;
use crate::dex::Pool;

pub struct ArbitrageGraph {
    pub graph: DiGraph<Address, EdgeData>,
    token_to_node: HashMap<Address, NodeIndex>,
}

impl ArbitrageGraph {
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            token_to_node: HashMap::new(),
        }
    }
    
    fn get_or_create_node(&mut self, token: Address) -> NodeIndex {
        if let Some(&node) = self.token_to_node.get(&token) {
            node
        } else {
            let node = self.graph.add_node(token);
            self.token_to_node.insert(token, node);
            node
        }
    }
    
    pub fn add_pool(&mut self, pool: &Pool) {
        let node0 = self.get_or_create_node(pool.token0);
        let node1 = self.get_or_create_node(pool.token1);
        
        // Get liquidity as f64 for edge data
        let liquidity = pool.liquidity.to::<u128>() as f64;
        
        // Add edge from token0 -> token1
        let price_0_to_1 = pool.effective_price_0_to_1();
        if price_0_to_1.is_finite() && price_0_to_1 > 0.0 {
            let edge_data = EdgeData::new(
                pool.address,
                pool.dex,
                price_0_to_1,
                pool.fee,
                liquidity,
            );
            self.graph.add_edge(node0, node1, edge_data);
        }
        
        // Add edge from token1 -> token0
        let price_1_to_0 = pool.effective_price_1_to_0();
        if price_1_to_0.is_finite() && price_1_to_0 > 0.0 {
            let edge_data = EdgeData::new(
                pool.address,
                pool.dex,
                price_1_to_0,
                pool.fee,
                liquidity,
            );
            self.graph.add_edge(node1, node0, edge_data);
        }
    }
    
    pub fn get_node(&self, token: Address) -> Option<NodeIndex> {
        self.token_to_node.get(&token).copied()
    }
    
    pub fn get_token(&self, node: NodeIndex) -> Option<Address> {
        self.graph.node_weight(node).copied()
    }
    
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }
    
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }
}
```

### 7.6 Bellman-Ford Cycle Detection (src/graph/bellman_ford.rs)

This is adapted from your reference implementation:

```rust
use alloy_primitives::Address;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use std::collections::{HashMap, HashSet};
use tracing::debug;

use super::builder::ArbitrageGraph;
use super::types::EdgeData;
use crate::dex::Dex;
use crate::config::tokens;

#[derive(Debug, Clone)]
pub struct ArbitrageCycle {
    pub path: Vec<Address>,
    pub pools: Vec<Address>,
    pub dexes: Vec<Dex>,
    pub total_weight: f64,
    pub expected_return: f64,
    pub prices: Vec<f64>,
    pub fees: Vec<u32>,
}

impl ArbitrageCycle {
    pub fn profit_percentage(&self) -> f64 {
        (self.expected_return - 1.0) * 100.0
    }
    
    pub fn profit_bps(&self) -> u32 {
        ((self.expected_return - 1.0) * 10000.0) as u32
    }
    
    pub fn hop_count(&self) -> usize {
        self.pools.len()
    }
    
    pub fn is_cross_dex(&self) -> bool {
        if self.dexes.is_empty() { return false; }
        let first = self.dexes[0];
        self.dexes.iter().any(|d| *d != first)
    }
    
    pub fn dex_path(&self) -> String {
        self.dexes.iter().map(|d| d.to_string()).collect::<Vec<_>>().join(" → ")
    }
    
    pub fn token_path(&self) -> String {
        self.path.iter()
            .map(|addr| tokens::symbol(*addr))
            .collect::<Vec<_>>()
            .join(" → ")
    }
    
    pub fn avg_fee_bps(&self) -> f64 {
        if self.fees.is_empty() { return 0.0; }
        self.fees.iter().map(|&f| f as f64).sum::<f64>() / self.fees.len() as f64 / 100.0
    }
    
    pub fn is_valid(&self) -> bool {
        if self.path.len() < 3 { return false; }
        if self.path.first() != self.path.last() { return false; }
        if self.path.len() != self.pools.len() + 1 { return false; }
        
        let intermediate: Vec<_> = self.path[1..self.path.len()-1].to_vec();
        let unique_intermediate: HashSet<_> = intermediate.iter().collect();
        if unique_intermediate.len() != intermediate.len() { return false; }
        
        let start = self.path[0];
        if intermediate.contains(&start) { return false; }
        
        let unique_pools: HashSet<_> = self.pools.iter().collect();
        if unique_pools.len() != self.pools.len() { return false; }
        
        if self.expected_return <= 0.0 || !self.expected_return.is_finite() { return false; }
        if self.expected_return > 10.0 { return false; }  // Cap at 1000% to filter noise
        
        true
    }
}

pub struct BoundedBellmanFord<'a> {
    graph: &'a ArbitrageGraph,
    max_hops: usize,
    min_return: f64,  // Minimum expected return (e.g., 1.001 for 0.1%)
}

impl<'a> BoundedBellmanFord<'a> {
    pub fn new(graph: &'a ArbitrageGraph, max_hops: usize, min_profit_bps: u32) -> Self {
        let min_return = 1.0 + (min_profit_bps as f64 / 10000.0);
        Self { graph, max_hops, min_return }
    }

    pub fn find_cycles_from(&self, start_token: Address) -> Vec<ArbitrageCycle> {
        let mut cycles = Vec::new();
        let Some(start_node) = self.graph.get_node(start_token) else {
            return cycles;
        };

        self.dfs_find_cycles(
            start_node, start_node,
            Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(),
            HashSet::new(), 0.0, &mut cycles, 1,
        );

        cycles
    }
    
    #[allow(clippy::too_many_arguments)]
    fn dfs_find_cycles(
        &self,
        start_node: NodeIndex,
        current_node: NodeIndex,
        mut path: Vec<Address>,
        mut pools: Vec<Address>,
        mut dexes: Vec<Dex>,
        mut prices: Vec<f64>,
        mut fees: Vec<u32>,
        mut visited: HashSet<NodeIndex>,
        total_weight: f64,
        cycles: &mut Vec<ArbitrageCycle>,
        depth: usize,
    ) {
        let current_token = match self.graph.get_token(current_node) {
            Some(t) => t,
            None => return,
        };
        
        path.push(current_token);
        if depth > 1 { visited.insert(current_node); }
        if depth > self.max_hops { return; }
        
        for edge in self.graph.graph.edges(current_node) {
            let target = edge.target();
            let edge_data = edge.weight();
            let new_weight = total_weight + edge_data.weight;
            
            // Check if we've completed a cycle back to start
            if target == start_node && depth >= 2 {
                let expected_return = (-new_weight).exp();
                
                // Only record profitable cycles above threshold
                if expected_return >= self.min_return {
                    let mut final_path = path.clone();
                    final_path.push(self.graph.get_token(start_node).unwrap());
                    
                    let mut final_pools = pools.clone();
                    final_pools.push(edge_data.pool_address);
                    
                    let mut final_dexes = dexes.clone();
                    final_dexes.push(edge_data.dex);
                    
                    let mut final_prices = prices.clone();
                    final_prices.push(edge_data.price);
                    
                    let mut final_fees = fees.clone();
                    final_fees.push(edge_data.fee);
                    
                    let cycle = ArbitrageCycle {
                        path: final_path,
                        pools: final_pools,
                        dexes: final_dexes,
                        total_weight: new_weight,
                        expected_return,
                        prices: final_prices,
                        fees: final_fees,
                    };
                    
                    if cycle.is_valid() {
                        cycles.push(cycle);
                    }
                }
            } else if !visited.contains(&target) && depth < self.max_hops {
                let mut new_pools = pools.clone();
                new_pools.push(edge_data.pool_address);
                
                let mut new_dexes = dexes.clone();
                new_dexes.push(edge_data.dex);
                
                let mut new_prices = prices.clone();
                new_prices.push(edge_data.price);
                
                let mut new_fees = fees.clone();
                new_fees.push(edge_data.fee);
                
                self.dfs_find_cycles(
                    start_node, target,
                    path.clone(), new_pools, new_dexes, new_prices, new_fees,
                    visited.clone(), new_weight, cycles, depth + 1,
                );
            }
        }
    }

    pub fn find_all_cycles(&self, base_tokens: &[Address]) -> Vec<ArbitrageCycle> {
        let mut all_cycles = Vec::new();
        let mut seen_signatures: HashSet<String> = HashSet::new();

        for &token in base_tokens {
            let cycles = self.find_cycles_from(token);
            
            for cycle in cycles {
                let signature = create_cycle_signature(&cycle);
                if !seen_signatures.contains(&signature) {
                    seen_signatures.insert(signature);
                    all_cycles.push(cycle);
                }
            }
        }

        // Sort by expected return (best first)
        all_cycles.sort_by(|a, b| {
            b.expected_return.partial_cmp(&a.expected_return)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        all_cycles
    }
}

fn create_cycle_signature(cycle: &ArbitrageCycle) -> String {
    let mut pool_strs: Vec<String> = cycle.pools.iter()
        .map(|p| format!("{:?}", p))
        .collect();
    pool_strs.sort();
    pool_strs.join("-")
}
```

### 7.7 Main Entry Point (src/main.rs)

```rust
mod config;
mod dex;
mod graph;

use alloy::providers::{Provider, ProviderBuilder};
use std::time::Duration;
use tokio::time::interval;
use tracing::{info, warn, error};
use tracing_subscriber::EnvFilter;

use config::{Config, tokens};
use dex::{uniswap_v3::UniswapV3Client, DexClient};
use graph::{builder::ArbitrageGraph, bellman_ford::BoundedBellmanFord};

#[tokio::main]
async fn main() -> eyre::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive("monad_arb_mvp=info".parse()?)
        )
        .init();
    
    info!("🚀 Monad Arbitrage MVP Starting...");
    
    // Load configuration
    let config = Config::from_env()?;
    info!("📡 Connecting to Monad Mainnet (Chain ID: {})", config.chain_id);
    
    // Create provider
    let provider = ProviderBuilder::new()
        .on_http(config.rpc_url.parse()?);
    
    // Verify connection
    let block = provider.get_block_number().await?;
    info!("✅ Connected! Current block: {}", block);
    
    // Initialize DEX clients
    let uniswap = UniswapV3Client::new(provider.clone());
    // TODO: Add more DEX clients
    
    // Token list to monitor
    let tokens_to_monitor = vec![
        tokens::WMON,
        tokens::USDC,
        tokens::USDT,
        tokens::WETH,
        tokens::WBTC,
        tokens::SMON,
        tokens::GMON,
    ];
    
    info!("👀 Monitoring {} tokens", tokens_to_monitor.len());
    
    // Main loop
    let mut poll_interval = interval(Duration::from_millis(config.poll_interval_ms));
    
    loop {
        poll_interval.tick().await;
        
        // Build fresh graph each iteration
        let mut graph = ArbitrageGraph::new();
        
        // Fetch pools from all DEXes
        match uniswap.get_pools(&tokens_to_monitor).await {
            Ok(pools) => {
                info!("📊 Found {} Uniswap V3 pools", pools.len());
                for pool in &pools {
                    graph.add_pool(pool);
                }
            }
            Err(e) => {
                warn!("Failed to fetch Uniswap pools: {}", e);
            }
        }
        
        // TODO: Add more DEX pool fetching here
        
        if graph.edge_count() == 0 {
            info!("⏳ No pools found yet, waiting...");
            continue;
        }
        
        info!("🔗 Graph: {} nodes, {} edges", graph.node_count(), graph.edge_count());
        
        // Find arbitrage cycles
        let detector = BoundedBellmanFord::new(&graph, config.max_hops, config.min_profit_bps);
        let cycles = detector.find_all_cycles(&tokens::BASE_TOKENS);
        
        if cycles.is_empty() {
            info!("🔍 No arbitrage opportunities found");
        } else {
            info!("💰 Found {} potential arbitrage opportunities!", cycles.len());
            
            // Log top opportunities
            for (i, cycle) in cycles.iter().take(10).enumerate() {
                let cross_dex = if cycle.is_cross_dex() { "🌐" } else { "📊" };
                
                println!("\n{} Opportunity #{}", cross_dex, i + 1);
                println!("   Path: {}", cycle.token_path());
                println!("   Profit: {:.4}% ({} bps)", cycle.profit_percentage(), cycle.profit_bps());
                println!("   Hops: {}", cycle.hop_count());
                println!("   DEXes: {}", cycle.dex_path());
                println!("   Avg Fee: {:.2} bps", cycle.avg_fee_bps());
            }
        }
        
        println!("\n---");
    }
}
```

---

## 8. Environment Setup

### .env File (Create in project root)

```bash
# Monad RPC Configuration
ALCHEMY_RPC_URL=https://monad-mainnet.g.alchemy.com/v2/YOUR_API_KEY_HERE

# Logging level
RUST_LOG=monad_arb_mvp=info
```

### .gitignore

```
/target
.env
*.log
```

---

## 9. Build & Run Instructions

### Windows Development

```powershell
# 1. Create new project
cargo new monad-arb-mvp
cd monad-arb-mvp

# 2. Copy the Cargo.toml and source files

# 3. Create .env file with your Alchemy API key

# 4. Build the project
cargo build --release

# 5. Run
cargo run --release
```

### Expected Output

```
🚀 Monad Arbitrage MVP Starting...
📡 Connecting to Monad Mainnet (Chain ID: 143)
✅ Connected! Current block: 12345678
👀 Monitoring 7 tokens
📊 Found 15 Uniswap V3 pools
🔗 Graph: 7 nodes, 30 edges
💰 Found 3 potential arbitrage opportunities!

🌐 Opportunity #1
   Path: WMON → USDC → WETH → WMON
   Profit: 0.5234% (52 bps)
   Hops: 3
   DEXes: Uniswap V3 → Uniswap V3 → Uniswap V3
   Avg Fee: 5.00 bps

📊 Opportunity #2
   Path: USDC → WMON → USDC
   Profit: 0.1523% (15 bps)
   Hops: 2
   DEXes: Uniswap V3 → Uniswap V3
   Avg Fee: 3.00 bps
---
```

---

## 10. Phase 2 Enhancements (Post-MVP)

After MVP is working:

1. **Add More DEXes**
   - PancakeSwap V3 client
   - LFJ (Liquidity Book) client
   - Kuru orderbook integration

2. **WebSocket Streaming**
   - Use `wss://monad-mainnet.g.alchemy.com/v2/...` for real-time block updates
   - Subscribe to pool events for instant price updates

3. **Gas Estimation**
   - Add gas cost calculation to profit estimation
   - Filter opportunities by net profitability

4. **Flash Loan Integration**
   - Neverland Protocol (Aave V3 fork) addresses:
     - Pool: `0x80F00661b13CC5F6ccd3885bE7b4C9c67545D585`
     - Fee: 0.09%

5. **MEV Protection & Bundle Submission**
   - **bloXroute**: Public API available for Monad (see Phase 2 doc)
   - **aPriori**: Liquid staking live, but searcher API is whitelisted/permissioned
   - See `MONAD_ARBITRAGE_PHASE2_EXECUTION.md` for details

---

## 11. Important Notes & Caveats

### Network Characteristics
- **Probabilistic Execution**: Monad's deferred execution means simulations may not match actual execution
- **Block Time**: ~400ms, much faster than Ethereum
- **Gas Fees**: Near-zero, not a significant factor

### Current Limitations
1. **MEV Infrastructure**:
   - **aPriori**: Liquid staking (aprMON) is LIVE, but searcher bundle API is in permissioned/whitelisted phase - no public documentation available
   - **bloXroute**: Has documented Monad support with API endpoints - this is the PUBLIC path for bundle submission (see Phase 2 doc)
2. **Flash Loan Liquidity**: Neverland has ~$1M TVL (limited)
3. **Chain Age**: Only 1 week old - expect volatility and bugs

### Security Reminders
- ✅ Never commit API keys to git
- ✅ Use separate wallet for bot operations
- ✅ Start with read-only mode (this MVP)
- ❌ Never hardcode private keys

---

## 12. Resources

| Resource | URL |
|----------|-----|
| Monad Docs | https://docs.monad.xyz |
| Monad Explorer | https://monadvision.com |
| Protocol Contracts | https://github.com/monad-crypto/protocols |
| Token List | https://github.com/monad-crypto/token-list |
| Neverland Docs | https://docs.neverland.money |
| Uniswap V3 Docs | https://docs.uniswap.org/contracts/v3 |
| Alloy Docs | https://alloy.rs |
| **bloXroute Docs** | https://docs.bloxroute.com (see "MONAD Network" section) |
| **aPriori** | https://apr.io |
| **Phase 2 Guide** | `MONAD_ARBITRAGE_PHASE2_EXECUTION.md` |

---

*Document generated: December 1, 2025*
*For Claude Code Opus implementation*
