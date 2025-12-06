//! Arbitrage execution statistics and logging
//!
//! Tracks detailed stats for every arb attempt to understand real-world behavior.

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::fs::{OpenOptions, File};
use std::io::{Write, BufWriter};
use std::path::PathBuf;

/// Detailed snapshot before arb execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreExecutionSnapshot {
    pub timestamp: String,
    pub wmon_balance: f64,
    pub usdc_balance: f64,
    pub mon_balance: f64,

    // Detected opportunity
    pub sell_dex: String,
    pub sell_price: f64,
    pub buy_dex: String,
    pub buy_price: f64,
    pub gross_spread_bps: i32,
    pub net_spread_bps: i32,

    // Execution parameters
    pub amount_wmon: f64,
    pub expected_usdc: f64,
    pub expected_wmon_back: f64,
    pub slippage_bps: u32,
}

/// Detailed snapshot after arb execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostExecutionSnapshot {
    pub timestamp: String,
    pub wmon_balance: f64,
    pub usdc_balance: f64,
    pub mon_balance: f64,

    // Execution results
    pub swap1_success: bool,
    pub swap1_tx_hash: String,
    pub swap1_gas_used: u64,
    pub swap1_gas_estimated: u64,

    pub swap2_success: bool,
    pub swap2_tx_hash: String,
    pub swap2_gas_used: u64,
    pub swap2_gas_estimated: u64,

    // Actual amounts
    pub actual_usdc_received: f64,
    pub actual_wmon_back: f64,

    // P&L
    pub wmon_delta: f64,           // wmon_after - wmon_before
    pub usdc_delta: f64,           // usdc_after - usdc_before (should be ~0)
    pub mon_delta: f64,            // mon_after - mon_before (gas costs)
    pub total_gas_cost_mon: f64,
    pub net_profit_wmon: f64,      // wmon_delta - gas equivalent
    pub net_profit_bps: i32,

    // Timing
    pub total_execution_ms: u128,
}

/// Complete arb execution record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbExecutionRecord {
    pub id: u64,
    pub pre: PreExecutionSnapshot,
    pub post: Option<PostExecutionSnapshot>,
    pub success: bool,
    pub error: Option<String>,
}

/// Stats logger that writes to JSON Lines file
pub struct StatsLogger {
    file_path: PathBuf,
    execution_count: u64,
}

impl StatsLogger {
    pub fn new(file_name: &str) -> Self {
        let file_path = PathBuf::from(file_name);
        Self {
            file_path,
            execution_count: 0,
        }
    }

    pub fn next_id(&mut self) -> u64 {
        self.execution_count += 1;
        self.execution_count
    }

    /// Log a complete execution record (append as JSON line)
    pub fn log_execution(&self, record: &ArbExecutionRecord) {
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file_path)
        {
            Ok(file) => {
                let mut writer = BufWriter::new(file);
                if let Ok(json) = serde_json::to_string(record) {
                    let _ = writeln!(writer, "{}", json);
                }
            }
            Err(e) => {
                eprintln!("Failed to write stats: {}", e);
            }
        }
    }
}

/// Print pre-execution snapshot to console
pub fn print_pre_execution(snap: &PreExecutionSnapshot) {
    println!();
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║              PRE-EXECUTION SNAPSHOT                          ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  Time: {}                            ║", &snap.timestamp[..19]);
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  BALANCES BEFORE:                                            ║");
    println!("║    MON:  {:>18.6}                                ║", snap.mon_balance);
    println!("║    WMON: {:>18.6}                                ║", snap.wmon_balance);
    println!("║    USDC: {:>18.6}                                ║", snap.usdc_balance);
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  OPPORTUNITY DETECTED:                                       ║");
    println!("║    Sell on: {:<12} @ {:.6} USDC/WMON              ║", snap.sell_dex, snap.sell_price);
    println!("║    Buy on:  {:<12} @ {:.6} USDC/WMON              ║", snap.buy_dex, snap.buy_price);
    println!("║    Gross Spread: {:>+6} bps                                 ║", snap.gross_spread_bps);
    println!("║    Net Spread:   {:>+6} bps (after fees)                    ║", snap.net_spread_bps);
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  EXECUTION PLAN:                                             ║");
    println!("║    Input:    {:>12.6} WMON                           ║", snap.amount_wmon);
    println!("║    Expected: {:>12.6} USDC (intermediate)            ║", snap.expected_usdc);
    println!("║    Expected: {:>12.6} WMON (output)                  ║", snap.expected_wmon_back);
    println!("║    Slippage: {:>12} bps                              ║", snap.slippage_bps);
    println!("╚══════════════════════════════════════════════════════════════╝");
}

/// Print post-execution snapshot to console
pub fn print_post_execution(pre: &PreExecutionSnapshot, post: &PostExecutionSnapshot) {
    println!();
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║              POST-EXECUTION SNAPSHOT                         ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  SWAP 1 (Sell WMON -> USDC on {}):                       ║", &pre.sell_dex);
    println!("║    Status: {}                                            ║",
        if post.swap1_success { "SUCCESS" } else { "FAILED " });
    if post.swap1_tx_hash.len() >= 42 {
        println!("║    TX: {}...                     ║", &post.swap1_tx_hash[..42]);
    } else if !post.swap1_tx_hash.is_empty() {
        println!("║    TX: {}                                          ║", &post.swap1_tx_hash);
    }
    println!("║    Gas: {} used / {} limit                       ║", post.swap1_gas_used, post.swap1_gas_estimated);
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  SWAP 2 (Buy USDC -> WMON on {}):                        ║", &pre.buy_dex);
    println!("║    Status: {}                                            ║",
        if post.swap2_success { "SUCCESS" } else { "FAILED " });
    if !post.swap2_tx_hash.is_empty() && post.swap2_tx_hash.len() >= 42 {
        println!("║    TX: {}...                     ║", &post.swap2_tx_hash[..42]);
    } else if !post.swap2_tx_hash.is_empty() {
        println!("║    TX: {}                                          ║", &post.swap2_tx_hash);
    }
    println!("║    Gas: {} used / {} limit                       ║", post.swap2_gas_used, post.swap2_gas_estimated);
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  BALANCES AFTER:                                             ║");
    println!("║    MON:  {:>18.6}  (Δ {:>+12.6})            ║", post.mon_balance, post.mon_delta);
    println!("║    WMON: {:>18.6}  (Δ {:>+12.6})            ║", post.wmon_balance, post.wmon_delta);
    println!("║    USDC: {:>18.6}  (Δ {:>+12.6})            ║", post.usdc_balance, post.usdc_delta);
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  PROFIT/LOSS ANALYSIS:                                       ║");
    let profit_color = if post.net_profit_wmon >= 0.0 { "32" } else { "31" };
    println!("║    WMON P/L:     \x1b[1;{}m{:>+18.6}\x1b[0m                      ║", profit_color, post.wmon_delta);
    println!("║    Gas Cost:     {:>18.6} MON                    ║", post.total_gas_cost_mon);
    println!("║    Net Profit:   \x1b[1;{}m{:>+18.6} WMON ({:>+5} bps)\x1b[0m    ║",
        profit_color, post.net_profit_wmon, post.net_profit_bps);
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  Execution Time: {:>6} ms                                   ║", post.total_execution_ms);
    println!("╚══════════════════════════════════════════════════════════════╝");
}
