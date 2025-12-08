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
use std::collections::{HashMap, VecDeque};
use std::fs::OpenOptions;
use std::io::{stdout, Write};
use std::time::Instant;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::display::calculate_spreads;
use crate::multicall::fetch_prices_batched;
use crate::pools::{
    create_lfj_active_id_call, create_lfj_bin_step_call, create_slot0_call, PoolPrice, PriceCall,
};
use crate::config::{get_lfj_pool, get_monday_trade_pool, get_v3_pools};

/// Helper function to get ANSI color code based on spread level
fn spread_level_color(spread_bps: i32) -> &'static str {
    match spread_bps {
        x if x < 0 => "\x1b[90m",      // Gray - negative
        0..=4 => "\x1b[37m",           // White - noise
        5..=9 => "\x1b[33m",           // Yellow - watching
        10..=14 => "\x1b[32m",         // Green - ready
        15..=24 => "\x1b[1;32m",       // Bold Green - hot
        _ => "\x1b[1;5;32m",           // Bold Blinking Green - critical
    }
}

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

/// Classification of spread at Proposed state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpreadTier {
    Noise,       // < 5 bps - ignore
    SubThreshold, // 5-9 bps - watch only
    Marginal,    // 10-14 bps - maybe actionable
    Actionable,  // 15-24 bps - execute
    Critical,    // 25+ bps - priority execute
}

impl SpreadTier {
    pub fn from_bps(bps: i32) -> Self {
        match bps {
            x if x < 5 => Self::Noise,
            5..=9 => Self::SubThreshold,
            10..=14 => Self::Marginal,
            15..=24 => Self::Actionable,
            _ => Self::Critical,
        }
    }

    #[allow(dead_code)]
    pub fn is_actionable(&self) -> bool {
        matches!(self, Self::Marginal | Self::Actionable | Self::Critical)
    }

    #[allow(dead_code)]
    pub fn color(&self) -> &'static str {
        match self {
            Self::Noise => "\x1b[90m",        // Gray
            Self::SubThreshold => "\x1b[37m", // White
            Self::Marginal => "\x1b[33m",     // Yellow
            Self::Actionable => "\x1b[32m",   // Green
            Self::Critical => "\x1b[1;32m",   // Bold Green
        }
    }
}

/// What happened to an actionable spread
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpreadOutcome {
    /// Was never actionable (< 10bps at Proposed)
    NotActionable,
    /// Still actionable at Finalized (>= 10bps)
    Persisted,
    /// Dropped below actionable but still positive (5-9bps)
    Decayed,
    /// Vanished completely (< 5bps) - captured by competitor
    Captured,
    /// Increased (rare but possible)
    Grew,
}

impl SpreadOutcome {
    pub fn classify(proposed_bps: i32, finalized_bps: i32) -> Self {
        if proposed_bps < 10 {
            return Self::NotActionable;
        }
        match finalized_bps {
            x if x >= proposed_bps => Self::Grew,
            x if x >= 10 => Self::Persisted,
            x if x >= 5 => Self::Decayed,
            _ => Self::Captured,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::NotActionable => "---",
            Self::Persisted => "PERSISTED",
            Self::Decayed => "DECAYED",
            Self::Captured => "CAPTURED",
            Self::Grew => "GREW!",
        }
    }

    pub fn color(&self) -> &'static str {
        match self {
            Self::NotActionable => "\x1b[90m",
            Self::Persisted => "\x1b[1;32m",  // Bold Green - success
            Self::Decayed => "\x1b[33m",      // Yellow - partial
            Self::Captured => "\x1b[31m",     // Red - missed
            Self::Grew => "\x1b[1;36m",       // Cyan - unexpected good
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

fn truncate_name(name: &str, max_len: usize) -> &str {
    if name.len() <= max_len { name } else { &name[..max_len] }
}

/// Real-time running statistics for dashboard display
#[derive(Debug, Default)]
pub struct RunningStats {
    // Block counts
    pub total_blocks: u64,
    pub complete_lifecycles: u64,

    // Timing stats (in milliseconds)
    pub timing_sum: u128,
    pub timing_min: u128,
    pub timing_max: u128,
    pub timing_recent: VecDeque<u128>,  // Last 20 for moving average

    // Spread categorization at Proposed
    pub actionable_count: u64,  // >= 10bps at Proposed
    pub max_spread_seen: i32,
    pub max_spread_block: u64,

    // Outcomes (only for actionable spreads)
    pub persisted_count: u64,   // Still >= 10bps at Finalized
    pub decayed_count: u64,     // 5-9bps at Finalized
    pub captured_count: u64,    // < 5bps at Finalized
    pub grew_count: u64,        // Increased (rare)

    // Spread value tracking
    pub spread_sum_proposed: i64,
    pub spread_sum_finalized: i64,

    // Recent actionable blocks (for display)
    pub recent_actionable: VecDeque<ActionableBlock>,
}

#[derive(Debug, Clone)]
pub struct ActionableBlock {
    pub block_number: u64,
    pub timing_ms: u128,
    pub pair: String,
    pub spread_proposed: i32,
    pub spread_finalized: i32,
    pub outcome: SpreadOutcome,
}

impl RunningStats {
    pub fn new() -> Self {
        Self {
            timing_min: u128::MAX,
            timing_recent: VecDeque::with_capacity(20),
            recent_actionable: VecDeque::with_capacity(10),
            ..Default::default()
        }
    }

    /// Record a completed block lifecycle
    pub fn record(&mut self, lifecycle: &BlockLifecycle) {
        self.complete_lifecycles += 1;

        // Timing
        if let Some(timing) = lifecycle.proposed_to_finalized_ms {
            self.timing_sum += timing;
            self.timing_min = self.timing_min.min(timing);
            self.timing_max = self.timing_max.max(timing);

            if self.timing_recent.len() >= 20 {
                self.timing_recent.pop_front();
            }
            self.timing_recent.push_back(timing);
        }

        // Spreads
        let proposed = lifecycle.spread_at_proposed_bps.unwrap_or(0);
        let finalized = lifecycle.spread_at_finalized_bps.unwrap_or(0);

        self.spread_sum_proposed += proposed as i64;
        self.spread_sum_finalized += finalized as i64;

        // Track max
        if proposed > self.max_spread_seen {
            self.max_spread_seen = proposed;
            self.max_spread_block = lifecycle.block_number;
        }

        // Classify actionable spreads
        if proposed >= 10 {
            self.actionable_count += 1;

            let outcome = SpreadOutcome::classify(proposed, finalized);
            match outcome {
                SpreadOutcome::Persisted => self.persisted_count += 1,
                SpreadOutcome::Decayed => self.decayed_count += 1,
                SpreadOutcome::Captured => self.captured_count += 1,
                SpreadOutcome::Grew => self.grew_count += 1,
                SpreadOutcome::NotActionable => {} // Should never happen
            }

            // Store in recent actionable
            let pair = lifecycle.proposed.as_ref()
                .and_then(|p| p.best_pair.as_ref())
                .map(|(buy, sell)| format!("{}->{}",
                    truncate_name(buy, 8),
                    truncate_name(sell, 8)))
                .unwrap_or_else(|| "Unknown".to_string());

            let block = ActionableBlock {
                block_number: lifecycle.block_number,
                timing_ms: lifecycle.proposed_to_finalized_ms.unwrap_or(0),
                pair,
                spread_proposed: proposed,
                spread_finalized: finalized,
                outcome,
            };

            if self.recent_actionable.len() >= 10 {
                self.recent_actionable.pop_front();
            }
            self.recent_actionable.push_back(block);
        }
    }

    // Computed statistics
    pub fn avg_timing_ms(&self) -> f64 {
        if self.complete_lifecycles == 0 { return 0.0; }
        self.timing_sum as f64 / self.complete_lifecycles as f64
    }

    pub fn min_timing_ms(&self) -> u128 {
        if self.timing_min == u128::MAX { 0 } else { self.timing_min }
    }

    pub fn execution_window_ms(&self) -> u128 {
        // Recommended execution time = min_timing - 100ms safety buffer
        self.min_timing_ms().saturating_sub(100)
    }

    pub fn avg_decay_bps(&self) -> f64 {
        if self.complete_lifecycles == 0 { return 0.0; }
        (self.spread_sum_finalized - self.spread_sum_proposed) as f64
            / self.complete_lifecycles as f64
    }

    pub fn persistence_rate(&self) -> f64 {
        if self.actionable_count == 0 { return 0.0; }
        (self.persisted_count + self.grew_count) as f64 / self.actionable_count as f64 * 100.0
    }

    pub fn capture_rate(&self) -> f64 {
        if self.actionable_count == 0 { return 0.0; }
        self.captured_count as f64 / self.actionable_count as f64 * 100.0
    }

    pub fn decay_rate(&self) -> f64 {
        if self.actionable_count == 0 { return 0.0; }
        self.decayed_count as f64 / self.actionable_count as f64 * 100.0
    }
}

/// Render real-time dashboard (replaces per-block logging)
pub fn render_dashboard(
    stats: &RunningStats,
    start_time: std::time::Instant,
    min_spread_filter: i32,
) -> String {
    let mut out = String::new();

    let runtime = start_time.elapsed();
    let runtime_str = format!("{:02}:{:02}",
        runtime.as_secs() / 60,
        runtime.as_secs() % 60);

    // Clear screen and position cursor at top
    out.push_str("\x1b[H\x1b[2J");

    // Header
    out.push_str("╔══════════════════════════════════════════════════════════════════════════╗\n");
    out.push_str(&format!(
        "║  MEV VALIDATION │ Runtime: {} │ Blocks: {:>5} │ Filter: >{}bps          ║\n",
        runtime_str,
        stats.complete_lifecycles,
        min_spread_filter
    ));
    out.push_str("╠══════════════════════════════════════════════════════════════════════════╣\n");

    // Three-column metrics
    out.push_str("║  TIMING          │ OPPORTUNITIES          │ COMPETITION                  ║\n");
    out.push_str(&format!(
        "║  Avg: {:>5.0}ms    │ Actionable: {:>3}/{:<5}   │ Persisted: {:>3} ({:>4.1}%)        ║\n",
        stats.avg_timing_ms(),
        stats.actionable_count,
        stats.complete_lifecycles,
        stats.persisted_count + stats.grew_count,
        stats.persistence_rate()
    ));
    out.push_str(&format!(
        "║  Min: {:>5}ms    │ Max Seen: {:>+4}bps      │ Captured:  {:>3} ({:>4.1}%)        ║\n",
        stats.min_timing_ms(),
        stats.max_spread_seen,
        stats.captured_count,
        stats.capture_rate()
    ));
    out.push_str(&format!(
        "║  Target: <{:>3}ms  │ Avg Decay: {:>+5.1}bps    │ Decayed:   {:>3} ({:>4.1}%)        ║\n",
        stats.execution_window_ms(),
        stats.avg_decay_bps(),
        stats.decayed_count,
        stats.decay_rate()
    ));
    out.push_str("╠══════════════════════════════════════════════════════════════════════════╣\n");

    // Recent actionable blocks
    out.push_str("║  RECENT ACTIONABLE (>10bps at Proposed)                                  ║\n");
    out.push_str("╠──────────────────────────────────────────────────────────────────────────╣\n");
    out.push_str("║  BLOCK     │  Δt   │ PAIR              │ SPREAD      │ OUTCOME          ║\n");
    out.push_str("╠──────────────────────────────────────────────────────────────────────────╣\n");

    // Show recent actionable blocks (or placeholder)
    if stats.recent_actionable.is_empty() {
        out.push_str("║  \x1b[90mNo actionable spreads (>10bps) detected yet...\x1b[0m                        ║\n");
        for _ in 0..4 {
            out.push_str("║                                                                          ║\n");
        }
    } else {
        for block in stats.recent_actionable.iter().rev().take(5) {
            let trend = if block.spread_finalized > block.spread_proposed { "▲" }
                       else if block.spread_finalized < block.spread_proposed { "▼" }
                       else { "─" };

            out.push_str(&format!(
                "║  {:>8} │ {:>4}ms │ {:<17} │ {:>+3}->{:>+3}bps {} │ {}{:<16}\x1b[0m ║\n",
                block.block_number,
                block.timing_ms,
                block.pair,
                block.spread_proposed,
                block.spread_finalized,
                trend,
                block.outcome.color(),
                block.outcome.label()
            ));
        }
        // Pad remaining rows
        for _ in stats.recent_actionable.len()..5 {
            out.push_str("║                                                                          ║\n");
        }
    }

    out.push_str("╠══════════════════════════════════════════════════════════════════════════╣\n");

    // Dynamic insight based on data
    let insight = generate_insight(stats);
    out.push_str(&format!("║  INSIGHT: {:<63}║\n", insight));

    out.push_str("╚══════════════════════════════════════════════════════════════════════════╝\n");
    out.push_str("\x1b[90m  Press Ctrl+C to stop and view final report\x1b[0m\n");

    out
}

/// Generate actionable insight based on current statistics
fn generate_insight(stats: &RunningStats) -> String {
    if stats.complete_lifecycles < 20 {
        return format!("Collecting data... ({}/20 blocks for initial analysis)",
            stats.complete_lifecycles);
    }

    let capture = stats.capture_rate();
    let persistence = stats.persistence_rate();
    let window = stats.execution_window_ms();

    if stats.actionable_count == 0 {
        return "No spreads >10bps observed. Market may be efficient or illiquid.".to_string();
    }

    if capture > 70.0 {
        format!(
            "HIGH COMPETITION: {:.0}% captured. Need <{}ms exec or mempool.",
            capture, window / 2
        )
    } else if capture > 40.0 {
        format!(
            "MODERATE COMPETITION: {:.0}% captured. Execute within {}ms.",
            capture, window
        )
    } else if persistence > 60.0 {
        format!(
            "LOW COMPETITION: {:.0}% persist! Good opportunity if exec <{}ms.",
            persistence, window
        )
    } else {
        format!(
            "MIXED: {:.0}% captured, {:.0}% decayed. Target {}ms execution.",
            capture, stats.decay_rate(), window
        )
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
    min_spread_bps: i32,
    running_stats: RunningStats,
    output_mode: OutputMode,
}

/// Output mode for MEV validation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Dashboard,
    Log,
    Quiet,
}

impl MevValidator {
    pub fn new(rpc_url: &str, ws_url: &str, min_spread_bps: i32, output_mode: OutputMode) -> Self {
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
            min_spread_bps,
            running_stats: RunningStats::new(),
            output_mode,
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
                    // In log mode, display proposed blocks
                    if self.output_mode == OutputMode::Log && snap.best_spread_bps >= self.min_spread_bps {
                        print!(
                            "\r[PROPOSED]  Block {} | Spread: {:+3}bps | {} -> {}           ",
                            block_num,
                            snap.best_spread_bps,
                            snap.best_pair.as_ref().map(|p| p.0.as_str()).unwrap_or("?"),
                            snap.best_pair.as_ref().map(|p| p.1.as_str()).unwrap_or("?"),
                        );
                        stdout().flush().ok();
                    }
                    lifecycle.proposed = Some(snap);
                }
            }
            "Finalized" => {
                if let Some(snap) = snapshot {
                    lifecycle.finalized = Some(snap);
                }

                // Check if lifecycle is complete
                if lifecycle.is_complete() {
                    lifecycle.compute_analysis();
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
            // Update running statistics
            self.running_stats.record(&completed);

            // Log to file (always, regardless of output mode)
            self.log_lifecycle(&completed);
            self.completed_blocks.push(completed.clone());

            // Output based on mode
            match self.output_mode {
                OutputMode::Dashboard => {
                    // Render dashboard (replaces all println! calls)
                    print!("{}", render_dashboard(
                        &self.running_stats,
                        self.start_time,
                        self.min_spread_bps
                    ));
                    stdout().flush().ok();
                }
                OutputMode::Log => {
                    // Traditional log output
                    let spread_proposed = completed.spread_at_proposed_bps.unwrap_or(0);
                    let spread_final = completed.spread_at_finalized_bps.unwrap_or(0);
                    let delta = completed.spread_delta_bps.unwrap_or(0);

                    let proposed_color = spread_level_color(spread_proposed);
                    let final_color = spread_level_color(spread_final);
                    let delta_color = if delta > 0 { "\x1b[32m" } else if delta < 0 { "\x1b[31m" } else { "\x1b[33m" };

                    println!();
                    println!("\x1b[1m[BLOCK {}]\x1b[0m Δt={}ms",
                        completed.block_number,
                        completed.proposed_to_finalized_ms.unwrap_or(0));
                    println!("  Spread: {}{}bps\x1b[0m -> {}{}bps\x1b[0m ({}{}Δ\x1b[0m)",
                        proposed_color, spread_proposed,
                        final_color, spread_final,
                        delta_color, format!("{:+}", delta));

                    if let Some(ref pair) = completed.proposed.as_ref().and_then(|p| p.best_pair.clone()) {
                        println!("  Pair: {} -> {}", pair.0, pair.1);
                    }

                    let outcome = SpreadOutcome::classify(spread_proposed, spread_final);
                    println!("  Status: {}{}\x1b[0m", outcome.color(), outcome.label());
                }
                OutputMode::Quiet => {
                    // No output during collection
                }
            }
        }

        // Cleanup old incomplete lifecycles (older than 20 blocks)
        // Use saturating_sub to avoid underflow when num > current_block
        let current_block = block_num;
        self.block_lifecycles
            .retain(|&num, _| current_block.saturating_sub(num) < 20);

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

    /// Print comprehensive final report (new dashboard style)
    pub fn print_final_report(&self) {
        let stats = &self.running_stats;

        println!();
        println!("╔══════════════════════════════════════════════════════════════════════════════╗");
        println!("║                      FINAL MEV VALIDATION REPORT                             ║");
        println!("╠══════════════════════════════════════════════════════════════════════════════╣");

        // Session summary
        println!("║  SESSION SUMMARY                                                             ║");
        println!("║    Total Blocks Analyzed:  {:>8}                                           ║", stats.complete_lifecycles);
        println!("║    Duration:               {:>8} seconds                                   ║", self.start_time.elapsed().as_secs());
        let log_display = if self.log_file.len() > 40 { &self.log_file[..40] } else { &self.log_file };
        println!("║    Data File:              {:<40}             ║", log_display);

        println!("╠══════════════════════════════════════════════════════════════════════════════╣");
        println!("║  TIMING ANALYSIS                                                             ║");
        println!("║  ──────────────────────────────────────────────────────────────────────────  ║");
        println!("║    Proposed -> Finalized Window:                                             ║");
        println!("║      Average:     {:>6.0}ms                                                   ║", stats.avg_timing_ms());
        println!("║      Minimum:     {:>6}ms  <- FASTEST POSSIBLE                              ║", stats.min_timing_ms());
        println!("║      Maximum:     {:>6}ms                                                   ║", stats.timing_max);
        println!("║                                                                              ║");
        println!("║    EXECUTION TARGET: Complete arb within {:>4}ms                             ║", stats.execution_window_ms());

        println!("╠══════════════════════════════════════════════════════════════════════════════╣");
        println!("║  OPPORTUNITY ANALYSIS                                                        ║");
        println!("║  ──────────────────────────────────────────────────────────────────────────  ║");

        let actionable_pct = if stats.complete_lifecycles > 0 {
            stats.actionable_count as f64 / stats.complete_lifecycles as f64 * 100.0
        } else { 0.0 };

        println!("║    Actionable Spreads (>=10bps):  {:>5} / {:>5}  ({:>5.1}% of blocks)         ║",
            stats.actionable_count, stats.complete_lifecycles, actionable_pct);
        println!("║    Maximum Spread Observed:       {:>+5}bps at block {:>8}                 ║",
            stats.max_spread_seen, stats.max_spread_block);
        println!("║    Average Spread Decay:          {:>+5.1}bps per block                        ║",
            stats.avg_decay_bps());

        if stats.actionable_count > 0 {
            println!("║                                                                              ║");
            println!("║    OF ACTIONABLE OPPORTUNITIES:                                              ║");
            println!("║      PERSISTED (still >=10bps): {:>4} ({:>5.1}%)  <- CAPTURABLE              ║",
                stats.persisted_count + stats.grew_count, stats.persistence_rate());
            println!("║      DECAYED (5-9bps):          {:>4} ({:>5.1}%)  <- MARGINAL                ║",
                stats.decayed_count, stats.decay_rate());
            println!("║      CAPTURED (<5bps):          {:>4} ({:>5.1}%)  <- MISSED                  ║",
                stats.captured_count, stats.capture_rate());
        }

        println!("╠══════════════════════════════════════════════════════════════════════════════╣");
        println!("║  COMPETITIVE ASSESSMENT                                                      ║");
        println!("║  ──────────────────────────────────────────────────────────────────────────  ║");

        let capture = stats.capture_rate();
        if capture > 70.0 {
            println!("║    HIGH COMPETITION DETECTED                                                 ║");
            println!("║                                                                              ║");
            println!("║    {:.0}% of actionable spreads were captured by faster competitors.         ║", capture);
            println!("║                                                                              ║");
            println!("║    RECOMMENDATIONS:                                                          ║");
            println!("║      1. Reduce execution latency below {}ms                                ║", stats.execution_window_ms() / 2);
            println!("║      2. Implement mempool monitoring for earlier spread detection            ║");
            println!("║      3. Focus on less competitive pairs or larger spreads (>15bps)           ║");
        } else if capture > 40.0 {
            println!("║    MODERATE COMPETITION                                                      ║");
            println!("║                                                                              ║");
            println!("║    {:.0}% captured by others. You can compete with optimizations.            ║", capture);
            println!("║                                                                              ║");
            println!("║    RECOMMENDATIONS:                                                          ║");
            println!("║      1. Target execution under {}ms                                        ║", stats.execution_window_ms());
            println!("║      2. Use atomic arb contract (single TX) for speed                        ║");
            println!("║      3. Pre-build transactions during price monitoring                       ║");
        } else if stats.actionable_count > 0 {
            println!("║    LOW COMPETITION - GOOD OPPORTUNITY                                        ║");
            println!("║                                                                              ║");
            println!("║    {:.0}% of spreads persist long enough for capture!                        ║", stats.persistence_rate());
            println!("║                                                                              ║");
            println!("║    RECOMMENDATIONS:                                                          ║");
            println!("║      1. Execute within {}ms window                                         ║", stats.execution_window_ms());
            println!("║      2. Set trigger threshold at 10-12bps for safety margin                  ║");
            println!("║      3. Monitor for competition increase over time                           ║");
        } else {
            println!("║    INSUFFICIENT DATA                                                         ║");
            println!("║                                                                              ║");
            println!("║    No actionable spreads (>=10bps) were observed during this session.        ║");
            println!("║    This could mean:                                                          ║");
            println!("║      - Market is currently efficient (arbitraged quickly)                    ║");
            println!("║      - Low liquidity / trading volume                                        ║");
            println!("║      - Try running during higher activity periods                            ║");
        }

        println!("╠══════════════════════════════════════════════════════════════════════════════╣");
        println!("║  NEXT STEPS                                                                  ║");
        println!("║  ──────────────────────────────────────────────────────────────────────────  ║");
        println!("║    1. Review detailed data: cat {}               ║", log_display);
        println!("║    2. Test execution timing: cargo run -- fast-arb --sell-dex X --buy-dex Y  ║");
        println!("║    3. Deploy atomic contract for single-TX execution                         ║");
        println!("║    4. Run auto-arb with validated parameters                                 ║");
        println!("╚══════════════════════════════════════════════════════════════════════════════╝");
        println!();
    }
}

/// Core validation loop - internal implementation
async fn run_validation_core(
    rpc_url: &str,
    ws_url: &str,
    duration_secs: u64,
    min_spread_bps: i32,
    output_mode: OutputMode
) -> Result<MevValidator> {
    use tokio::time::{timeout, Duration};

    let mut validator = MevValidator::new(rpc_url, ws_url, min_spread_bps, output_mode);

    // Connect to WebSocket
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

    let deadline = Duration::from_secs(duration_secs);
    let start = Instant::now();

    loop {
        if start.elapsed() >= deadline {
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

    Ok(validator)
}

/// Dashboard mode - full screen interactive dashboard
pub async fn run_mev_validation_dashboard(
    rpc_url: &str,
    ws_url: &str,
    duration_secs: u64,
    min_spread_bps: i32
) -> Result<()> {
    // Enter alternate screen for clean dashboard
    print!("\x1b[?1049h"); // Alternate screen buffer
    print!("\x1b[?25l");   // Hide cursor
    stdout().flush().ok();

    let validator = run_validation_core(rpc_url, ws_url, duration_secs, min_spread_bps, OutputMode::Dashboard).await?;

    // On exit: restore terminal and print final stats
    print!("\x1b[?1049l"); // Exit alternate screen
    print!("\x1b[?25h");   // Show cursor
    stdout().flush().ok();

    // Print final comprehensive report
    validator.print_final_report();

    Ok(())
}

/// Log mode - traditional line-by-line output (good for piping to files)
pub async fn run_mev_validation_log(
    rpc_url: &str,
    ws_url: &str,
    duration_secs: u64,
    min_spread_bps: i32
) -> Result<()> {
    let rpc_display = if rpc_url.len() > 52 { &rpc_url[..52] } else { rpc_url };
    let ws_display = if ws_url.len() > 52 { &ws_url[..52] } else { ws_url };

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║              MEV VALIDATION - LOG MODE                       ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  RPC: {:<52} ║", rpc_display);
    println!("║  WS:  {:<52} ║", ws_display);
    println!("║  Duration: {} seconds | Min Spread: {}bps                    ║",
        duration_secs, min_spread_bps);
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();
    println!("Connecting to WebSocket...");

    let validator = run_validation_core(rpc_url, ws_url, duration_secs, min_spread_bps, OutputMode::Log).await?;

    println!("\n\nValidation period complete.");
    validator.print_final_report();

    Ok(())
}

/// Quiet mode - no output during collection, only final report
pub async fn run_mev_validation_quiet(
    rpc_url: &str,
    ws_url: &str,
    duration_secs: u64,
    min_spread_bps: i32
) -> Result<()> {
    println!("Starting MEV validation (quiet mode)...");
    println!("Duration: {} seconds | Min Spread: {}bps", duration_secs, min_spread_bps);
    println!("Collecting data...\n");

    let validator = run_validation_core(rpc_url, ws_url, duration_secs, min_spread_bps, OutputMode::Quiet).await?;

    validator.print_final_report();

    Ok(())
}

/// Main validation loop using single monadNewHeads subscription (default: dashboard mode)
pub async fn run_mev_validation(rpc_url: &str, ws_url: &str, duration_secs: u64, min_spread_bps: i32) -> Result<()> {
    run_mev_validation_dashboard(rpc_url, ws_url, duration_secs, min_spread_bps).await
}
