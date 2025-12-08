//! Enhanced spread display with real-time tracking and visualization
//!
//! Features:
//! - Non-clearing display (uses cursor positioning)
//! - Color-coded spread levels (green/yellow/red)
//! - Trend arrows showing direction
//! - Mini sparkline for recent history
//! - Actionable alerts when threshold exceeded

use std::collections::{HashMap, VecDeque};
use std::io::{stdout, Write};
use std::time::Instant;

use chrono::Local;

use crate::display::SpreadOpportunity;
use crate::pools::PoolPrice;

/// Spread alert levels for color coding
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpreadLevel {
    Dead,     // < 0 bps (negative)
    Noise,    // 0-5 bps
    Watching, // 5-10 bps
    Ready,    // 10-15 bps
    Hot,      // 15-25 bps
    Critical, // > 25 bps
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
            Self::Dead => "\x1b[90m",        // Gray
            Self::Noise => "\x1b[37m",       // White
            Self::Watching => "\x1b[33m",    // Yellow
            Self::Ready => "\x1b[32m",       // Green
            Self::Hot => "\x1b[1;32m",       // Bold Green
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
    Rising,  // Spread increasing
    Stable,  // Within +/-1 bps
    Falling, // Spread decreasing
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
            Self::Rising => "\x1b[32m",  // Green (good - opportunity growing)
            Self::Stable => "\x1b[33m",  // Yellow
            Self::Falling => "\x1b[31m", // Red (bad - opportunity shrinking)
        }
    }
}

/// History buffer for a specific pool pair
#[derive(Debug, Clone)]
pub struct PairHistory {
    pub pair_key: String,            // "BuyPool→SellPool"
    pub history: VecDeque<i32>,      // Last N spread values in bps
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

        values
            .iter()
            .map(|&v| {
                let idx = ((v - min) * 7 / range).clamp(0, 7) as usize;
                CHARS[idx]
            })
            .collect()
    }
}

/// Main display state manager
pub struct SpreadDisplay {
    /// History per pool pair
    pub pair_histories: HashMap<String, PairHistory>,
    /// Display threshold in bps
    pub min_display_bps: i32,
    /// History capacity per pair
    pub history_size: usize,
    /// Last display update time
    pub last_update: Instant,
    /// Block number at last update
    pub last_block: u64,
    /// Alert sound enabled
    pub alert_sound: bool,
}

impl SpreadDisplay {
    pub fn new(min_display_bps: i32, history_size: usize) -> Self {
        Self {
            pair_histories: HashMap::new(),
            min_display_bps,
            history_size,
            last_update: Instant::now(),
            last_block: 0,
            alert_sound: true,
        }
    }

    /// Update with new spread data
    pub fn update(&mut self, spreads: &[SpreadOpportunity]) {
        for spread in spreads {
            let key = format!("{}→{}", spread.buy_pool, spread.sell_pool);
            let net_bps = (spread.net_spread_pct * 100.0) as i32;

            let history = self
                .pair_histories
                .entry(key.clone())
                .or_insert_with(|| PairHistory::new(key, self.history_size));

            history.push(net_bps);
        }
        self.last_update = Instant::now();
    }

    /// Render the display (non-clearing)
    pub fn render(&self, block_number: Option<u64>) -> String {
        let mut output = String::new();
        let now = Local::now().format("%H:%M:%S%.3f");

        // Header line with block info
        output.push_str(&format!(
            "\x1b[2K\r\x1b[1;36m[{}] Block: {} | Pairs: {} | Filter: >{}bps\x1b[0m\n",
            now,
            block_number
                .map(|b| b.to_string())
                .unwrap_or_else(|| "?".to_string()),
            self.pair_histories.len(),
            self.min_display_bps
        ));

        // Collect and sort by spread
        let mut active_pairs: Vec<_> = self
            .pair_histories
            .iter()
            .filter_map(|(key, hist)| {
                let last = *hist.history.back()?;
                if last >= self.min_display_bps {
                    Some((key, hist, last))
                } else {
                    None
                }
            })
            .collect();

        active_pairs.sort_by(|a, b| b.2.cmp(&a.2)); // Sort descending by spread

        if active_pairs.is_empty() {
            output.push_str(&format!(
                "\x1b[2K  \x1b[90mNo spreads above {}bps threshold\x1b[0m\n",
                self.min_display_bps
            ));
        } else {
            // Column headers
            output.push_str("\x1b[2K  \x1b[1m");
            output.push_str(&format!(
                "{:<25} {:>8} {:>6} {:>3} {:>12} {:>8}\x1b[0m\n",
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
                    color,
                    key,
                    last_bps,
                    color,
                    level.label(),
                    trend_color,
                    trend.arrow(),
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
        let best = self
            .pair_histories
            .iter()
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

/// Render full dashboard with prices and spreads
pub fn render_full_dashboard(
    display: &SpreadDisplay,
    prices: &[PoolPrice],
    block: Option<u64>,
) -> String {
    let mut out = String::new();
    let now = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");

    // Header
    out.push_str("\x1b[2K\x1b[1;36m");
    out.push_str(
        "╔══════════════════════════════════════════════════════════════════════════╗\n",
    );
    out.push_str(
        "\x1b[2K║                    MONAD MEV SPREAD DASHBOARD                           ║\n",
    );
    out.push_str(
        "\x1b[2K╠══════════════════════════════════════════════════════════════════════════╣\n",
    );
    out.push_str(&format!(
        "\x1b[2K║  {} │ Block: {:>12} │ Latency: {:>4}ms                ║\n",
        now,
        block.map(|b| b.to_string()).unwrap_or_else(|| "?".into()),
        display.last_update.elapsed().as_millis()
    ));
    out.push_str(
        "\x1b[2K╠══════════════════════════════════════════════════════════════════════════╣\x1b[0m\n",
    );

    // Prices section
    out.push_str("\x1b[2K\x1b[1m  CURRENT PRICES (USDC/WMON)\x1b[0m\n");
    out.push_str(
        "\x1b[2K  ─────────────────────────────────────────────────────────────────────\n",
    );

    let mut sorted_prices = prices.to_vec();
    sorted_prices.sort_by(|a, b| {
        b.price
            .partial_cmp(&a.price)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let best_price = sorted_prices.first().map(|p| p.price).unwrap_or(0.0);

    for (i, price) in sorted_prices.iter().enumerate() {
        let diff_pct = if best_price > 0.0 {
            ((price.price - best_price) / best_price) * 100.0
        } else {
            0.0
        };

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

    // Spreads section
    out.push_str("\x1b[2K\n");
    out.push_str("\x1b[2K\x1b[1m  SPREAD OPPORTUNITIES\x1b[0m\n");
    out.push_str(
        "\x1b[2K  ─────────────────────────────────────────────────────────────────────\n",
    );
    out.push_str(&format!(
        "\x1b[2K  {:<26} {:>8} {:>7} {:>3} {:>12} {:>10}\n",
        "PAIR", "NET", "LEVEL", "", "TREND", "ACTION"
    ));
    out.push_str(
        "\x1b[2K  ─────────────────────────────────────────────────────────────────────\n",
    );

    // Get active pairs sorted by spread
    let mut pairs: Vec<_> = display
        .pair_histories
        .iter()
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

    // Footer
    out.push_str(
        "\x1b[2K\x1b[1;36m╠══════════════════════════════════════════════════════════════════════════╣\x1b[0m\n",
    );
    out.push_str(
        "\x1b[2K  \x1b[90mKeys: q=quit │ s=sound toggle │ +=increase threshold │ -=decrease\x1b[0m\n",
    );
    out.push_str(
        "\x1b[2K\x1b[1;36m╚══════════════════════════════════════════════════════════════════════════╝\x1b[0m\n",
    );

    out
}

/// Enter alternate screen buffer for clean display
pub fn enter_alternate_screen() {
    print!("\x1b[?1049h"); // Enter alternate screen
    print!("\x1b[?25l"); // Hide cursor
    let _ = stdout().flush();
}

/// Exit alternate screen buffer and restore terminal
pub fn exit_alternate_screen() {
    print!("\x1b[?1049l"); // Exit alternate screen
    print!("\x1b[?25h"); // Show cursor
    let _ = stdout().flush();
}

/// Move cursor to home position
pub fn cursor_home() {
    print!("\x1b[H");
    let _ = stdout().flush();
}

/// Check if running in interactive terminal
pub fn is_interactive() -> bool {
    atty::is(atty::Stream::Stdout)
}
