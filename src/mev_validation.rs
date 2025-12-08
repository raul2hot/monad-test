//! MEV Strategy Validation Module (v2)
//!
//! Phase 1: Observation and measurement only - NO EXECUTION
//!
//! Key insight: monadNewHeads provides ALL block states in one subscription.
//! We filter by commitState to track blocks through their lifecycle.

use chrono::Local;
use eyre::Result;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::time::Instant;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::display::calculate_spreads;
use crate::multicall::fetch_prices_batched;
use crate::pools::{
    create_lfj_active_id_call, create_lfj_bin_step_call, create_slot0_call, PoolPrice, PriceCall,
};
use crate::config::{get_lfj_pool, get_monday_trade_pool, get_v3_pools};

/// Block commit states in Monad lifecycle
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommitState {
    Proposed,  // Just proposed by leader (EARLIEST)
    Voted,     // Has QC (quorum certificate)
    Finalized, // QC-on-QC confirmed
    Verified,  // Merkle root confirmed (D=3 blocks later)
}

impl CommitState {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "Proposed" => Some(Self::Proposed),
            "Voted" => Some(Self::Voted),
            "Finalized" => Some(Self::Finalized),
            "Verified" => Some(Self::Verified),
            _ => None,
        }
    }
}

/// Block header from monadNewHeads subscription
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonadBlockHeader {
    pub number: String,       // Hex block number
    pub hash: String,
    pub commit_state: String, // "Proposed", "Voted", "Finalized", "Verified"
    pub timestamp: String,    // Hex timestamp
    #[serde(default)]
    pub miner: String,
}

impl MonadBlockHeader {
    pub fn block_number(&self) -> u64 {
        u64::from_str_radix(self.number.trim_start_matches("0x"), 16).unwrap_or(0)
    }

    #[allow(dead_code)]
    pub fn state(&self) -> Option<CommitState> {
        CommitState::from_str(&self.commit_state)
    }
}

/// Snapshot of prices at a specific block state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceSnapshot {
    pub block_number: u64,
    pub commit_state: String,
    pub timestamp_ms: u128,              // Time since validation start
    pub wall_clock: String,              // Human readable timestamp
    pub prices: Vec<PoolPriceRecord>,
    pub best_spread_bps: i32,            // Best net spread at this moment
    pub best_pair: Option<(String, String)>, // (buy_pool, sell_pool)
}

/// Simplified price record for logging
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolPriceRecord {
    pub pool_name: String,
    pub price: f64,
    pub fee_bps: u32,
}

impl From<&PoolPrice> for PoolPriceRecord {
    fn from(p: &PoolPrice) -> Self {
        Self {
            pool_name: p.pool_name.clone(),
            price: p.price,
            fee_bps: p.fee_bps,
        }
    }
}

/// Track a single block through its lifecycle
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockLifecycle {
    pub block_number: u64,
    pub proposed: Option<PriceSnapshot>,
    pub voted: Option<PriceSnapshot>,
    pub finalized: Option<PriceSnapshot>,
    pub verified: Option<PriceSnapshot>,

    // Timing analysis (filled when we have both proposed and finalized)
    pub proposed_to_finalized_ms: Option<u128>,

    // Spread analysis
    pub spread_at_proposed_bps: Option<i32>,
    pub spread_at_finalized_bps: Option<i32>,
    pub spread_delta_bps: Option<i32>,
    pub spread_persisted: Option<bool>, // Was spread >10bps at finalized?
}

impl BlockLifecycle {
    fn new(block_number: u64) -> Self {
        Self {
            block_number,
            proposed: None,
            voted: None,
            finalized: None,
            verified: None,
            proposed_to_finalized_ms: None,
            spread_at_proposed_bps: None,
            spread_at_finalized_bps: None,
            spread_delta_bps: None,
            spread_persisted: None,
        }
    }

    fn is_complete(&self) -> bool {
        self.proposed.is_some() && self.finalized.is_some()
    }

    fn compute_analysis(&mut self) {
        if let (Some(proposed), Some(finalized)) = (&self.proposed, &self.finalized) {
            self.proposed_to_finalized_ms = Some(finalized.timestamp_ms - proposed.timestamp_ms);
            self.spread_at_proposed_bps = Some(proposed.best_spread_bps);
            self.spread_at_finalized_bps = Some(finalized.best_spread_bps);
            self.spread_delta_bps = Some(finalized.best_spread_bps - proposed.best_spread_bps);
            self.spread_persisted = Some(finalized.best_spread_bps > 10);
        }
    }
}

/// Aggregated validation statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationStats {
    pub total_blocks_observed: u64,
    pub complete_lifecycles: u64,           // Have both Proposed and Finalized
    pub blocks_with_spread_gt_10bps: u64,   // At Proposed state
    pub blocks_where_spread_persisted: u64,
    pub avg_proposed_to_finalized_ms: f64,
    pub min_proposed_to_finalized_ms: u128,
    pub max_proposed_to_finalized_ms: u128,
    pub avg_spread_at_proposed_bps: f64,
    pub avg_spread_at_finalized_bps: f64,
    pub avg_spread_decay_bps: f64,
    pub max_spread_seen_bps: i32,
    pub persistence_rate_pct: f64, // % of spreads >10bps that survived
}

/// MEV Validation Runner
pub struct MevValidator {
    ws_url: String,
    rpc_url: String,
    price_calls: Vec<PriceCall>,
    start_time: Instant,
    block_lifecycles: HashMap<u64, BlockLifecycle>,
    completed_blocks: Vec<BlockLifecycle>,
    log_file: String,
}

impl MevValidator {
    pub fn new(rpc_url: &str, ws_url: &str) -> Self {
        // Build price calls (same as monitor)
        let mut price_calls: Vec<PriceCall> = Vec::new();
        for pool in get_v3_pools() {
            price_calls.push(create_slot0_call(&pool));
        }
        let lfj_pool = get_lfj_pool();
        price_calls.push(create_lfj_active_id_call(&lfj_pool));
        price_calls.push(create_lfj_bin_step_call(&lfj_pool));
        let monday_pool = get_monday_trade_pool();
        price_calls.push(create_slot0_call(&monday_pool));

        let timestamp = Local::now().format("%Y%m%d_%H%M%S");
        let log_file = format!("mev_validation_{}.jsonl", timestamp);

        Self {
            ws_url: ws_url.to_string(),
            rpc_url: rpc_url.to_string(),
            price_calls,
            start_time: Instant::now(),
            block_lifecycles: HashMap::new(),
            completed_blocks: Vec::new(),
            log_file,
        }
    }

    /// Fetch current prices and calculate best spread
    async fn snapshot_prices(&self, block_number: u64, state: &str) -> Result<PriceSnapshot> {
        let url: reqwest::Url = self.rpc_url.parse()?;
        let provider = alloy::providers::ProviderBuilder::new().connect_http(url);

        let (prices, _) = fetch_prices_batched(&provider, self.price_calls.clone()).await?;

        let spreads = calculate_spreads(&prices);
        let best = spreads.first();

        let (best_spread_bps, best_pair) = match best {
            Some(s) => (
                (s.net_spread_pct * 100.0) as i32,
                Some((s.buy_pool.clone(), s.sell_pool.clone())),
            ),
            None => (0, None),
        };

        Ok(PriceSnapshot {
            block_number,
            commit_state: state.to_string(),
            timestamp_ms: self.start_time.elapsed().as_millis(),
            wall_clock: Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
            prices: prices.iter().map(|p| p.into()).collect(),
            best_spread_bps,
            best_pair,
        })
    }

    /// Log completed block lifecycle to JSONL file
    fn log_lifecycle(&self, lifecycle: &BlockLifecycle) {
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_file)
        {
            if let Ok(json) = serde_json::to_string(lifecycle) {
                let _ = writeln!(file, "{}", json);
            }
        }
    }

    /// Process a block header from monadNewHeads
    async fn handle_block(&mut self, header: MonadBlockHeader) -> Result<()> {
        let block_num = header.block_number();
        let state = header.commit_state.clone();

        // Snapshot prices BEFORE getting mutable lifecycle reference
        // Only snapshot prices for Proposed and Finalized (save RPC calls)
        let snapshot = match state.as_str() {
            "Proposed" | "Finalized" => Some(self.snapshot_prices(block_num, &state).await?),
            _ => None,
        };

        // Capture timestamp for Voted/Verified states
        let timestamp_ms = self.start_time.elapsed().as_millis();

        // Get or create lifecycle tracker for this block
        let lifecycle = self
            .block_lifecycles
            .entry(block_num)
            .or_insert_with(|| BlockLifecycle::new(block_num));

        // Variable to track if we need to log a completed lifecycle
        let mut completed_lifecycle: Option<BlockLifecycle> = None;

        // Store snapshot in appropriate slot
        match state.as_str() {
            "Proposed" => {
                if let Some(snap) = snapshot {
                    print!(
                        "\r[PROPOSED]  Block {} | Spread: {:+3}bps | {} -> {}           ",
                        block_num,
                        snap.best_spread_bps,
                        snap.best_pair.as_ref().map(|p| p.0.as_str()).unwrap_or("?"),
                        snap.best_pair.as_ref().map(|p| p.1.as_str()).unwrap_or("?"),
                    );
                    std::io::stdout().flush().ok();
                    lifecycle.proposed = Some(snap);
                }
            }
            "Finalized" => {
                if let Some(snap) = snapshot {
                    lifecycle.finalized = Some(snap);
                }

                // Debug: check state of lifecycle
                eprintln!("[DEBUG] Block {} Finalized: proposed={}, finalized={}",
                    block_num,
                    lifecycle.proposed.is_some(),
                    lifecycle.finalized.is_some());

                // Check if lifecycle is complete
                if lifecycle.is_complete() {
                    lifecycle.compute_analysis();

                    // Print comparison
                    println!();
                    println!(
                        "[FINALIZED] Block {} | Dt: {:>4}ms | Spread: {:+3} -> {:+3} bps (D{:+3}) | {}",
                        block_num,
                        lifecycle.proposed_to_finalized_ms.unwrap_or(0),
                        lifecycle.spread_at_proposed_bps.unwrap_or(0),
                        lifecycle.spread_at_finalized_bps.unwrap_or(0),
                        lifecycle.spread_delta_bps.unwrap_or(0),
                        if lifecycle.spread_persisted.unwrap_or(false) {
                            "PERSISTED"
                        } else {
                            "DECAYED"
                        }
                    );

                    // Clone lifecycle for logging after we release the mutable borrow
                    completed_lifecycle = Some(lifecycle.clone());
                }
            }
            "Voted" => {
                lifecycle.voted = Some(PriceSnapshot {
                    block_number: block_num,
                    commit_state: state,
                    timestamp_ms,
                    wall_clock: Local::now().format("%H:%M:%S%.3f").to_string(),
                    prices: vec![],
                    best_spread_bps: 0,
                    best_pair: None,
                });
            }
            "Verified" => {
                lifecycle.verified = Some(PriceSnapshot {
                    block_number: block_num,
                    commit_state: state,
                    timestamp_ms,
                    wall_clock: Local::now().format("%H:%M:%S%.3f").to_string(),
                    prices: vec![],
                    best_spread_bps: 0,
                    best_pair: None,
                });
            }
            _ => {}
        }

        // Log and store completed lifecycle (after releasing mutable borrow)
        if let Some(completed) = completed_lifecycle {
            self.log_lifecycle(&completed);
            self.completed_blocks.push(completed);
        }

        // Cleanup old incomplete lifecycles (older than 20 blocks)
        let current_block = block_num;
        self.block_lifecycles
            .retain(|&num, _| current_block - num < 20);

        Ok(())
    }

    /// Calculate aggregate statistics
    pub fn calculate_stats(&self) -> ValidationStats {
        let completed: Vec<_> = self
            .completed_blocks
            .iter()
            .filter(|b| b.is_complete())
            .collect();

        let total = completed.len() as u64;
        if total == 0 {
            return ValidationStats {
                total_blocks_observed: self.block_lifecycles.len() as u64,
                complete_lifecycles: 0,
                blocks_with_spread_gt_10bps: 0,
                blocks_where_spread_persisted: 0,
                avg_proposed_to_finalized_ms: 0.0,
                min_proposed_to_finalized_ms: 0,
                max_proposed_to_finalized_ms: 0,
                avg_spread_at_proposed_bps: 0.0,
                avg_spread_at_finalized_bps: 0.0,
                avg_spread_decay_bps: 0.0,
                max_spread_seen_bps: 0,
                persistence_rate_pct: 0.0,
            };
        }

        let with_spread: u64 = completed
            .iter()
            .filter(|b| b.spread_at_proposed_bps.unwrap_or(0) > 10)
            .count() as u64;

        let persisted: u64 = completed
            .iter()
            .filter(|b| b.spread_persisted.unwrap_or(false))
            .count() as u64;

        let times: Vec<u128> = completed
            .iter()
            .filter_map(|b| b.proposed_to_finalized_ms)
            .collect();

        let avg_time = if !times.is_empty() {
            times.iter().sum::<u128>() as f64 / times.len() as f64
        } else {
            0.0
        };

        let avg_proposed = completed
            .iter()
            .filter_map(|b| b.spread_at_proposed_bps)
            .map(|s| s as f64)
            .sum::<f64>()
            / total as f64;

        let avg_finalized = completed
            .iter()
            .filter_map(|b| b.spread_at_finalized_bps)
            .map(|s| s as f64)
            .sum::<f64>()
            / total as f64;

        let avg_decay = completed
            .iter()
            .filter_map(|b| b.spread_delta_bps)
            .map(|d| d as f64)
            .sum::<f64>()
            / total as f64;

        let max_spread = completed
            .iter()
            .filter_map(|b| b.spread_at_proposed_bps)
            .max()
            .unwrap_or(0);

        let persistence_rate = if with_spread > 0 {
            (persisted as f64 / with_spread as f64) * 100.0
        } else {
            0.0
        };

        ValidationStats {
            total_blocks_observed: self.block_lifecycles.len() as u64 + total,
            complete_lifecycles: total,
            blocks_with_spread_gt_10bps: with_spread,
            blocks_where_spread_persisted: persisted,
            avg_proposed_to_finalized_ms: avg_time,
            min_proposed_to_finalized_ms: times.iter().copied().min().unwrap_or(0),
            max_proposed_to_finalized_ms: times.iter().copied().max().unwrap_or(0),
            avg_spread_at_proposed_bps: avg_proposed,
            avg_spread_at_finalized_bps: avg_finalized,
            avg_spread_decay_bps: avg_decay,
            max_spread_seen_bps: max_spread,
            persistence_rate_pct: persistence_rate,
        }
    }

    /// Print statistics summary
    pub fn print_stats(&self) {
        let stats = self.calculate_stats();

        println!();
        println!("╔══════════════════════════════════════════════════════════════╗");
        println!("║              MEV VALIDATION STATISTICS                       ║");
        println!("╠══════════════════════════════════════════════════════════════╣");
        println!("║  BLOCK COVERAGE                                              ║");
        println!(
            "║    Total Observed:       {:>6}                              ║",
            stats.total_blocks_observed
        );
        println!(
            "║    Complete Lifecycles:  {:>6}                              ║",
            stats.complete_lifecycles
        );
        println!("╠══════════════════════════════════════════════════════════════╣");
        println!("║  TIMING (Proposed -> Finalized)                              ║");
        println!(
            "║    Average:              {:>6.1}ms                            ║",
            stats.avg_proposed_to_finalized_ms
        );
        println!(
            "║    Min:                  {:>6}ms                            ║",
            stats.min_proposed_to_finalized_ms
        );
        println!(
            "║    Max:                  {:>6}ms                            ║",
            stats.max_proposed_to_finalized_ms
        );
        println!("╠══════════════════════════════════════════════════════════════╣");
        println!("║  SPREAD ANALYSIS                                             ║");
        println!(
            "║    Avg @ Proposed:       {:>+6.1}bps                          ║",
            stats.avg_spread_at_proposed_bps
        );
        println!(
            "║    Avg @ Finalized:      {:>+6.1}bps                          ║",
            stats.avg_spread_at_finalized_bps
        );
        println!(
            "║    Avg Decay:            {:>+6.1}bps                          ║",
            stats.avg_spread_decay_bps
        );
        println!(
            "║    Max Spread Seen:      {:>+6}bps                          ║",
            stats.max_spread_seen_bps
        );
        println!("╠══════════════════════════════════════════════════════════════╣");
        println!("║  OPPORTUNITY ANALYSIS                                        ║");
        println!(
            "║    Spreads >10bps:       {:>6} ({:>5.1}% of blocks)          ║",
            stats.blocks_with_spread_gt_10bps,
            if stats.complete_lifecycles > 0 {
                stats.blocks_with_spread_gt_10bps as f64 / stats.complete_lifecycles as f64 * 100.0
            } else {
                0.0
            }
        );
        println!(
            "║    Persisted to Final:   {:>6} ({:>5.1}% persistence)        ║",
            stats.blocks_where_spread_persisted, stats.persistence_rate_pct
        );
        println!("╚══════════════════════════════════════════════════════════════╝");
        println!();
        println!("  Data saved to: {}", self.log_file);
    }
}

/// Main validation loop using single monadNewHeads subscription
pub async fn run_mev_validation(rpc_url: &str, ws_url: &str, duration_secs: u64) -> Result<()> {
    use tokio::time::{timeout, Duration};

    let rpc_display = if rpc_url.len() > 52 {
        &rpc_url[..52]
    } else {
        rpc_url
    };
    let ws_display = if ws_url.len() > 52 {
        &ws_url[..52]
    } else {
        ws_url
    };

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║              MEV VALIDATION - PHASE 1 (v2)                   ║");
    println!("║              Observation Mode (No Execution)                 ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  RPC: {:<52} ║", rpc_display);
    println!("║  WS:  {:<52} ║", ws_display);
    println!(
        "║  Duration: {} seconds                                        ║",
        duration_secs
    );
    println!("║                                                              ║");
    println!("║  Strategy: Subscribe monadNewHeads, filter by commitState   ║");
    println!("║  Tracking: Proposed -> Finalized timing & spread decay      ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();

    let mut validator = MevValidator::new(rpc_url, ws_url);

    // Connect to WebSocket
    println!("Connecting to WebSocket...");
    let (ws_stream, _) = connect_async(ws_url).await?;
    let (mut write, mut read) = ws_stream.split();

    // Subscribe to monadNewHeads
    let subscribe_msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_subscribe",
        "params": ["monadNewHeads"]
    });
    write
        .send(Message::Text(subscribe_msg.to_string()))
        .await?;

    println!("Subscribed to monadNewHeads. Listening for blocks...\n");
    println!("Press Ctrl+C to stop early.\n");

    let deadline = Duration::from_secs(duration_secs);
    let start = Instant::now();

    loop {
        if start.elapsed() >= deadline {
            println!("\n\nValidation period complete.");
            break;
        }

        // Read next message with timeout
        let msg_result = timeout(Duration::from_secs(5), read.next()).await;

        match msg_result {
            Ok(Some(Ok(Message::Text(text)))) => {
                // Parse the subscription notification
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    // Skip subscription confirmation
                    if json.get("result").is_some() && json.get("id").is_some() {
                        continue;
                    }

                    // Extract block header from subscription notification
                    if let Some(params) = json.get("params") {
                        if let Some(result) = params.get("result") {
                            if let Ok(header) =
                                serde_json::from_value::<MonadBlockHeader>(result.clone())
                            {
                                if let Err(e) = validator.handle_block(header).await {
                                    eprintln!("\nError handling block: {}", e);
                                }
                            }
                        }
                    }
                }
            }
            Ok(Some(Ok(Message::Ping(data)))) => {
                // Respond to ping with pong
                let _ = write.send(Message::Pong(data)).await;
            }
            Ok(Some(Err(e))) => {
                eprintln!("\nWebSocket error: {}", e);
                break;
            }
            Ok(None) => {
                eprintln!("\nWebSocket closed");
                break;
            }
            Err(_) => {
                // Timeout - just continue
            }
            _ => {}
        }
    }

    validator.print_stats();

    Ok(())
}
