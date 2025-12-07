//! Spread velocity tracking for arbitrage analysis
//!
//! Tracks spread history to identify spike vs gradual patterns before arb execution.
//! Uses a ring buffer for O(1) insertion with bounded memory.

use std::collections::VecDeque;
use std::time::Instant;
use serde::{Serialize, Deserialize};

/// Single spread snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpreadSnapshot {
    pub timestamp_ms: u128,        // Milliseconds since tracker start
    pub buy_pool: String,
    pub sell_pool: String,
    pub buy_price: f64,
    pub sell_price: f64,
    pub gross_spread_bps: i32,
    pub net_spread_bps: i32,
}

/// Spread velocity analysis result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VelocityAnalysis {
    pub snapshots: Vec<SpreadSnapshot>,  // Last N snapshots (oldest first)
    pub velocity_bps_per_sec: f64,       // Rate of change
    pub acceleration: f64,                // 2nd derivative
    pub is_spike: bool,                   // velocity > threshold
    pub spread_at_trigger: i32,
    pub max_spread_in_window: i32,
    pub min_spread_in_window: i32,
    pub window_duration_ms: u128,
}

/// Ring buffer for spread history - zero allocation after init
pub struct SpreadTracker {
    history: VecDeque<SpreadSnapshot>,
    capacity: usize,
    start_time: Instant,
}

impl SpreadTracker {
    pub fn new(capacity: usize) -> Self {
        Self {
            history: VecDeque::with_capacity(capacity),
            capacity,
            start_time: Instant::now(),
        }
    }

    /// Record current best spread - called every poll cycle
    /// MUST be non-blocking and fast
    pub fn record(
        &mut self,
        buy_pool: &str,
        sell_pool: &str,
        buy_price: f64,
        sell_price: f64,
        gross_spread_bps: i32,
        net_spread_bps: i32,
    ) {
        let snapshot = SpreadSnapshot {
            timestamp_ms: self.start_time.elapsed().as_millis(),
            buy_pool: buy_pool.to_string(),
            sell_pool: sell_pool.to_string(),
            buy_price,
            sell_price,
            gross_spread_bps,
            net_spread_bps,
        };

        if self.history.len() >= self.capacity {
            self.history.pop_front();
        }
        self.history.push_back(snapshot);
    }

    /// Analyze velocity at trigger time - returns None if insufficient data
    pub fn analyze(&self) -> Option<VelocityAnalysis> {
        if self.history.len() < 2 {
            return None;
        }

        let snapshots: Vec<_> = self.history.iter().cloned().collect();
        let n = snapshots.len();

        let first = &snapshots[0];
        let last = &snapshots[n - 1];

        let window_duration_ms = last.timestamp_ms - first.timestamp_ms;
        if window_duration_ms == 0 {
            return None;
        }

        // Calculate velocity: (last_spread - first_spread) / time_seconds
        let spread_delta = last.net_spread_bps - first.net_spread_bps;
        let time_secs = window_duration_ms as f64 / 1000.0;
        let velocity_bps_per_sec = spread_delta as f64 / time_secs;

        // Calculate acceleration using middle point
        let acceleration = if n >= 3 {
            let mid = n / 2;
            let mid_snapshot = &snapshots[mid];

            let t1 = (mid_snapshot.timestamp_ms - first.timestamp_ms) as f64 / 1000.0;
            let t2 = (last.timestamp_ms - mid_snapshot.timestamp_ms) as f64 / 1000.0;

            if t1 > 0.0 && t2 > 0.0 {
                let v1 = (mid_snapshot.net_spread_bps - first.net_spread_bps) as f64 / t1;
                let v2 = (last.net_spread_bps - mid_snapshot.net_spread_bps) as f64 / t2;
                (v2 - v1) / ((t1 + t2) / 2.0)
            } else {
                0.0
            }
        } else {
            0.0
        };

        let max_spread = snapshots.iter().map(|s| s.net_spread_bps).max().unwrap_or(0);
        let min_spread = snapshots.iter().map(|s| s.net_spread_bps).min().unwrap_or(0);
        let spread_at_trigger = last.net_spread_bps;

        Some(VelocityAnalysis {
            snapshots,
            velocity_bps_per_sec,
            acceleration,
            is_spike: velocity_bps_per_sec > 10.0, // Threshold: 10 bps/sec = spike
            spread_at_trigger,
            max_spread_in_window: max_spread,
            min_spread_in_window: min_spread,
            window_duration_ms,
        })
    }

    /// Get last N snapshots as formatted string for logging
    pub fn format_history(&self) -> String {
        self.history
            .iter()
            .map(|s| format!("{}bps@{}ms", s.net_spread_bps, s.timestamp_ms))
            .collect::<Vec<_>>()
            .join(" -> ")
    }
}
