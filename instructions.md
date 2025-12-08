# Console Display Enhancement Instructions

## Objective
Improve the console display system for the Monad MEV arbitrage bot to provide real-time, actionable visibility into spread opportunities when spreads exceed 5bps threshold.

## Current State Analysis

### Existing Display Components

1. **`src/display.rs`** - Main price monitor display
   - `display_prices()` - Clears screen and shows current prices + spreads
   - `calculate_spreads()` - Computes all pairwise spread opportunities
   - `log_arb_opportunities()` - Logs spreads > 10bps to file
   - Problem: Screen clears every cycle, losing history

2. **`src/mev_validation.rs`** - Block lifecycle tracking
   - Shows spreads at Proposed/Finalized states
   - Uses `\r` carriage return for in-place updates
   - Problem: Only shows during mev-validate command

3. **`src/main.rs`** - CLI and monitoring loops
   - `run_monitor()` - Price polling loop
   - `run_auto_arb()` - Automated arb with spread display
   - Problem: Inconsistent display formats across commands

4. **`src/spread_tracker.rs`** - Velocity tracking
   - Ring buffer for spread history
   - `VelocityAnalysis` - Calculates spread velocity/acceleration
   - Problem: Only used in auto-arb, not visible in monitor

### Current Pain Points
- Screen clearing loses spread history
- No visual distinction between actionable vs noise spreads
- No trend/velocity visualization in monitor mode
- Hard to correlate spread spikes with block events
- No persistent "spread dashboard" view

---

## Implementation Requirements

### Phase 1: Enhanced Spread Display Module

Create new file: `src/spread_display.rs`

```rust
//! Enhanced spread display with real-time tracking and visualization
//! 
//! Features:
//! - Non-clearing display (uses cursor positioning)
//! - Color-coded spread levels (green/yellow/red)
//! - Trend arrows showing direction
//! - Mini sparkline for recent history
//! - Actionable alerts when threshold exceeded

use std::collections::VecDeque;
use std::io::{Write, stdout};
use chrono::Local;

/// Spread alert levels for color coding
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpreadLevel {
    Dead,       // < 0 bps (negative)
    Noise,      // 0-5 bps
    Watching,   // 5-10 bps
    Ready,      // 10-15 bps
    Hot,        // 15-25 bps
    Critical,   // > 25 bps
}

impl SpreadLevel {
    pub fn from_bps(spread_bps: i32) -> Self {
        match spread_bps {
            x if x < 0 => Self::Dead,
            0..=4 => Self::Noise,
            5..=9 => Self::Watching,
            10..=14 => Self::Ready,
            15..=24 => Self::Hot,
            _ => Self::Critical,
        }
    }
    
    pub fn color_code(&self) -> &'static str {
        match self {
            Self::Dead => "\x1b[90m",      // Gray
            Self::Noise => "\x1b[37m",     // White
            Self::Watching => "\x1b[33m",  // Yellow
            Self::Ready => "\x1b[32m",     // Green
            Self::Hot => "\x1b[1;32m",     // Bold Green
            Self::Critical => "\x1b[1;5;32m", // Bold Blinking Green
        }
    }
    
    pub fn label(&self) -> &'static str {
        match self {
            Self::Dead => "DEAD",
            Self::Noise => "NOISE",
            Self::Watching => "WATCH",
            Self::Ready => "READY",
            Self::Hot => "HOT!",
            Self::Critical => "GO GO GO",
        }
    }
}

/// Trend direction based on recent spread changes
#[derive(Debug, Clone, Copy)]
pub enum Trend {
    Rising,     // Spread increasing
    Stable,     // Within ±1 bps
    Falling,    // Spread decreasing
}

impl Trend {
    pub fn arrow(&self) -> &'static str {
        match self {
            Self::Rising => "↑",
            Self::Stable => "→",
            Self::Falling => "↓",
        }
    }
    
    pub fn color(&self) -> &'static str {
        match self {
            Self::Rising => "\x1b[32m",   // Green (good - opportunity growing)
            Self::Stable => "\x1b[33m",   // Yellow
            Self::Falling => "\x1b[31m",  // Red (bad - opportunity shrinking)
        }
    }
}

/// Single spread observation
#[derive(Debug, Clone)]
pub struct SpreadObs {
    pub timestamp_ms: u128,
    pub buy_pool: String,
    pub sell_pool: String,
    pub net_spread_bps: i32,
}

/// History buffer for a specific pool pair
pub struct PairHistory {
    pub pair_key: String,  // "BuyPool→SellPool"
    pub history: VecDeque<i32>,  // Last N spread values in bps
    pub capacity: usize,
}

impl PairHistory {
    pub fn new(key: String, capacity: usize) -> Self {
        Self {
            pair_key: key,
            history: VecDeque::with_capacity(capacity),
            capacity,
        }
    }
    
    pub fn push(&mut self, spread_bps: i32) {
        if self.history.len() >= self.capacity {
            self.history.pop_front();
        }
        self.history.push_back(spread_bps);
    }
    
    pub fn trend(&self) -> Trend {
        if self.history.len() < 2 {
            return Trend::Stable;
        }
        let recent: Vec<_> = self.history.iter().rev().take(3).collect();
        let avg_recent = recent.iter().map(|&&x| x).sum::<i32>() / recent.len() as i32;
        let oldest = *self.history.front().unwrap_or(&0);
        
        match avg_recent - oldest {
            d if d > 1 => Trend::Rising,
            d if d < -1 => Trend::Falling,
            _ => Trend::Stable,
        }
    }
    
    /// Generate mini sparkline (last 10 values)
    pub fn sparkline(&self) -> String {
        const CHARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        
        let values: Vec<i32> = self.history.iter().rev().take(10).rev().copied().collect();
        if values.is_empty() {
            return String::from("...........");
        }
        
        let min = *values.iter().min().unwrap_or(&0);
        let max = *values.iter().max().unwrap_or(&1);
        let range = (max - min).max(1);
        
        values.iter().map(|&v| {
            let idx = ((v - min) * 7 / range).clamp(0, 7) as usize;
            CHARS[idx]
        }).collect()
    }
}

/// Main display state manager
pub struct SpreadDisplay {
    /// History per pool pair
    pub pair_histories: std::collections::HashMap<String, PairHistory>,
    /// Display threshold in bps
    pub min_display_bps: i32,
    /// History capacity per pair
    pub history_size: usize,
    /// Last display update time
    pub last_update: std::time::Instant,
    /// Block number at last update
    pub last_block: u64,
    /// Alert sound enabled
    pub alert_sound: bool,
}

impl SpreadDisplay {
    pub fn new(min_display_bps: i32, history_size: usize) -> Self {
        Self {
            pair_histories: std::collections::HashMap::new(),
            min_display_bps,
            history_size,
            last_update: std::time::Instant::now(),
            last_block: 0,
            alert_sound: true,
        }
    }
    
    /// Update with new spread data
    pub fn update(&mut self, spreads: &[crate::display::SpreadOpportunity]) {
        for spread in spreads {
            let key = format!("{}→{}", spread.buy_pool, spread.sell_pool);
            let net_bps = (spread.net_spread_pct * 100.0) as i32;
            
            let history = self.pair_histories
                .entry(key.clone())
                .or_insert_with(|| PairHistory::new(key, self.history_size));
            
            history.push(net_bps);
        }
        self.last_update = std::time::Instant::now();
    }
    
    /// Render the display (non-clearing)
    pub fn render(&self, block_number: Option<u64>) -> String {
        let mut output = String::new();
        let now = Local::now().format("%H:%M:%S%.3f");
        
        // Header line with block info
        output.push_str(&format!(
            "\x1b[2K\r\x1b[1;36m[{}] Block: {} | Pairs: {} | Filter: >{}bps\x1b[0m\n",
            now,
            block_number.map(|b| b.to_string()).unwrap_or_else(|| "?".to_string()),
            self.pair_histories.len(),
            self.min_display_bps
        ));
        
        // Collect and sort by spread
        let mut active_pairs: Vec<_> = self.pair_histories.iter()
            .filter_map(|(key, hist)| {
                let last = *hist.history.back()?;
                if last >= self.min_display_bps {
                    Some((key, hist, last))
                } else {
                    None
                }
            })
            .collect();
        
        active_pairs.sort_by(|a, b| b.2.cmp(&a.2));  // Sort descending by spread
        
        if active_pairs.is_empty() {
            output.push_str(&format!(
                "\x1b[2K  \x1b[90mNo spreads above {}bps threshold\x1b[0m\n",
                self.min_display_bps
            ));
        } else {
            // Column headers
            output.push_str("\x1b[2K  \x1b[1m");
            output.push_str(&format!("{:<25} {:>8} {:>6} {:>3} {:>12} {:>8}\x1b[0m\n",
                "PAIR", "NET BPS", "LEVEL", "→", "SPARKLINE", "AGE"
            ));
            output.push_str(&format!("\x1b[2K  {}\n", "─".repeat(70)));
            
            for (key, hist, last_bps) in active_pairs.iter().take(10) {
                let level = SpreadLevel::from_bps(*last_bps);
                let trend = hist.trend();
                let sparkline = hist.sparkline();
                let color = level.color_code();
                let trend_color = trend.color();
                
                output.push_str(&format!(
                    "\x1b[2K  {}{:<25}\x1b[0m {:>+8} {}{:>6}\x1b[0m {}{:>3}\x1b[0m {:>12} {:>8}\n",
                    color, key,
                    last_bps,
                    color, level.label(),
                    trend_color, trend.arrow(),
                    sparkline,
                    format!("{}ms", self.last_update.elapsed().as_millis())
                ));
            }
        }
        
        // Footer with legend
        output.push_str(&format!("\x1b[2K  {}\n", "─".repeat(70)));
        output.push_str("\x1b[2K  \x1b[90mLevels: ");
        output.push_str("\x1b[37mNOISE(<5)\x1b[90m | ");
        output.push_str("\x1b[33mWATCH(5-9)\x1b[90m | ");
        output.push_str("\x1b[32mREADY(10-14)\x1b[90m | ");
        output.push_str("\x1b[1;32mHOT(15-24)\x1b[90m | ");
        output.push_str("\x1b[1;5;32mGO(25+)\x1b[0m\n");
        
        output
    }
    
    /// Render single-line status (for non-interactive mode)
    pub fn render_oneline(&self) -> String {
        let best = self.pair_histories.iter()
            .filter_map(|(key, hist)| {
                let last = *hist.history.back()?;
                Some((key, last))
            })
            .max_by_key(|(_, spread)| *spread);
        
        match best {
            Some((key, spread)) => {
                let level = SpreadLevel::from_bps(spread);
                format!(
                    "{}[{:>+4}bps] {:<25} {}\x1b[0m",
                    level.color_code(),
                    spread,
                    key,
                    level.label()
                )
            }
            None => String::from("\x1b[90mNo active spreads\x1b[0m"),
        }
    }
}
```

### Phase 2: Update Monitor Command

Modify `src/main.rs` `run_monitor()` function:

```rust
async fn run_monitor() -> Result<()> {
    // ... existing setup ...
    
    // NEW: Initialize spread display
    let mut spread_display = spread_display::SpreadDisplay::new(5, 20);  // 5bps threshold, 20 history
    
    // NEW: Terminal mode selection
    let interactive = std::env::var("TERM").is_ok() && atty::is(atty::Stream::Stdout);
    
    if interactive {
        // Enter alternate screen buffer for clean display
        print!("\x1b[?1049h");  // Enter alternate screen
        print!("\x1b[?25l");    // Hide cursor
    }
    
    loop {
        poll_interval.tick().await;
        
        match fetch_prices_batched(&provider, price_calls.clone()).await {
            Ok((prices, elapsed_ms)) => {
                let spreads = calculate_spreads(&prices);
                spread_display.update(&spreads);
                
                if interactive {
                    // Move cursor to top and render
                    print!("\x1b[H");  // Move to home
                    print!("{}", spread_display.render(None));
                    stdout().flush().ok();
                } else {
                    // Non-interactive: single line update
                    print!("\r{}", spread_display.render_oneline());
                    stdout().flush().ok();
                }
            }
            Err(e) => {
                eprintln!("\x1b[31mError: {}\x1b[0m", e);
            }
        }
    }
    
    // Cleanup on exit (Ctrl+C handler needed)
    if interactive {
        print!("\x1b[?1049l");  // Exit alternate screen
        print!("\x1b[?25h");    // Show cursor
    }
    
    Ok(())
}
```

### Phase 3: Create Live Dashboard View

Add new CLI command `dashboard`:

```rust
/// Live spread dashboard with detailed visualization
Dashboard {
    /// Minimum spread to display (bps)
    #[arg(long, default_value = "5")]
    min_spread: i32,
    
    /// History depth per pair
    #[arg(long, default_value = "30")]
    history: usize,
    
    /// Refresh rate in milliseconds
    #[arg(long, default_value = "100")]
    refresh_ms: u64,
    
    /// Enable sound alerts for HOT+ spreads
    #[arg(long, default_value = "false")]
    sound: bool,
    
    /// WebSocket mode (uses monadNewHeads for block-aligned updates)
    #[arg(long, default_value = "false")]
    ws: bool,
}
```

Implement `run_dashboard()`:

```rust
async fn run_dashboard(min_spread: i32, history: usize, refresh_ms: u64, sound: bool, ws: bool) -> Result<()> {
    let node_config = NodeConfig::from_env();
    
    // Setup display
    let mut display = spread_display::SpreadDisplay::new(min_spread, history);
    display.alert_sound = sound;
    
    // Enter alternate screen
    print!("\x1b[?1049h\x1b[?25l\x1b[H");
    
    // Install Ctrl+C handler to restore terminal
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;
    
    if ws {
        // WebSocket mode: update on each block
        run_dashboard_ws(&mut display, &node_config, running).await?;
    } else {
        // Polling mode
        run_dashboard_polling(&mut display, &node_config, refresh_ms, running).await?;
    }
    
    // Restore terminal
    print!("\x1b[?1049l\x1b[?25h");
    
    Ok(())
}

async fn run_dashboard_polling(
    display: &mut SpreadDisplay,
    config: &NodeConfig,
    refresh_ms: u64,
    running: Arc<AtomicBool>,
) -> Result<()> {
    let url: reqwest::Url = config.rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url);
    
    let price_calls = build_price_calls();  // Extract from main.rs
    let mut interval = tokio::time::interval(Duration::from_millis(refresh_ms));
    
    while running.load(Ordering::SeqCst) {
        interval.tick().await;
        
        let block_num = provider.get_block_number().await.ok();
        
        match fetch_prices_batched(&provider, price_calls.clone()).await {
            Ok((prices, _)) => {
                let spreads = calculate_spreads(&prices);
                display.update(&spreads);
                
                // Render dashboard
                print!("\x1b[H");  // Home
                print!("{}", render_full_dashboard(display, &prices, block_num));
                stdout().flush().ok();
                
                // Sound alert for HOT+ spreads
                if display.alert_sound {
                    if let Some(best) = spreads.first() {
                        let bps = (best.net_spread_pct * 100.0) as i32;
                        if bps >= 15 {
                            print!("\x07");  // Terminal bell
                        }
                    }
                }
            }
            Err(_) => {}
        }
    }
    
    Ok(())
}

fn render_full_dashboard(display: &SpreadDisplay, prices: &[PoolPrice], block: Option<u64>) -> String {
    let mut out = String::new();
    let now = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    
    // ═══════════════ HEADER ═══════════════
    out.push_str("\x1b[2K\x1b[1;36m");
    out.push_str("╔══════════════════════════════════════════════════════════════════════════╗\n");
    out.push_str("\x1b[2K║                    MONAD MEV SPREAD DASHBOARD                           ║\n");
    out.push_str("\x1b[2K╠══════════════════════════════════════════════════════════════════════════╣\n");
    out.push_str(&format!("\x1b[2K║  {} │ Block: {:>12} │ Latency: {:>4}ms                ║\n",
        now,
        block.map(|b| b.to_string()).unwrap_or("?".into()),
        display.last_update.elapsed().as_millis()
    ));
    out.push_str("\x1b[2K╠══════════════════════════════════════════════════════════════════════════╣\x1b[0m\n");
    
    // ═══════════════ PRICES SECTION ═══════════════
    out.push_str("\x1b[2K\x1b[1m  CURRENT PRICES (USDC/WMON)\x1b[0m\n");
    out.push_str("\x1b[2K  ─────────────────────────────────────────────────────────────────────\n");
    
    let mut sorted_prices = prices.to_vec();
    sorted_prices.sort_by(|a, b| b.price.partial_cmp(&a.price).unwrap_or(std::cmp::Ordering::Equal));
    
    let best_price = sorted_prices.first().map(|p| p.price).unwrap_or(0.0);
    
    for (i, price) in sorted_prices.iter().enumerate() {
        let diff_pct = if best_price > 0.0 {
            ((price.price - best_price) / best_price) * 100.0
        } else { 0.0 };
        
        let marker = if i == 0 { "\x1b[1;32m★\x1b[0m" } else { " " };
        let diff_str = if i == 0 {
            "\x1b[1;32mBEST\x1b[0m".to_string()
        } else {
            format!("\x1b[33m{:+.2}%\x1b[0m", diff_pct)
        };
        
        out.push_str(&format!(
            "\x1b[2K  {} {:<14} │ {:>12.6} │ {:>10} │ Fee: {:.2}%\n",
            marker,
            price.pool_name,
            price.price,
            diff_str,
            price.fee_bps as f64 / 100.0
        ));
    }
    
    // ═══════════════ SPREADS SECTION ═══════════════
    out.push_str("\x1b[2K\n");
    out.push_str("\x1b[2K\x1b[1m  SPREAD OPPORTUNITIES\x1b[0m\n");
    out.push_str("\x1b[2K  ─────────────────────────────────────────────────────────────────────\n");
    out.push_str(&format!("\x1b[2K  {:<26} {:>8} {:>7} {:>3} {:>12} {:>10}\n",
        "PAIR", "NET", "LEVEL", "", "TREND", "ACTION"
    ));
    out.push_str("\x1b[2K  ─────────────────────────────────────────────────────────────────────\n");
    
    // Get active pairs sorted by spread
    let mut pairs: Vec<_> = display.pair_histories.iter()
        .filter_map(|(k, h)| {
            let last = *h.history.back()?;
            Some((k.clone(), h.clone(), last))
        })
        .collect();
    pairs.sort_by(|a, b| b.2.cmp(&a.2));
    
    for (key, hist, spread_bps) in pairs.iter().take(8) {
        let level = SpreadLevel::from_bps(*spread_bps);
        let trend = hist.trend();
        let sparkline = hist.sparkline();
        
        let action = match level {
            SpreadLevel::Critical | SpreadLevel::Hot => "\x1b[1;32mEXECUTE\x1b[0m",
            SpreadLevel::Ready => "\x1b[32mREADY\x1b[0m",
            SpreadLevel::Watching => "\x1b[33mMONITOR\x1b[0m",
            _ => "\x1b[90m-\x1b[0m",
        };
        
        out.push_str(&format!(
            "\x1b[2K  {}{:<26}\x1b[0m {:>+8} {}{:>7}\x1b[0m {}{:>3}\x1b[0m {:>12} {:>10}\n",
            level.color_code(),
            key,
            spread_bps,
            level.color_code(),
            level.label(),
            trend.color(),
            trend.arrow(),
            sparkline,
            action
        ));
    }
    
    // Fill remaining rows with empty lines for consistent height
    for _ in pairs.len()..8 {
        out.push_str("\x1b[2K\n");
    }
    
    // ═══════════════ FOOTER ═══════════════
    out.push_str("\x1b[2K\x1b[1;36m╠══════════════════════════════════════════════════════════════════════════╣\x1b[0m\n");
    out.push_str("\x1b[2K  \x1b[90mKeys: q=quit │ s=sound toggle │ +=increase threshold │ -=decrease\x1b[0m\n");
    out.push_str("\x1b[2K\x1b[1;36m╚══════════════════════════════════════════════════════════════════════════╝\x1b[0m\n");
    
    out
}
```

### Phase 4: Update Existing Commands

#### 4.1 Update `run_auto_arb()` display

Replace the current `print!("\r...")` pattern with:

```rust
// In the main loop of run_auto_arb:
if let Some(spread) = best_spread {
    spread_display.update(&spreads);
    
    // Enhanced single-line display with more context
    let level = SpreadLevel::from_bps((spread.net_spread_pct * 100.0) as i32);
    let trend = spread_display.pair_histories
        .get(&format!("{}→{}", spread.buy_pool, spread.sell_pool))
        .map(|h| h.trend())
        .unwrap_or(Trend::Stable);
    
    print!("\r\x1b[2K");
    print!("[{}] ", Local::now().format("%H:%M:%S"));
    print!("{}{:<20}\x1b[0m ", level.color_code(), 
        format!("{}→{}", spread.buy_pool, spread.sell_pool));
    print!("{:>+6}bps ", (spread.net_spread_pct * 100.0) as i32);
    print!("{}{}\x1b[0m ", trend.color(), trend.arrow());
    print!("{}{:>6}\x1b[0m ", level.color_code(), level.label());
    
    // Show sparkline if we have history
    if let Some(hist) = spread_display.pair_histories
        .get(&format!("{}→{}", spread.buy_pool, spread.sell_pool)) {
        print!("[{}] ", hist.sparkline());
    }
    
    // Show P&L if tracking
    print!("P&L: {:>+.4} WMON ", cumulative_pnl);
    
    stdout().flush().ok();
}
```

#### 4.2 Update `mev_validation` display

Enhance the MEV validation output to show spread evolution:

```rust
// In handle_block() for Finalized state:
if lifecycle.is_complete() {
    lifecycle.compute_analysis();
    
    let spread_proposed = lifecycle.spread_at_proposed_bps.unwrap_or(0);
    let spread_final = lifecycle.spread_at_finalized_bps.unwrap_or(0);
    let delta = lifecycle.spread_delta_bps.unwrap_or(0);
    
    // Color-coded output
    let proposed_color = SpreadLevel::from_bps(spread_proposed).color_code();
    let final_color = SpreadLevel::from_bps(spread_final).color_code();
    let delta_color = if delta > 0 { "\x1b[32m" } else if delta < 0 { "\x1b[31m" } else { "\x1b[33m" };
    
    println!();
    println!("\x1b[1m[BLOCK {}]\x1b[0m Δt={}ms", 
        lifecycle.block_number,
        lifecycle.proposed_to_finalized_ms.unwrap_or(0));
    println!("  Spread: {}{}bps\x1b[0m → {}{}bps\x1b[0m ({}{}Δ\x1b[0m)",
        proposed_color, spread_proposed,
        final_color, spread_final,
        delta_color, format!("{:+}", delta));
    
    if let Some(ref pair) = lifecycle.proposed.as_ref().and_then(|p| p.best_pair.clone()) {
        println!("  Pair: {} → {}", pair.0, pair.1);
    }
    
    let status = if lifecycle.spread_persisted.unwrap_or(false) {
        "\x1b[1;32mPERSISTED - ACTIONABLE\x1b[0m"
    } else if spread_final > 0 {
        "\x1b[33mDECAYED - PARTIAL\x1b[0m"
    } else {
        "\x1b[31mGONE - CAPTURED\x1b[0m"
    };
    println!("  Status: {}", status);
}
```

### Phase 5: Add Logging Integration

Create `src/spread_logger.rs` for persistent spread data:

```rust
//! Spread event logging for analysis

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use serde::Serialize;
use chrono::Local;

#[derive(Debug, Serialize)]
pub struct SpreadEvent {
    pub timestamp: String,
    pub block_number: Option<u64>,
    pub buy_pool: String,
    pub sell_pool: String,
    pub buy_price: f64,
    pub sell_price: f64,
    pub gross_spread_bps: i32,
    pub net_spread_bps: i32,
    pub level: String,
    pub trend: String,
    pub velocity_bps_sec: Option<f64>,
}

pub struct SpreadLogger {
    writer: BufWriter<File>,
}

impl SpreadLogger {
    pub fn new(filename: &str) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(filename)?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }
    
    pub fn log(&mut self, event: &SpreadEvent) {
        if let Ok(json) = serde_json::to_string(event) {
            let _ = writeln!(self.writer, "{}", json);
            let _ = self.writer.flush();
        }
    }
}

// Log spread events when they exceed threshold
impl SpreadDisplay {
    pub fn log_significant_spreads(&self, logger: &mut SpreadLogger, block: Option<u64>) {
        for (key, hist) in &self.pair_histories {
            if let Some(&spread_bps) = hist.history.back() {
                if spread_bps >= 10 {  // Log spreads >= 10bps
                    let parts: Vec<_> = key.split('→').collect();
                    let event = SpreadEvent {
                        timestamp: Local::now().to_rfc3339(),
                        block_number: block,
                        buy_pool: parts.get(0).unwrap_or(&"").to_string(),
                        sell_pool: parts.get(1).unwrap_or(&"").to_string(),
                        buy_price: 0.0,  // Fill from prices
                        sell_price: 0.0,
                        gross_spread_bps: spread_bps + 10,  // Approximate
                        net_spread_bps: spread_bps,
                        level: SpreadLevel::from_bps(spread_bps).label().to_string(),
                        trend: format!("{}", hist.trend().arrow()),
                        velocity_bps_sec: None,  // Fill from analysis
                    };
                    logger.log(&event);
                }
            }
        }
    }
}
```

---

## File Changes Summary

### New Files to Create
1. `src/spread_display.rs` - Enhanced display module (Phase 1)
2. `src/spread_logger.rs` - Persistent spread logging (Phase 5)

### Files to Modify
1. `src/main.rs`:
   - Add `mod spread_display; mod spread_logger;`
   - Add `Dashboard` command enum variant
   - Update `run_monitor()` to use `SpreadDisplay`
   - Add `run_dashboard()` function
   - Update `run_auto_arb()` display logic
   
2. `src/mev_validation.rs`:
   - Update `handle_block()` display formatting
   - Add color-coded spread evolution output
   
3. `src/display.rs`:
   - Export `SpreadOpportunity` struct (make pub if not already)
   - Add `impl Clone for SpreadOpportunity` if missing

4. `Cargo.toml`:
   - Add `ctrlc = "3.4"` for Ctrl+C handling
   - Add `atty = "0.2"` for terminal detection

---

## Testing Checklist

1. [ ] `cargo run -- monitor` shows enhanced spread display
2. [ ] `cargo run -- dashboard --min-spread 5` renders full dashboard
3. [ ] Spreads > 5bps show in yellow (WATCH)
4. [ ] Spreads > 10bps show in green (READY)
5. [ ] Spreads > 15bps show in bold green (HOT)
6. [ ] Trend arrows update correctly (↑/→/↓)
7. [ ] Sparklines show last 10 values
8. [ ] Terminal restores correctly on Ctrl+C
9. [ ] Non-interactive mode (piped output) shows single-line
10. [ ] `mev-validate` shows color-coded spread evolution
11. [ ] Spread events logged to JSONL when > 10bps
12. [ ] Auto-arb shows enhanced inline display

---

## Example Output

### Dashboard Mode
```
╔══════════════════════════════════════════════════════════════════════════╗
║                    MONAD MEV SPREAD DASHBOARD                           ║
╠══════════════════════════════════════════════════════════════════════════╣
║  2025-12-08 14:32:15.234 │ Block:      1234567 │ Latency:   23ms        ║
╠══════════════════════════════════════════════════════════════════════════╣
  CURRENT PRICES (USDC/WMON)
  ─────────────────────────────────────────────────────────────────────
  ★ Uniswap        │     0.037254 │       BEST │ Fee: 0.30%
    PancakeSwap1   │     0.037198 │     -0.15% │ Fee: 0.05%
    MondayTrade    │     0.037156 │     -0.26% │ Fee: 0.05%
    PancakeSwap2   │     0.037142 │     -0.30% │ Fee: 0.25%
    LFJ            │     0.037089 │     -0.44% │ Fee: 0.10%

  SPREAD OPPORTUNITIES
  ─────────────────────────────────────────────────────────────────────
  PAIR                         NET   LEVEL   →       TREND     ACTION
  ─────────────────────────────────────────────────────────────────────
  PancakeSwap1→Uniswap         +12   READY   ↑    ▂▃▄▅▆▇█▇▆▇     READY
  LFJ→Uniswap                   +9   WATCH   →    ▄▄▄▅▅▅▅▅▅▅   MONITOR
  MondayTrade→Uniswap           +7   WATCH   ↓    ▆▆▅▅▄▄▃▃▂▂   MONITOR
  PancakeSwap1→PancakeSwap2     +4   NOISE   →    ▃▃▃▃▃▃▃▃▃▃         -
╠══════════════════════════════════════════════════════════════════════════╣
  Keys: q=quit │ s=sound toggle │ +=increase threshold │ -=decrease
╚══════════════════════════════════════════════════════════════════════════╝
```

### Single-Line Mode (non-interactive)
```
[+12bps] PancakeSwap1→Uniswap    READY ↑
```

### MEV Validation Mode
```
[BLOCK 1234567] Δt=687ms
  Spread: +15bps → +12bps (-3Δ)
  Pair: PancakeSwap1 → Uniswap
  Status: PERSISTED - ACTIONABLE
```