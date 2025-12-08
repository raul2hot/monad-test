use chrono::Local;
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::pools::PoolPrice;

/// Default log file name for ARB opportunities
const ARB_LOG_FILE: &str = "arb_opportunities.log";

// Track active arb opportunities to only log new ones
static ACTIVE_ARBS: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Get the path to the ARB log file
fn get_arb_log_path() -> PathBuf {
    PathBuf::from(ARB_LOG_FILE)
}

/// Write an ARB opportunity to the log file
fn write_arb_to_file(message: &str) {
    let log_path = get_arb_log_path();

    match OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(mut file) => {
            if let Err(e) = writeln!(file, "{}", message) {
                // Only print error once, avoid spamming stderr
                eprintln!("Warning: Failed to write to arb log file: {}", e);
            }
        }
        Err(e) => {
            // Only print error once, avoid spamming stderr
            eprintln!("Warning: Failed to open arb log file {}: {}", log_path.display(), e);
        }
    }
}

/// Initialize the ARB log file and return the path for display
/// Call this at startup to inform the user where logs are written
pub fn init_arb_log() -> PathBuf {
    let log_path = get_arb_log_path();
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");

    // Write a session start marker to the log file
    let message = format!(
        "\n[{}] === ARB MONITOR SESSION STARTED ===",
        timestamp
    );
    write_arb_to_file(&message);

    log_path
}

/// Represents an arbitrage opportunity between two pools
#[derive(Debug, Clone)]
pub struct SpreadOpportunity {
    pub buy_pool: String,
    pub buy_price: f64,
    pub buy_fee_bps: u32,
    pub sell_pool: String,
    pub sell_price: f64,
    pub sell_fee_bps: u32,
    pub gross_spread_pct: f64,
    pub net_spread_pct: f64,
}

/// Calculate all spread opportunities between pools
pub fn calculate_spreads(prices: &[PoolPrice]) -> Vec<SpreadOpportunity> {
    let mut spreads = Vec::new();

    for buy in prices.iter() {
        for sell in prices.iter() {
            if buy.pool_name == sell.pool_name {
                continue;
            }

            // Show all pairs (positive and negative spreads)
            let gross_spread_pct = ((sell.price - buy.price) / buy.price) * 100.0;

            // Net spread accounts for both buy and sell fees
            let buy_fee_pct = buy.fee_bps as f64 / 100.0;
            let sell_fee_pct = sell.fee_bps as f64 / 100.0;
            let net_spread_pct = gross_spread_pct - buy_fee_pct - sell_fee_pct;

            spreads.push(SpreadOpportunity {
                buy_pool: buy.pool_name.clone(),
                buy_price: buy.price,
                buy_fee_bps: buy.fee_bps,
                sell_pool: sell.pool_name.clone(),
                sell_price: sell.price,
                sell_fee_bps: sell.fee_bps,
                gross_spread_pct,
                net_spread_pct,
            });
        }
    }

    // Sort by net spread descending (best opportunities first)
    spreads.sort_by(|a, b| b.net_spread_pct.partial_cmp(&a.net_spread_pct).unwrap());

    spreads
}

/// Log arb opportunities with net spread > 0.1% to log file (only new ones)
fn log_arb_opportunities(spreads: &[SpreadOpportunity], timestamp: &str) {
    let mut active_arbs = ACTIVE_ARBS.lock().unwrap();
    let prev_arbs = active_arbs.get_or_insert_with(HashSet::new);

    // Build current set of arb opportunities
    let mut current_arbs = HashSet::new();

    for spread in spreads.iter() {
        if spread.net_spread_pct > 0.1 {
            let key = format!("{}→{}", spread.buy_pool, spread.sell_pool);
            current_arbs.insert(key.clone());

            // Only log if this is a new opportunity
            if !prev_arbs.contains(&key) {
                let message = format!(
                    "[{}] ARB DETECTED | {} → {} | Gross: {:.2}% | Net: {:.2}% | Buy: {:.5} | Sell: {:.5}",
                    timestamp,
                    spread.buy_pool,
                    spread.sell_pool,
                    spread.gross_spread_pct,
                    spread.net_spread_pct,
                    spread.buy_price,
                    spread.sell_price
                );
                write_arb_to_file(&message);
            }
        }
    }

    // Update tracked arbs for next cycle
    *prev_arbs = current_arbs;
}

/// Clears the terminal screen
pub fn clear_screen() {
    print!("\x1B[2J\x1B[1;1H");
}

/// Displays the price monitor output
pub fn display_prices(prices: &[PoolPrice], elapsed_ms: u128) {
    clear_screen();

    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");

    // Header
    println!(
        "\x1b[1;36m{}\x1b[0m",
        "═".repeat(67)
    );
    println!(
        "\x1b[1;36m  WMON/USDC Price Monitor | Monad Mainnet | {}\x1b[0m",
        timestamp
    );
    println!(
        "\x1b[1;36m{}\x1b[0m",
        "═".repeat(67)
    );
    println!();

    if prices.is_empty() {
        println!("\x1b[1;31m  No price data available. Check RPC connection.\x1b[0m");
        println!();
        return;
    }

    // Sort prices by price descending (highest first)
    let mut sorted_prices = prices.to_vec();
    sorted_prices.sort_by(|a, b| b.price.partial_cmp(&a.price).unwrap());

    let best_price = sorted_prices[0].price;

    // Price table header
    println!(
        "  \x1b[1m{:<16} │ {:>12} │ {:>10} │ {:>6}\x1b[0m",
        "DEX", "Price (USDC)", "vs Best", "Fee"
    );
    println!("  {}", "─".repeat(16 + 3 + 12 + 3 + 10 + 3 + 6));

    // Price rows
    for (i, price) in sorted_prices.iter().enumerate() {
        let vs_best = if i == 0 {
            "\x1b[1;32mBEST\x1b[0m".to_string()
        } else {
            let diff_pct = ((price.price - best_price) / best_price) * 100.0;
            format!("\x1b[1;33m{:+.2}%\x1b[0m", diff_pct)
        };

        let fee_str = format!("{:.2}%", price.fee_bps as f64 / 100.0);

        println!(
            "  {:<16} │ {:>12.5} │ {:>10} │ {:>6}",
            price.pool_name, price.price, vs_best, fee_str
        );
    }

    println!();

    // Spread opportunities
    let spreads = calculate_spreads(&sorted_prices);

    // Log profitable arb opportunities (net > 0.1%) to stderr so they persist
    log_arb_opportunities(&spreads, &timestamp.to_string());

    println!(
        "\x1b[1;36m{}\x1b[0m",
        "═".repeat(67)
    );
    println!("\x1b[1;36m  SPREAD OPPORTUNITIES (sorted by net profit)\x1b[0m");
    println!(
        "\x1b[1;36m{}\x1b[0m",
        "═".repeat(67)
    );
    println!();

    if spreads.is_empty() {
        println!("  No spread opportunities found.");
    } else {
        for spread in spreads.iter() {
            let profit_indicator = if spread.net_spread_pct > 0.0 {
                "\x1b[1;32m✓\x1b[0m"
            } else {
                "\x1b[1;31m✗\x1b[0m"
            };

            println!(
                "  Buy @ {} ({:.5}) → Sell @ {} ({:.5})",
                spread.buy_pool, spread.buy_price, spread.sell_pool, spread.sell_price
            );
            println!(
                "  Gross Spread: \x1b[1m{:.2}%\x1b[0m | Net (after fees): \x1b[1m{:+.2}%\x1b[0m {}",
                spread.gross_spread_pct, spread.net_spread_pct, profit_indicator
            );
            println!();
        }
    }

    // Footer
    println!(
        "\x1b[1;36m{}\x1b[0m",
        "═".repeat(67)
    );
    println!(
        "  Polling: 1s | RPC Calls: 1 (batched) | Last update: {}ms",
        elapsed_ms
    );
    println!(
        "\x1b[1;36m{}\x1b[0m",
        "═".repeat(67)
    );
}
