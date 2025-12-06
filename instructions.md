# Monad Arbitrage Bot - Phase 1 & 2 Implementation Instructions

## Overview

Wire the existing price monitor to the fast arbitrage execution system. This creates an automated arbitrage bot that detects opportunities and executes them.

**CRITICAL**: Do NOT modify existing functions unless explicitly instructed. Add new code alongside existing code.

---

## Phase 1: Auto-Arb Command with Detailed Stats

### Goal
Create a new CLI command `auto-arb` that:
1. Continuously monitors prices (like `monitor` command)
2. Detects arbitrage opportunities based on net spread threshold
3. Executes fast arbitrage when opportunity is found
4. Logs detailed statistics before/during/after execution
5. Works with configurable small amounts for testing

### File Changes Required

---

### 1. Create New File: `src/stats.rs`

Purpose: Track and log detailed execution statistics.

```rust
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
    println!("║  SWAP 1 (Sell WMON → USDC on {}):                       ║", &pre.sell_dex);
    println!("║    Status: {}                                            ║", 
        if post.swap1_success { "SUCCESS" } else { "FAILED " });
    println!("║    TX: {}...                     ║", &post.swap1_tx_hash[..42]);
    println!("║    Gas: {} used / {} limit                       ║", post.swap1_gas_used, post.swap1_gas_estimated);
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  SWAP 2 (Buy USDC → WMON on {}):                        ║", &pre.buy_dex);
    println!("║    Status: {}                                            ║",
        if post.swap2_success { "SUCCESS" } else { "FAILED " });
    if !post.swap2_tx_hash.is_empty() {
        println!("║    TX: {}...                     ║", &post.swap2_tx_hash[..42.min(post.swap2_tx_hash.len())]);
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
```

**Add to Cargo.toml** (if not present):
```toml
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
```

---

### 2. Modify `src/main.rs`

#### 2.1 Add module declaration (after existing mod declarations around line 30):

```rust
mod stats;
```

#### 2.2 Add new imports (add to existing use statements):

```rust
use stats::{
    StatsLogger, ArbExecutionRecord, PreExecutionSnapshot, PostExecutionSnapshot,
    print_pre_execution, print_post_execution,
};
```

#### 2.3 Add new CLI command in `Commands` enum (after `FastArb`):

```rust
    /// Automated arbitrage: monitors prices and executes when opportunity found
    AutoArb {
        /// Minimum net spread in bps to trigger execution (e.g., -50 for testing, 10 for production)
        #[arg(long, default_value = "-100")]
        min_spread_bps: i32,

        /// Amount of WMON per arb execution
        #[arg(long, default_value = "0.1")]
        amount: f64,

        /// Slippage tolerance in bps
        #[arg(long, default_value = "200")]
        slippage: u32,
        
        /// Maximum executions (0 = unlimited)
        #[arg(long, default_value = "1")]
        max_executions: u32,
        
        /// Cooldown between executions in seconds
        #[arg(long, default_value = "10")]
        cooldown_secs: u64,
        
        /// Dry run mode (detect but don't execute)
        #[arg(long, default_value = "false")]
        dry_run: bool,
    },
```

#### 2.4 Add command handler in main match statement (before closing brace):

```rust
        Some(Commands::AutoArb { 
            min_spread_bps, 
            amount, 
            slippage, 
            max_executions,
            cooldown_secs,
            dry_run,
        }) => {
            run_auto_arb(min_spread_bps, amount, slippage, max_executions, cooldown_secs, dry_run).await
        }
```

#### 2.5 Add the `run_auto_arb` function (add before `#[tokio::main]`):

```rust
/// Automated arbitrage: monitors and executes when spread opportunity detected
async fn run_auto_arb(
    min_spread_bps: i32,
    amount: f64,
    slippage: u32,
    max_executions: u32,
    cooldown_secs: u64,
    dry_run: bool,
) -> Result<()> {
    use crate::display::calculate_spreads;
    use chrono::Local;
    
    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");

    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url.clone());

    let signer = PrivateKeySigner::from_str(&private_key)?;
    let signer_address = signer.address();
    
    // Initialize nonce
    init_nonce(&provider, signer_address).await?;

    // Create provider with signer (reused for all executions)
    let wallet = EthereumWallet::from(signer);
    let provider_with_signer = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(url);

    // Initialize stats logger
    let timestamp = Local::now().format("%Y%m%d_%H%M%S");
    let stats_file = format!("arb_stats_{}.jsonl", timestamp);
    let mut stats_logger = StatsLogger::new(&stats_file);

    println!("═══════════════════════════════════════════════════════════════");
    println!("  AUTO-ARB BOT STARTED");
    println!("═══════════════════════════════════════════════════════════════");
    println!("  Wallet:          {:?}", signer_address);
    println!("  Min Spread:      {} bps", min_spread_bps);
    println!("  Amount per arb:  {} WMON", amount);
    println!("  Slippage:        {} bps", slippage);
    println!("  Max executions:  {}", if max_executions == 0 { "unlimited".to_string() } else { max_executions.to_string() });
    println!("  Cooldown:        {} seconds", cooldown_secs);
    println!("  Dry run:         {}", dry_run);
    println!("  Stats file:      {}", stats_file);
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    // Show initial balances
    let initial_balances = get_balances(&provider, signer_address).await?;
    print_balances(&initial_balances);

    let mut execution_count = 0u32;
    let mut last_execution = std::time::Instant::now() - std::time::Duration::from_secs(cooldown_secs);
    let mut poll_interval = tokio::time::interval(Duration::from_millis(POLL_INTERVAL_MS));

    loop {
        poll_interval.tick().await;

        // Check if we've hit max executions
        if max_executions > 0 && execution_count >= max_executions {
            println!("\n  Reached max executions ({}). Stopping.", max_executions);
            break;
        }

        // Fetch current prices
        let prices = match get_current_prices(&provider).await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("  Price fetch error: {}", e);
                continue;
            }
        };

        // Calculate spreads
        let spreads = calculate_spreads(&prices);

        // Find best opportunity (first one is best due to sorting)
        let best_spread = spreads.first();
        
        if let Some(spread) = best_spread {
            // Display current best opportunity
            let now = Local::now().format("%H:%M:%S");
            print!("\r[{}] Best: {} → {} | Net: {:+.2}% ({:+} bps)    ", 
                now,
                spread.buy_pool, 
                spread.sell_pool,
                spread.net_spread_pct,
                (spread.net_spread_pct * 100.0) as i32
            );
            std::io::Write::flush(&mut std::io::stdout()).ok();

            let net_spread_bps = (spread.net_spread_pct * 100.0) as i32;

            // Check if spread meets threshold and cooldown has passed
            let cooldown_elapsed = last_execution.elapsed().as_secs() >= cooldown_secs;
            
            if net_spread_bps >= min_spread_bps && cooldown_elapsed {
                println!();  // New line after the \r print
                println!("\n  🎯 OPPORTUNITY DETECTED! Net spread: {} bps (threshold: {} bps)", 
                    net_spread_bps, min_spread_bps);

                // Get routers for the opportunity
                let sell_router = match get_router_by_name(&spread.sell_pool) {
                    Some(r) => r,
                    None => {
                        eprintln!("  Router not found for {}", spread.sell_pool);
                        continue;
                    }
                };
                let buy_router = match get_router_by_name(&spread.buy_pool) {
                    Some(r) => r,
                    None => {
                        eprintln!("  Router not found for {}", spread.buy_pool);
                        continue;
                    }
                };

                // Get current balances (pre-execution)
                let balances_before = get_balances(&provider, signer_address).await?;

                // Check if we have enough WMON
                if balances_before.wmon_human < amount {
                    println!("  ⚠ Insufficient WMON. Have: {:.6}, Need: {:.6}", 
                        balances_before.wmon_human, amount);
                    continue;
                }

                // Calculate expected amounts
                let expected_usdc = amount * spread.sell_price;
                let expected_wmon_back = expected_usdc / spread.buy_price;

                // Create pre-execution snapshot
                let pre_snapshot = PreExecutionSnapshot {
                    timestamp: Local::now().to_rfc3339(),
                    wmon_balance: balances_before.wmon_human,
                    usdc_balance: balances_before.usdc_human,
                    mon_balance: balances_before.mon_human,
                    sell_dex: spread.sell_pool.clone(),
                    sell_price: spread.sell_price,
                    buy_dex: spread.buy_pool.clone(),
                    buy_price: spread.buy_price,
                    gross_spread_bps: (spread.gross_spread_pct * 100.0) as i32,
                    net_spread_bps,
                    amount_wmon: amount,
                    expected_usdc,
                    expected_wmon_back,
                    slippage_bps: slippage,
                };

                print_pre_execution(&pre_snapshot);

                if dry_run {
                    println!("\n  [DRY RUN] Would execute arb but dry_run=true. Skipping.");
                    
                    // Log dry run
                    let record = ArbExecutionRecord {
                        id: stats_logger.next_id(),
                        pre: pre_snapshot,
                        post: None,
                        success: false,
                        error: Some("Dry run - execution skipped".to_string()),
                    };
                    stats_logger.log_execution(&record);
                    
                    last_execution = std::time::Instant::now();
                    execution_count += 1;
                    continue;
                }

                // Fetch gas price
                let gas_price = provider.get_gas_price().await.unwrap_or(100_000_000_000);

                // Execute fast arb
                println!("\n  🚀 EXECUTING ARB...");
                let exec_start = std::time::Instant::now();
                
                let arb_result = execute_fast_arb(
                    &provider_with_signer,
                    signer_address,
                    &sell_router,
                    &buy_router,
                    amount,
                    spread.sell_price,
                    spread.buy_price,
                    slippage,
                    gas_price,
                ).await;

                let exec_time = exec_start.elapsed().as_millis();

                // Get post-execution balances
                let balances_after = get_balances(&provider, signer_address).await?;

                // Create post-execution snapshot
                let post_snapshot = match &arb_result {
                    Ok(result) => {
                        let wmon_delta = balances_after.wmon_human - balances_before.wmon_human;
                        let usdc_delta = balances_after.usdc_human - balances_before.usdc_human;
                        let mon_delta = balances_after.mon_human - balances_before.mon_human;
                        let net_profit_bps = if amount > 0.0 {
                            (wmon_delta / amount * 10000.0) as i32
                        } else {
                            0
                        };

                        PostExecutionSnapshot {
                            timestamp: Local::now().to_rfc3339(),
                            wmon_balance: balances_after.wmon_human,
                            usdc_balance: balances_after.usdc_human,
                            mon_balance: balances_after.mon_human,
                            swap1_success: result.swap1_success,
                            swap1_tx_hash: result.swap1_tx_hash.clone(),
                            swap1_gas_used: result.swap1_gas_used,
                            swap1_gas_estimated: result.swap1_gas_estimated,
                            swap2_success: result.swap2_success,
                            swap2_tx_hash: result.swap2_tx_hash.clone(),
                            swap2_gas_used: result.swap2_gas_used,
                            swap2_gas_estimated: result.swap2_gas_estimated,
                            actual_usdc_received: result.usdc_intermediate,
                            actual_wmon_back: result.wmon_out,
                            wmon_delta,
                            usdc_delta,
                            mon_delta,
                            total_gas_cost_mon: result.total_gas_cost_mon,
                            net_profit_wmon: wmon_delta,
                            net_profit_bps,
                            total_execution_ms: exec_time,
                        }
                    }
                    Err(e) => {
                        // Failed execution - still record balances
                        PostExecutionSnapshot {
                            timestamp: Local::now().to_rfc3339(),
                            wmon_balance: balances_after.wmon_human,
                            usdc_balance: balances_after.usdc_human,
                            mon_balance: balances_after.mon_human,
                            swap1_success: false,
                            swap1_tx_hash: String::new(),
                            swap1_gas_used: 0,
                            swap1_gas_estimated: 0,
                            swap2_success: false,
                            swap2_tx_hash: String::new(),
                            swap2_gas_used: 0,
                            swap2_gas_estimated: 0,
                            actual_usdc_received: 0.0,
                            actual_wmon_back: 0.0,
                            wmon_delta: balances_after.wmon_human - balances_before.wmon_human,
                            usdc_delta: balances_after.usdc_human - balances_before.usdc_human,
                            mon_delta: balances_after.mon_human - balances_before.mon_human,
                            total_gas_cost_mon: 0.0,
                            net_profit_wmon: 0.0,
                            net_profit_bps: 0,
                            total_execution_ms: exec_time,
                        }
                    }
                };

                print_post_execution(&pre_snapshot, &post_snapshot);

                // Log execution record
                let record = ArbExecutionRecord {
                    id: stats_logger.next_id(),
                    pre: pre_snapshot,
                    post: Some(post_snapshot),
                    success: arb_result.as_ref().map(|r| r.success).unwrap_or(false),
                    error: arb_result.as_ref().err().map(|e| e.to_string()),
                };
                stats_logger.log_execution(&record);

                // Print result summary
                if let Ok(result) = &arb_result {
                    print_fast_arb_result(result, &spread.sell_pool, &spread.buy_pool);
                } else if let Err(e) = &arb_result {
                    println!("\n  ❌ ARB EXECUTION FAILED: {}", e);
                }

                last_execution = std::time::Instant::now();
                execution_count += 1;

                println!("\n  Executions: {} / {}", 
                    execution_count, 
                    if max_executions == 0 { "∞".to_string() } else { max_executions.to_string() }
                );
                println!("  Cooldown: {} seconds before next execution...\n", cooldown_secs);
            }
        }
    }

    // Final summary
    println!("\n═══════════════════════════════════════════════════════════════");
    println!("  AUTO-ARB SESSION COMPLETE");
    println!("═══════════════════════════════════════════════════════════════");
    println!("  Total executions: {}", execution_count);
    println!("  Stats saved to:   {}", stats_file);
    
    let final_balances = get_balances(&provider, signer_address).await?;
    println!("\n  Final Balances:");
    println!("    MON:  {:>18.6} (Δ {:>+.6})", final_balances.mon_human, 
        final_balances.mon_human - initial_balances.mon_human);
    println!("    WMON: {:>18.6} (Δ {:>+.6})", final_balances.wmon_human,
        final_balances.wmon_human - initial_balances.wmon_human);
    println!("    USDC: {:>18.6} (Δ {:>+.6})", final_balances.usdc_human,
        final_balances.usdc_human - initial_balances.usdc_human);
    println!("═══════════════════════════════════════════════════════════════");

    Ok(())
}
```

---

### 3. Modify `src/display.rs`

Make `calculate_spreads` public (it should already be `pub`, verify):

```rust
/// Calculate all spread opportunities between pools
pub fn calculate_spreads(prices: &[PoolPrice]) -> Vec<SpreadOpportunity> {
```

Also make `SpreadOpportunity` public with public fields (verify it's accessible from main.rs).

---

## Testing Phase 1

### Test Commands

1. **Dry run test** (detects but doesn't execute):
```bash
cargo run -- auto-arb --min-spread-bps -200 --amount 0.1 --dry-run true --max-executions 3
```

2. **Single execution test** (will execute once at any spread >= -100 bps):
```bash
cargo run -- auto-arb --min-spread-bps -100 --amount 0.1 --max-executions 1 --slippage 200
```

3. **Multiple executions with cooldown**:
```bash
cargo run -- auto-arb --min-spread-bps -50 --amount 0.1 --max-executions 5 --cooldown-secs 30
```

### Expected Output

The stats file (`arb_stats_YYYYMMDD_HHMMSS.jsonl`) will contain JSON lines like:
```json
{"id":1,"pre":{"timestamp":"2025-06-12T...","wmon_balance":10.5,...},"post":{...},"success":true,"error":null}
```

---

## Phase 2: Production Mode

### Additional Features for Phase 2 (implement after Phase 1 is tested):

1. **Add to `Commands` enum**:
```rust
    /// Production arbitrage bot with safety checks
    ProdArb {
        /// Minimum net spread in bps (must be positive for production)
        #[arg(long, default_value = "20")]
        min_spread_bps: i32,
        
        /// Amount of WMON per arb
        #[arg(long, default_value = "1.0")]
        amount: f64,
        
        /// Slippage tolerance in bps
        #[arg(long, default_value = "100")]
        slippage: u32,
        
        /// Max daily loss in WMON (stops bot if exceeded)
        #[arg(long, default_value = "0.5")]
        max_daily_loss: f64,
        
        /// Max consecutive failures before pause
        #[arg(long, default_value = "3")]
        max_failures: u32,
    },
```

2. **Production safety checks** (in `run_prod_arb` function):
   - Enforce `min_spread_bps > 0`
   - Track cumulative P&L, stop if loss exceeds threshold
   - Track consecutive failures, pause if too many
   - Verify router approvals before starting
   - Add graceful shutdown on SIGINT (Ctrl+C)

3. **Metrics to track**:
   - Total P&L (WMON)
   - Total gas spent (MON)
   - Win rate (successful profitable arbs / total attempts)
   - Average execution time
   - Average slippage vs expected

---

## File Summary

| File | Action | Purpose |
|------|--------|---------|
| `src/stats.rs` | CREATE | Stats tracking and logging |
| `src/main.rs` | MODIFY | Add `auto-arb` command |
| `src/display.rs` | VERIFY | Ensure `calculate_spreads` is public |
| `Cargo.toml` | VERIFY | Ensure `serde` and `serde_json` dependencies |

---

## Critical Notes

1. **DO NOT** modify `execute_fast_arb` - it works correctly
2. **DO NOT** modify existing CLI commands - they must continue working
3. **DO NOT** change price fetching logic - it's optimized
4. **DO NOT** change nonce management - it's thread-safe

5. **The `calculate_spreads` function** returns spreads sorted by `net_spread_pct` descending (best first)
6. **Spread convention**: `buy_pool` is where you BUY WMON (lower price), `sell_pool` is where you SELL WMON (higher price)
7. **The fast_arb expects**: `sell_router` (higher price, sell WMON first) and `buy_router` (lower price, buy WMON back)

---

## Verification Checklist

After implementation:

- [ ] `cargo run -- monitor` still works
- [ ] `cargo run -- balance` still works
- [ ] `cargo run -- fast-arb --sell-dex uniswap --buy-dex pancakeswap1 --amount 0.1` still works
- [ ] `cargo run -- auto-arb --dry-run true --max-executions 1` runs without errors
- [ ] Stats file is created with valid JSON
- [ ] Pre/post execution snapshots display correctly