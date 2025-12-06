// =============================================================================
// MONAD LOCAL NODE INTEGRATION PATCHES v2.0
// =============================================================================
// Updated: December 6, 2025
// Monad Version: 0.12.3
// CRITICAL: RPC ports are 8080 (HTTP) and 8081 (WS) - NOT 8545/8546!
// =============================================================================

// =============================================================================
// PATCH 1: CORRECTED ENVIRONMENT CONFIGURATION
// =============================================================================
// File: .env or environment variables
// 
// BEFORE (WRONG - these are Ethereum standard ports):
// MONAD_RPC_URL=http://127.0.0.1:8545
// MONAD_WS_URL=ws://127.0.0.1:8546
//
// AFTER (CORRECT - Monad actual ports):
// MONAD_RPC_URL=http://127.0.0.1:8080
// MONAD_WS_URL=ws://127.0.0.1:8081


// =============================================================================
// PATCH 2: NODE CONFIGURATION MODULE
// =============================================================================
// File: src/config/node.rs (new file)

use std::time::Duration;

/// Configuration for Monad node connection
#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// Whether connected to a local node (enables aggressive optimizations)
    pub is_local: bool,
    /// HTTP RPC endpoint
    pub rpc_url: String,
    /// WebSocket endpoint
    pub ws_url: String,
    /// Polling interval for price updates
    pub poll_interval: Duration,
    /// Polling interval for transaction receipts
    pub receipt_poll_interval: Duration,
    /// Gas buffer multiplier (1.10 = 10% buffer)
    pub gas_buffer: f64,
    /// Whether to skip block confirmations (safe for local node)
    pub skip_block_wait: bool,
}

impl NodeConfig {
    /// Create configuration from environment variables
    pub fn from_env() -> Self {
        let rpc_url = std::env::var("MONAD_RPC_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8080".to_string()); // MONAD PORT!
        
        let ws_url = std::env::var("MONAD_WS_URL")
            .unwrap_or_else(|_| "ws://127.0.0.1:8081".to_string()); // MONAD PORT!
        
        // Detect if we're connecting to a local node
        let is_local = rpc_url.contains("127.0.0.1") 
            || rpc_url.contains("localhost")
            || rpc_url.starts_with("http://10.")
            || rpc_url.starts_with("http://192.168.");
        
        if is_local {
            Self::local_config(rpc_url, ws_url)
        } else {
            Self::remote_config(rpc_url, ws_url)
        }
    }
    
    /// Aggressive settings for local node
    fn local_config(rpc_url: String, ws_url: String) -> Self {
        Self {
            is_local: true,
            rpc_url,
            ws_url,
            poll_interval: Duration::from_millis(100),      // 10x faster
            receipt_poll_interval: Duration::from_millis(5), // 4x faster
            gas_buffer: 1.10,                                // 10% (tighter)
            skip_block_wait: true,                           // State is consistent
        }
    }
    
    /// Conservative settings for remote/public RPC
    fn remote_config(rpc_url: String, ws_url: String) -> Self {
        Self {
            is_local: false,
            rpc_url,
            ws_url,
            poll_interval: Duration::from_millis(1000),      // Standard
            receipt_poll_interval: Duration::from_millis(20), // Standard
            gas_buffer: 1.15,                                 // 15% buffer
            skip_block_wait: false,                           // Wait for propagation
        }
    }
    
    /// Log configuration on startup
    pub fn log_config(&self) {
        println!("=== Node Configuration ===");
        println!("RPC URL: {}", self.rpc_url);
        println!("WS URL: {}", self.ws_url);
        println!("Local Node: {}", self.is_local);
        println!("Poll Interval: {:?}", self.poll_interval);
        println!("Receipt Poll: {:?}", self.receipt_poll_interval);
        println!("Gas Buffer: {:.0}%", (self.gas_buffer - 1.0) * 100.0);
        println!("Skip Block Wait: {}", self.skip_block_wait);
        println!("==========================");
    }
}


// =============================================================================
// PATCH 3: HEALTH CHECK MODULE
// =============================================================================
// File: src/health/node_health.rs (new file)

use ethers::prelude::*;
use std::time::{Duration, Instant};

/// Node health status
#[derive(Debug, Clone)]
pub struct NodeHealth {
    pub is_healthy: bool,
    pub block_number: u64,
    pub peer_count: u64,
    pub is_syncing: bool,
    pub rpc_latency_ms: u64,
    pub last_check: Instant,
}

impl NodeHealth {
    /// Check node health (call periodically)
    pub async fn check(provider: &Provider<Http>) -> Result<Self, Box<dyn std::error::Error>> {
        let start = Instant::now();
        
        // Get block number (also measures latency)
        let block_number = provider.get_block_number().await?.as_u64();
        let rpc_latency_ms = start.elapsed().as_millis() as u64;
        
        // Get peer count
        let peer_count = provider
            .request::<_, U64>("net_peerCount", ())
            .await
            .unwrap_or(U64::zero())
            .as_u64();
        
        // Check sync status
        let sync_status = provider.syncing().await?;
        let is_syncing = match sync_status {
            SyncingStatus::IsSyncing(_) => true,
            SyncingStatus::NotSyncing => false,
        };
        
        // Determine health
        let is_healthy = !is_syncing && peer_count > 5 && rpc_latency_ms < 100;
        
        Ok(Self {
            is_healthy,
            block_number,
            peer_count,
            is_syncing,
            rpc_latency_ms,
            last_check: Instant::now(),
        })
    }
    
    /// Quick latency check
    pub async fn ping(provider: &Provider<Http>) -> Result<Duration, Box<dyn std::error::Error>> {
        let start = Instant::now();
        let _ = provider.get_block_number().await?;
        Ok(start.elapsed())
    }
}


// =============================================================================
// PATCH 4: UPDATED MULTICALL WITH LOCAL NODE OPTIMIZATION
// =============================================================================
// File: src/multicall.rs - Updates to existing file

// CHANGE 1: Update default RPC URL
// BEFORE:
// const DEFAULT_RPC: &str = "https://some-public-rpc.monad.xyz";
// AFTER:
const DEFAULT_RPC: &str = "http://127.0.0.1:8080"; // Local Monad node

// CHANGE 2: Add batch size optimization for local node
impl PriceOracle {
    /// Optimized batch size based on node type
    fn get_batch_size(&self, config: &NodeConfig) -> usize {
        if config.is_local {
            // Local node can handle larger batches with lower latency
            100
        } else {
            // Public RPCs may have stricter limits
            50
        }
    }
    
    /// Fetch prices with node-aware optimization
    pub async fn fetch_prices_optimized(
        &self,
        pairs: &[Address],
        config: &NodeConfig,
    ) -> Result<Vec<PriceData>, Box<dyn std::error::Error>> {
        let batch_size = self.get_batch_size(config);
        
        let mut all_prices = Vec::new();
        for chunk in pairs.chunks(batch_size) {
            let prices = self.multicall_prices(chunk).await?;
            all_prices.extend(prices);
            
            // No delay needed for local node
            if !config.is_local {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        
        Ok(all_prices)
    }
}


// =============================================================================
// PATCH 5: UPDATED SWAP EXECUTION WITH LOCAL NODE OPTIMIZATION
// =============================================================================
// File: src/execution/swap.rs - Updates to existing file

impl SwapExecutor {
    /// Execute swap with node-aware timing
    pub async fn execute_with_config(
        &self,
        swap: &SwapParams,
        config: &NodeConfig,
    ) -> Result<TransactionReceipt, ExecutionError> {
        // Estimate gas with tighter buffer for local node
        let gas_estimate = self.provider.estimate_gas(&swap.to_tx(), None).await?;
        let gas_limit = (gas_estimate.as_u64() as f64 * config.gas_buffer) as u64;
        
        // Build and sign transaction
        let tx = swap.to_tx().gas(gas_limit);
        let pending_tx = self.wallet.send_transaction(tx, None).await?;
        
        // Wait for receipt with appropriate polling interval
        let receipt = self.wait_for_receipt_optimized(
            pending_tx.tx_hash(),
            config,
        ).await?;
        
        Ok(receipt)
    }
    
    /// Optimized receipt polling for local node
    async fn wait_for_receipt_optimized(
        &self,
        tx_hash: H256,
        config: &NodeConfig,
    ) -> Result<TransactionReceipt, ExecutionError> {
        let max_attempts = if config.is_local { 200 } else { 50 };
        let poll_interval = config.receipt_poll_interval;
        
        for _ in 0..max_attempts {
            if let Some(receipt) = self.provider.get_transaction_receipt(tx_hash).await? {
                return Ok(receipt);
            }
            tokio::time::sleep(poll_interval).await;
        }
        
        Err(ExecutionError::ReceiptTimeout)
    }
}


// =============================================================================
// PATCH 6: MAIN.RS STARTUP WITH HEALTH CHECK
// =============================================================================
// File: src/main.rs - Add to startup sequence

async fn startup() -> Result<(), Box<dyn std::error::Error>> {
    // Load node configuration
    let config = NodeConfig::from_env();
    config.log_config();
    
    // Create provider with correct Monad port
    let provider = Provider::<Http>::try_from(&config.rpc_url)?;
    
    // Verify node health before starting bot
    println!("Checking node health...");
    let health = NodeHealth::check(&provider).await?;
    
    if !health.is_healthy {
        if health.is_syncing {
            println!("WARNING: Node is still syncing. Block: {}", health.block_number);
            println!("Wait for sync to complete before running arbitrage.");
            return Err("Node not synced".into());
        }
        if health.peer_count < 5 {
            println!("WARNING: Low peer count ({}). Node may be isolated.", health.peer_count);
        }
    }
    
    println!("Node healthy! Block: {}, Peers: {}, Latency: {}ms",
        health.block_number,
        health.peer_count,
        health.rpc_latency_ms
    );
    
    // Verify it's actually a Monad node
    let chain_id = provider.get_chainid().await?;
    if chain_id != U256::from(10143) { // Monad mainnet chain ID - VERIFY THIS
        println!("WARNING: Unexpected chain ID: {}. Expected Monad mainnet.", chain_id);
    }
    
    // Continue with bot initialization...
    Ok(())
}


// =============================================================================
// PATCH 7: WEBSOCKET SUBSCRIPTION FOR MEMPOOL (ADVANCED)
// =============================================================================
// File: src/mempool/subscriber.rs (new file - optional advanced feature)

use ethers::prelude::*;
use futures::StreamExt;

/// Subscribe to pending transactions for MEV opportunities
pub struct MempoolSubscriber {
    ws_url: String,
}

impl MempoolSubscriber {
    pub fn new(config: &NodeConfig) -> Self {
        Self {
            ws_url: config.ws_url.clone(),
        }
    }
    
    /// Subscribe to pending transactions
    /// NOTE: Requires Monad node with --trace_calls flag enabled
    pub async fn subscribe_pending_txs(
        &self,
    ) -> Result<impl futures::Stream<Item = Transaction>, Box<dyn std::error::Error>> {
        // Connect to WebSocket on port 8081 (MONAD PORT!)
        let ws = Ws::connect(&self.ws_url).await?;
        let provider = Provider::new(ws);
        
        // Subscribe to pending transactions
        let stream = provider.subscribe_pending_txs().await?;
        
        // Transform to full transactions
        let tx_stream = stream.then(move |tx_hash| {
            let provider = provider.clone();
            async move {
                provider.get_transaction(tx_hash).await.ok().flatten()
            }
        })
        .filter_map(|tx| async { tx });
        
        Ok(tx_stream)
    }
    
    /// Subscribe to new blocks (for timing arbitrage execution)
    pub async fn subscribe_blocks(
        &self,
    ) -> Result<impl futures::Stream<Item = Block<H256>>, Box<dyn std::error::Error>> {
        let ws = Ws::connect(&self.ws_url).await?;
        let provider = Provider::new(ws);
        
        let stream = provider.subscribe_blocks().await?;
        Ok(stream)
    }
}


// =============================================================================
// PATCH 8: CONSTANTS UPDATE
// =============================================================================
// File: src/constants.rs - Update port constants

// MONAD MAINNET CONFIGURATION
// CRITICAL: These are NOT Ethereum standard ports!
pub const MONAD_RPC_PORT: u16 = 8080;           // HTTP JSON-RPC
pub const MONAD_WS_PORT: u16 = 8081;            // WebSocket
pub const MONAD_P2P_PORT: u16 = 8000;           // Consensus (for reference)
pub const MONAD_METRICS_PORT: u16 = 9090;       // Prometheus (for reference)

// Default URLs for local node
pub const DEFAULT_MONAD_RPC: &str = "http://127.0.0.1:8080";
pub const DEFAULT_MONAD_WS: &str = "ws://127.0.0.1:8081";

// Monad Mainnet Chain ID (VERIFY with official docs)
pub const MONAD_MAINNET_CHAIN_ID: u64 = 10143; // TODO: Verify actual chain ID

// Timing constants
pub const MONAD_BLOCK_TIME_MS: u64 = 1000;      // ~1 second blocks
pub const MONAD_FINALITY_MS: u64 = 1000;        // Near-instant finality


// =============================================================================
// PATCH 9: CARGO.TOML DEPENDENCIES (for reference)
// =============================================================================
/*
[dependencies]
ethers = { version = "2.0", features = ["ws", "rustls"] }
tokio = { version = "1.0", features = ["full"] }
futures = "0.3"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
dotenv = "0.15"
*/


// =============================================================================
// QUICK REFERENCE: MONAD vs ETHEREUM PORTS
// =============================================================================
/*
SERVICE          | ETHEREUM (GETH) | MONAD
-----------------+-----------------+-------
HTTP RPC         | 8545            | 8080
WebSocket        | 8546            | 8081
P2P              | 30303           | 8000
Metrics          | 6060            | 9090

CRITICAL: If your code uses 8545/8546, it will NOT connect to Monad!
*/