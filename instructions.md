# MEV Validation Console Logging Improvements

## Executive Summary

The current `mev-validate` output is noisy and fails to provide actionable insights. This document provides detailed instructions for Claude Code Opus to refactor the logging system into a clean, insightful dashboard.

**Current Output (Problematic):**
```
[BLOCK 40711430] Δt=616ms
  Spread: 1bps → -7bps (-8Δ)
  Pair: MondayTrade → PancakeSwap1
  Status: GONE - CAPTURED
[DEBUG] Block 40711431 Finalized: proposed=true, finalized=true
```

**Target Output (Clean Dashboard):**
```
╔══════════════════════════════════════════════════════════════════════════╗
║  MEV VALIDATION │ Runtime: 05:23 │ Blocks: 324 │ Filter: >10bps          ║
╠══════════════════════════════════════════════════════════════════════════╣
║  TIMING          │ OPPORTUNITIES          │ COMPETITION                  ║
║  Avg: 742ms      │ Actionable: 23/324     │ Persisted: 8 (34.8%)        ║
║  Min: 498ms      │ Max Seen: +18bps       │ Captured: 12 (52.2%)        ║
║  Window: 398ms   │ Avg Decay: -6.2bps     │ Decayed: 3 (13.0%)          ║
╠══════════════════════════════════════════════════════════════════════════╣
║  RECENT ACTIONABLE (showing only >10bps at Proposed)                     ║
║  #40711428 │ 733ms │ Uniswap→LFJ │ +15→+3bps │ ▼ DECAYED                 ║
║  #40711427 │ 790ms │ Uniswap→LFJ │ +18→ 0bps │ ▼ CAPTURED                ║
╠══════════════════════════════════════════════════════════════════════════╣
║  INSIGHT: 52% captured by competitors. Need <398ms execution or mempool  ║
╚══════════════════════════════════════════════════════════════════════════╝
```

---

## Part 1: Code Changes to `src/mev_validation.rs`

### 1.1 Remove Debug Output

**Location**: Around line 234 in `handle_block()` method

```rust
// DELETE THIS BLOCK:
eprintln!("[DEBUG] Block {} Finalized: proposed={}, finalized={}",
    block_num,
    lifecycle.proposed.is_some(),
    lifecycle.finalized.is_some());
```

### 1.2 Add New Types for Classification

Add these types after the existing `CommitState` enum:

```rust
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

    pub fn is_actionable(&self) -> bool {
        matches!(self, Self::Marginal | Self::Actionable | Self::Critical)
    }

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
```

### 1.3 Add Running Statistics Struct

Add this struct to track real-time aggregates:

```rust
use std::collections::VecDeque;

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
                .map(|(buy, sell)| format!("{}→{}", 
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

fn truncate_name(name: &str, max_len: usize) -> &str {
    if name.len() <= max_len { name } else { &name[..max_len] }
}
```

### 1.4 Add Dashboard Renderer

Add this function to render the dashboard:

```rust
use std::io::{stdout, Write};

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
                "║  {:>8} │ {:>4}ms │ {:<17} │ {:>+3}→{:>+3}bps {} │ {}{:<16}\x1b[0m ║\n",
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
    out.push_str(&format!("║  💡 {:<71}║\n", insight));

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
            "HIGH COMPETITION: {:.0}% captured. Need <{}ms exec or mempool monitoring.",
            capture, window / 2
        )
    } else if capture > 40.0 {
        format!(
            "MODERATE COMPETITION: {:.0}% captured. Execute within {}ms to compete.",
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
```

### 1.5 Update the Validator Struct

Add `running_stats` field to `MevValidator`:

```rust
pub struct MevValidator {
    ws_url: String,
    rpc_url: String,
    price_calls: Vec<PriceCall>,
    start_time: Instant,
    block_lifecycles: HashMap<u64, BlockLifecycle>,
    completed_blocks: Vec<BlockLifecycle>,
    log_file: String,
    min_spread_bps: i32,
    running_stats: RunningStats,  // ADD THIS
}

impl MevValidator {
    pub fn new(rpc_url: &str, ws_url: &str, min_spread_bps: i32) -> Self {
        // ... existing code ...
        
        Self {
            // ... existing fields ...
            running_stats: RunningStats::new(),  // ADD THIS
        }
    }
}
```

### 1.6 Update `handle_block()` Method

Replace the current per-block logging with dashboard updates:

```rust
async fn handle_block(&mut self, header: MonadBlockHeader) -> Result<()> {
    let block_num = header.block_number();
    let state = header.commit_state.clone();

    // ... existing snapshot code ...

    // When lifecycle completes, update stats and render dashboard
    if let Some(completed) = completed_lifecycle {
        // Update running statistics
        self.running_stats.record(&completed);
        
        // Log to file (silent)
        self.log_lifecycle(&completed);
        
        // Render dashboard (replaces all println! calls)
        print!("{}", render_dashboard(
            &self.running_stats,
            self.start_time,
            self.min_spread_bps
        ));
        stdout().flush().ok();
    }

    // ... cleanup code ...

    Ok(())
}
```

### 1.7 Update Main Loop in `run_mev_validation()`

```rust
pub async fn run_mev_validation(
    rpc_url: &str,
    ws_url: &str,
    duration_secs: u64,
    min_spread_bps: i32
) -> Result<()> {
    // ... existing setup code ...

    // Enter alternate screen for clean dashboard
    print!("\x1b[?1049h"); // Alternate screen buffer
    print!("\x1b[?25l");   // Hide cursor
    stdout().flush().ok();

    // ... existing WebSocket loop ...

    // On exit: restore terminal and print final stats
    print!("\x1b[?1049l"); // Exit alternate screen
    print!("\x1b[?25h");   // Show cursor
    stdout().flush().ok();

    // Print final comprehensive report
    validator.print_final_report();

    Ok(())
}
```

### 1.8 Add Final Report Method

```rust
impl MevValidator {
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
        println!("║    Data File:              {}                                    ║", &self.log_file[..self.log_file.len().min(40)]);
        
        println!("╠══════════════════════════════════════════════════════════════════════════════╣");
        println!("║  TIMING ANALYSIS                                                             ║");
        println!("║  ──────────────────────────────────────────────────────────────────────────  ║");
        println!("║    Proposed → Finalized Window:                                              ║");
        println!("║      Average:     {:>6.0}ms                                                   ║", stats.avg_timing_ms());
        println!("║      Minimum:     {:>6}ms  ← FASTEST POSSIBLE                               ║", stats.min_timing_ms());
        println!("║      Maximum:     {:>6}ms                                                   ║", stats.timing_max);
        println!("║                                                                              ║");
        println!("║    ⚡ EXECUTION TARGET: Complete arb within {:>4}ms                          ║", stats.execution_window_ms());
        
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
            println!("║      ✅ PERSISTED (still >=10bps): {:>4} ({:>5.1}%)  ← CAPTURABLE             ║",
                stats.persisted_count + stats.grew_count, stats.persistence_rate());
            println!("║      ⚠️  DECAYED (5-9bps):          {:>4} ({:>5.1}%)  ← MARGINAL               ║",
                stats.decayed_count, stats.decay_rate());
            println!("║      ❌ CAPTURED (<5bps):          {:>4} ({:>5.1}%)  ← MISSED                  ║",
                stats.captured_count, stats.capture_rate());
        }
        
        println!("╠══════════════════════════════════════════════════════════════════════════════╣");
        println!("║  COMPETITIVE ASSESSMENT                                                      ║");
        println!("║  ──────────────────────────────────────────────────────────────────────────  ║");
        
        let capture = stats.capture_rate();
        if capture > 70.0 {
            println!("║    ⚠️  HIGH COMPETITION DETECTED                                             ║");
            println!("║                                                                              ║");
            println!("║    {:.0}% of actionable spreads were captured by faster competitors.         ║", capture);
            println!("║                                                                              ║");
            println!("║    RECOMMENDATIONS:                                                          ║");
            println!("║      1. Reduce execution latency below {}ms                                ║", stats.execution_window_ms() / 2);
            println!("║      2. Implement mempool monitoring for earlier spread detection            ║");
            println!("║      3. Consider validator priority inclusion (contact ken_category_labs)    ║");
            println!("║      4. Focus on less competitive pairs or larger spreads (>15bps)           ║");
        } else if capture > 40.0 {
            println!("║    ⚡ MODERATE COMPETITION                                                   ║");
            println!("║                                                                              ║");
            println!("║    {:.0}% captured by others. You can compete with optimizations.            ║", capture);
            println!("║                                                                              ║");
            println!("║    RECOMMENDATIONS:                                                          ║");
            println!("║      1. Target execution under {}ms                                        ║", stats.execution_window_ms());
            println!("║      2. Use atomic arb contract (single TX) for speed                        ║");
            println!("║      3. Pre-build transactions during price monitoring                       ║");
        } else if stats.actionable_count > 0 {
            println!("║    ✅ LOW COMPETITION - GOOD OPPORTUNITY                                     ║");
            println!("║                                                                              ║");
            println!("║    {:.0}% of spreads persist long enough for capture!                        ║", stats.persistence_rate());
            println!("║                                                                              ║");
            println!("║    RECOMMENDATIONS:                                                          ║");
            println!("║      1. Execute within {}ms window                                         ║", stats.execution_window_ms());
            println!("║      2. Set trigger threshold at 10-12bps for safety margin                  ║");
            println!("║      3. Monitor for competition increase over time                           ║");
        } else {
            println!("║    ℹ️  INSUFFICIENT DATA                                                     ║");
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
        println!("║    1. Review detailed data: cat {}                          ║", &self.log_file);
        println!("║    2. Test execution timing: cargo run -- fast-arb --sell-dex X --buy-dex Y  ║");
        println!("║    3. Deploy atomic contract for single-TX execution                         ║");
        println!("║    4. Run auto-arb with validated parameters                                 ║");
        println!("╚══════════════════════════════════════════════════════════════════════════════╝");
        println!();
    }
}
```

---

## Part 2: CLI Enhancements

### 2.1 Update `MevValidate` Command in `main.rs`

```rust
/// MEV validation - observe block timing and spread persistence (Phase 1)
MevValidate {
    /// Duration to run validation in seconds
    #[arg(long, default_value = "300")]
    duration: u64,

    /// Minimum spread in bps to consider "actionable" (default: 10)
    #[arg(long, default_value = "10")]
    min_spread: i32,

    /// Output mode: "dashboard" (default), "log", "quiet"
    #[arg(long, default_value = "dashboard")]
    output: String,
}
```

### 2.2 Update `run_mev_validate()` in `main.rs`

```rust
async fn run_mev_validate(duration: u64, min_spread_bps: i32, output_mode: &str) -> Result<()> {
    let node_config = NodeConfig::from_env();
    
    match output_mode {
        "dashboard" => {
            mev_validation::run_mev_validation_dashboard(
                &node_config.rpc_url, 
                &node_config.ws_url, 
                duration, 
                min_spread_bps
            ).await
        }
        "log" => {
            mev_validation::run_mev_validation_log(
                &node_config.rpc_url, 
                &node_config.ws_url, 
                duration, 
                min_spread_bps
            ).await
        }
        "quiet" => {
            mev_validation::run_mev_validation_quiet(
                &node_config.rpc_url, 
                &node_config.ws_url, 
                duration, 
                min_spread_bps
            ).await
        }
        _ => {
            eprintln!("Unknown output mode: {}. Using 'dashboard'", output_mode);
            mev_validation::run_mev_validation_dashboard(
                &node_config.rpc_url, 
                &node_config.ws_url, 
                duration, 
                min_spread_bps
            ).await
        }
    }
}
```

---

## Part 3: Testing

After implementing these changes, test with:

```bash
# Interactive dashboard (default)
cargo run -- mev-validate --duration 120 --min-spread 10

# Log mode (for piping to file or non-interactive environments)
cargo run -- mev-validate --duration 120 --output log 2>&1 | tee validation.log

# Quiet mode (only final report)
cargo run -- mev-validate --duration 120 --output quiet
```

---

## Summary of Changes

| File | Change |
|------|--------|
| `src/mev_validation.rs` | Remove debug prints, add `SpreadTier`, `SpreadOutcome`, `RunningStats` |
| `src/mev_validation.rs` | Add `render_dashboard()` function |
| `src/mev_validation.rs` | Add `print_final_report()` method |
| `src/mev_validation.rs` | Update `handle_block()` to use dashboard |
| `src/main.rs` | Add `--output` flag to `MevValidate` command |

The goal is to transform noisy block-by-block output into a clean, real-time dashboard that answers:
1. **Can we execute fast enough?** (timing metrics)
2. **Are there opportunities?** (actionable count, max spread)
3. **Is competition fierce?** (capture rate)
4. **What should we do?** (insight + final recommendations)