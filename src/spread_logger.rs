//! Spread event logging for analysis

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};

use chrono::Local;
use serde::Serialize;

use crate::spread_display::{SpreadDisplay, SpreadLevel, Trend};

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

/// Extension method to log significant spreads from SpreadDisplay
pub fn log_significant_spreads(
    display: &SpreadDisplay,
    logger: &mut SpreadLogger,
    block: Option<u64>,
    prices: &[(String, f64, f64)], // (pool_name, price, fee_bps)
) {
    // Build price lookup map
    let price_map: std::collections::HashMap<&str, (f64, f64)> = prices
        .iter()
        .map(|(name, price, fee)| (name.as_str(), (*price, *fee)))
        .collect();

    for (key, hist) in &display.pair_histories {
        if let Some(&spread_bps) = hist.history.back() {
            if spread_bps >= 10 {
                // Log spreads >= 10bps
                // Parse the key to get buy and sell pools
                let parts: Vec<_> = key.split('→').collect();
                let buy_pool = parts.first().unwrap_or(&"").to_string();
                let sell_pool = parts.get(1).unwrap_or(&"").to_string();

                // Get prices if available
                let (buy_price, _) = price_map
                    .get(buy_pool.as_str())
                    .copied()
                    .unwrap_or((0.0, 0.0));
                let (sell_price, _) = price_map
                    .get(sell_pool.as_str())
                    .copied()
                    .unwrap_or((0.0, 0.0));

                let level = SpreadLevel::from_bps(spread_bps);
                let trend = hist.trend();

                let event = SpreadEvent {
                    timestamp: Local::now().to_rfc3339(),
                    block_number: block,
                    buy_pool,
                    sell_pool,
                    buy_price,
                    sell_price,
                    gross_spread_bps: spread_bps + 10, // Approximate (add back fees)
                    net_spread_bps: spread_bps,
                    level: level.label().to_string(),
                    trend: trend.arrow().to_string(),
                    velocity_bps_sec: None, // Fill from velocity analysis if available
                };
                logger.log(&event);
            }
        }
    }
}
