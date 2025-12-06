use alloy::providers::Provider;
use alloy::primitives::U64;
use eyre::Result;
use std::time::{Duration, Instant};

use crate::node_config::MONAD_MAINNET_CHAIN_ID;

/// Node health status for startup verification
#[derive(Debug, Clone)]
pub struct NodeHealth {
    pub is_healthy: bool,
    pub block_number: u64,
    pub peer_count: u64,
    pub is_syncing: bool,
    pub rpc_latency_ms: u64,
    pub chain_id: u64,
    pub chain_id_valid: bool,
}

impl NodeHealth {
    /// Comprehensive node health check
    /// Verifies connectivity, sync status, peer count, and chain ID
    pub async fn check<P: Provider>(provider: &P) -> Result<Self> {
        let start = Instant::now();

        // Get block number (also measures latency)
        let block_number = provider.get_block_number().await?;
        let rpc_latency_ms = start.elapsed().as_millis() as u64;

        // Get peer count (may not be supported by all nodes)
        let peer_count = match provider
            .client()
            .request::<_, U64>("net_peerCount", ())
            .await
        {
            Ok(count) => count.to::<u64>(),
            Err(_) => 0, // Some nodes don't expose this
        };

        // Check sync status
        // Use Debug representation to check if syncing (workaround for Alloy SyncStatus API)
        let is_syncing = match provider.syncing().await {
            Ok(status) => {
                // SyncStatus::None means not syncing
                // Any other variant means syncing
                let status_str = format!("{:?}", status);
                !status_str.contains("None")
            }
            Err(_) => false, // Assume not syncing if call fails
        };

        // Verify chain ID
        let chain_id = provider.get_chain_id().await?;
        let chain_id_valid = chain_id == MONAD_MAINNET_CHAIN_ID;

        // Determine overall health
        // Local nodes may have 0 peers (not connected to P2P network)
        // We mainly care about: not syncing, reasonable latency, correct chain
        let is_healthy = !is_syncing && rpc_latency_ms < 500 && chain_id_valid;

        Ok(Self {
            is_healthy,
            block_number,
            peer_count,
            is_syncing,
            rpc_latency_ms,
            chain_id,
            chain_id_valid,
        })
    }

    /// Quick latency check (ping)
    pub async fn ping<P: Provider>(provider: &P) -> Result<Duration> {
        let start = Instant::now();
        let _ = provider.get_block_number().await?;
        Ok(start.elapsed())
    }

    /// Print health status to console
    pub fn print_status(&self) {
        if self.is_healthy {
            println!("\x1b[1;32mNode Health: HEALTHY\x1b[0m");
        } else {
            println!("\x1b[1;31mNode Health: UNHEALTHY\x1b[0m");
        }

        println!("  Block Number: {}", self.block_number);
        println!("  RPC Latency: {}ms", self.rpc_latency_ms);

        if self.peer_count > 0 {
            println!("  Peer Count: {}", self.peer_count);
        } else {
            println!("  Peer Count: N/A (local node or not exposed)");
        }

        if self.is_syncing {
            println!("  \x1b[1;33mWARNING: Node is syncing!\x1b[0m");
        }

        if !self.chain_id_valid {
            println!("  \x1b[1;31mERROR: Chain ID {} does not match Monad mainnet ({})\x1b[0m",
                self.chain_id, MONAD_MAINNET_CHAIN_ID);
        } else {
            println!("  Chain ID: {} (Monad Mainnet)", self.chain_id);
        }
    }
}

/// Verify node is ready for trading operations
/// Returns error if node is not in a good state
pub async fn verify_node_ready<P: Provider>(provider: &P) -> Result<NodeHealth> {
    println!("\nChecking node health...");
    let health = NodeHealth::check(provider).await?;
    health.print_status();

    if health.is_syncing {
        return Err(eyre::eyre!(
            "Node is still syncing at block {}. Wait for sync to complete before trading.",
            health.block_number
        ));
    }

    if !health.chain_id_valid {
        return Err(eyre::eyre!(
            "Connected to wrong chain! Expected Monad mainnet ({}), got chain ID {}",
            MONAD_MAINNET_CHAIN_ID,
            health.chain_id
        ));
    }

    if health.rpc_latency_ms > 500 {
        println!("\x1b[1;33mWARNING: High RPC latency ({}ms). Consider using a local node.\x1b[0m",
            health.rpc_latency_ms);
    }

    println!();
    Ok(health)
}
