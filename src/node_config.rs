use std::time::Duration;

/// Configuration for Monad node connection
/// Automatically detects local vs remote node and optimizes settings accordingly
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
    /// Batch size for multicall operations
    pub multicall_batch_size: usize,
}

impl NodeConfig {
    /// Create configuration from environment variables
    /// Automatically detects local node by URL pattern and applies optimizations
    pub fn from_env() -> Self {
        // MONAD PORTS: 8080 (HTTP) and 8081 (WS) - NOT Ethereum's 8545/8546!
        let rpc_url = std::env::var("MONAD_RPC_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());

        // Derive WS URL from RPC URL or use explicit setting
        let ws_url = std::env::var("MONAD_WS_URL").unwrap_or_else(|_| {
            if rpc_url.contains("127.0.0.1") || rpc_url.contains("localhost") {
                // Local node: use port 8081
                "ws://127.0.0.1:8081".to_string()
            } else {
                // Remote: derive from RPC URL
                rpc_url.replace("https://", "wss://").replace("http://", "ws://")
            }
        });

        // Detect if we're connecting to a local node
        let is_local = Self::detect_local_node(&rpc_url);

        if is_local {
            Self::local_config(rpc_url, ws_url)
        } else {
            Self::remote_config(rpc_url, ws_url)
        }
    }

    /// Detect if the RPC URL points to a local node
    fn detect_local_node(rpc_url: &str) -> bool {
        rpc_url.contains("127.0.0.1")
            || rpc_url.contains("localhost")
            || rpc_url.starts_with("http://10.")
            || rpc_url.starts_with("http://192.168.")
            || rpc_url.starts_with("http://172.")
    }

    /// Aggressive settings for local Monad node
    /// These settings are safe because:
    /// - No network latency to the node
    /// - State is immediately consistent (no propagation delay)
    /// - No rate limits from public RPC providers
    fn local_config(rpc_url: String, ws_url: String) -> Self {
        Self {
            is_local: true,
            rpc_url,
            ws_url,
            poll_interval: Duration::from_millis(50),        // 20x faster than remote (50ms)
            receipt_poll_interval: Duration::from_millis(2), // Ultra-fast 2ms polling
            gas_buffer: 1.10,                                // 10% (tighter, saves cost)
            skip_block_wait: true,                           // State is consistent locally
            multicall_batch_size: 100,                       // Larger batches OK
        }
    }

    /// Conservative settings for remote/public RPC
    /// These settings account for:
    /// - Network latency
    /// - RPC rate limits
    /// - State propagation delays
    fn remote_config(rpc_url: String, ws_url: String) -> Self {
        Self {
            is_local: false,
            rpc_url,
            ws_url,
            poll_interval: Duration::from_millis(1000),       // Standard 1s polling
            receipt_poll_interval: Duration::from_millis(100), // Standard polling
            gas_buffer: 1.15,                                  // 15% buffer for safety
            skip_block_wait: false,                            // Wait for propagation
            multicall_batch_size: 50,                          // Conservative batch size
        }
    }

    /// Log configuration on startup for debugging
    pub fn log_config(&self) {
        println!("=== Monad Node Configuration ===");
        println!("RPC URL: {}", self.rpc_url);
        println!("WS URL: {}", self.ws_url);
        println!("Local Node: {} {}",
            self.is_local,
            if self.is_local { "(optimizations enabled)" } else { "(conservative mode)" }
        );
        println!("Poll Interval: {:?}", self.poll_interval);
        println!("Receipt Poll: {:?}", self.receipt_poll_interval);
        println!("Gas Buffer: {:.0}%", (self.gas_buffer - 1.0) * 100.0);
        println!("Skip Block Wait: {}", self.skip_block_wait);
        println!("Multicall Batch: {}", self.multicall_batch_size);
        println!("================================");
    }

    /// Get gas limit with appropriate buffer based on node type
    pub fn apply_gas_buffer(&self, estimated_gas: u64) -> u64 {
        (estimated_gas as f64 * self.gas_buffer) as u64
    }
}

// ============== MONAD PORT CONSTANTS ==============
// CRITICAL: Monad uses different ports than Ethereum!

/// Monad HTTP JSON-RPC port (NOT 8545!)
pub const MONAD_RPC_PORT: u16 = 8080;

/// Monad WebSocket port (NOT 8546!)
pub const MONAD_WS_PORT: u16 = 8081;

/// Monad P2P/Consensus port (for reference)
pub const MONAD_P2P_PORT: u16 = 8000;

/// Monad Prometheus metrics port (for reference)
pub const MONAD_METRICS_PORT: u16 = 9090;

/// Default local Monad RPC URL
pub const DEFAULT_MONAD_RPC: &str = "http://127.0.0.1:8080";

/// Default local Monad WebSocket URL
pub const DEFAULT_MONAD_WS: &str = "ws://127.0.0.1:8081";

/// Monad Mainnet Chain ID
pub const MONAD_MAINNET_CHAIN_ID: u64 = 143;

/// Monad block time in milliseconds (~1 second)
pub const MONAD_BLOCK_TIME_MS: u64 = 1000;

/// Monad finality time in milliseconds (near-instant)
pub const MONAD_FINALITY_MS: u64 = 1000;
