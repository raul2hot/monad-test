# Spread Velocity Tracking - Implementation Instructions

## Context

We're running a Monad mainnet arbitrage bot. Current win rate is ~30% (3/10). Analysis suggests:

1. **Sudden spikes** are profitable - spread jumps rapidly, we enter early and execute at peak
2. **Gradual climbs** are unprofitable - spread slowly rises, by trigger time it's already decaying
3. We need data to confirm this hypothesis before building a spike detector

## Objective

Add spread history tracking to capture the last N spread values BEFORE each arb trigger. This data will help us understand:
- Was the spread accelerating (spike) or decelerating (gradual)?
- What's the velocity pattern of winning vs losing trades?

**Critical constraint: ZERO additional latency.** Use data already collected during polling.

## Command Line Arguments

Add these to the `AutoArb` command in `src/main.rs`:

```rust
/// Enable spread velocity tracking (saves last N spreads before trigger)
#[arg(long, default_value = "false")]
track_velocity: bool,

/// Number of spread snapshots to retain in ring buffer
#[arg(long, default_value = "10")]
history_size: usize,

/// Minimum spread velocity (bps/sec) to trigger - 0 disables velocity filter
#[arg(long, default_value = "0")]
min_velocity: i32,
```

## Technical Design

### 1. Create New Module: `src/spread_tracker.rs`

```rust
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
    pub fn record(&mut self, 
                  buy_pool: &str, 
                  sell_pool: &str,
                  buy_price: f64,
                  sell_price: f64,
                  gross_spread_bps: i32,
                  net_spread_bps: i32) {
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

        Some(VelocityAnalysis {
            snapshots,
            velocity_bps_per_sec,
            acceleration,
            is_spike: velocity_bps_per_sec > 10.0, // Threshold: 10 bps/sec = spike
            spread_at_trigger: last.net_spread_bps,
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
            .join(" → ")
    }
}
```

### 2. Update `src/stats.rs`

Add velocity analysis to `PreExecutionSnapshot`:

```rust
// Add to PreExecutionSnapshot struct:
pub spread_history: Option<Vec<SpreadSnapshot>>,  // Last N spreads before trigger
pub velocity_bps_per_sec: Option<f64>,
pub acceleration: Option<f64>,
pub is_spike_pattern: Option<bool>,
```

Add velocity analysis to `ArbExecutionRecord` or create separate velocity log.

### 3. Update `src/main.rs` - `run_auto_arb` Function

**Step 3a: Add tracker initialization after provider setup**

```rust
// After stats_logger initialization:
let mut spread_tracker = if track_velocity {
    Some(SpreadTracker::new(history_size))
} else {
    None
};
```

**Step 3b: Record spread every poll cycle (inside main loop)**

After `calculate_spreads(&prices)` and finding `best_spread`:

```rust
// Record spread for velocity tracking (no extra latency - uses existing data)
if let (Some(tracker), Some(spread)) = (&mut spread_tracker, best_spread) {
    tracker.record(
        &spread.buy_pool,
        &spread.sell_pool,
        spread.buy_price,
        spread.sell_price,
        (spread.gross_spread_pct * 100.0) as i32,
        (spread.net_spread_pct * 100.0) as i32,
    );
}
```

**Step 3c: Analyze velocity at trigger time**

When `net_spread_bps >= min_spread_bps && cooldown_elapsed`:

```rust
// Analyze spread velocity before execution
let velocity_analysis = spread_tracker.as_ref().and_then(|t| t.analyze());

if let Some(ref analysis) = velocity_analysis {
    println!("\n  SPREAD VELOCITY ANALYSIS:");
    println!("    History: {}", spread_tracker.as_ref().unwrap().format_history());
    println!("    Velocity: {:.2} bps/sec", analysis.velocity_bps_per_sec);
    println!("    Acceleration: {:.2} bps/sec²", analysis.acceleration);
    println!("    Pattern: {}", if analysis.is_spike { "SPIKE ⚡" } else { "GRADUAL 📈" });
    println!("    Window: {} ms", analysis.window_duration_ms);
    println!("    Range: {} to {} bps", analysis.min_spread_in_window, analysis.max_spread_in_window);
}

// Optional: Skip if velocity filter enabled and not a spike
if min_velocity > 0 {
    if let Some(ref analysis) = velocity_analysis {
        if analysis.velocity_bps_per_sec < min_velocity as f64 {
            println!("    SKIPPING: velocity {:.2} < threshold {} bps/sec", 
                analysis.velocity_bps_per_sec, min_velocity);
            continue;
        }
    }
}
```

**Step 3d: Include in stats logging**

Update `PreExecutionSnapshot` creation to include velocity data:

```rust
let pre_snapshot = PreExecutionSnapshot {
    // ... existing fields ...
    spread_history: velocity_analysis.as_ref().map(|a| a.snapshots.clone()),
    velocity_bps_per_sec: velocity_analysis.as_ref().map(|a| a.velocity_bps_per_sec),
    acceleration: velocity_analysis.as_ref().map(|a| a.acceleration),
    is_spike_pattern: velocity_analysis.as_ref().map(|a| a.is_spike),
};
```

### 4. Update Module Declarations

In `src/main.rs`, add:
```rust
mod spread_tracker;
use spread_tracker::SpreadTracker;
```

### 5. Output Format

When `--track-velocity` is enabled, console output should show:

```
[14:32:15] Best: PancakeSwap1 -> Uniswap | Net: +0.05% (+5 bps)    

  OPPORTUNITY DETECTED! Net spread: 5 bps (threshold: 5 bps)

  SPREAD VELOCITY ANALYSIS:
    History: -2bps@0ms → 0bps@50ms → 1bps@100ms → 3bps@150ms → 5bps@200ms
    Velocity: 35.00 bps/sec
    Acceleration: 12.50 bps/sec²
    Pattern: SPIKE ⚡
    Window: 200 ms
    Range: -2 to 5 bps
```

### 6. Stats File Enhancement

The JSONL stats file should include velocity data when available:

```json
{
  "id": 1,
  "pre": {
    "timestamp": "2025-01-15T14:32:15.123Z",
    "spread_history": [
      {"timestamp_ms": 0, "net_spread_bps": -2, ...},
      {"timestamp_ms": 50, "net_spread_bps": 0, ...},
      {"timestamp_ms": 100, "net_spread_bps": 1, ...},
      {"timestamp_ms": 150, "net_spread_bps": 3, ...},
      {"timestamp_ms": 200, "net_spread_bps": 5, ...}
    ],
    "velocity_bps_per_sec": 35.0,
    "acceleration": 12.5,
    "is_spike_pattern": true,
    ...
  },
  "post": {...},
  "success": true
}
```

## Testing Commands

```bash
# Basic velocity tracking (data collection only)
cargo run -- auto-arb --min-spread-bps 5 --amount 0.1 --track-velocity --history-size 10 --force

# With velocity filter (skip non-spikes)
cargo run -- auto-arb --min-spread-bps 5 --amount 0.1 --track-velocity --min-velocity 20 --force

# Large history for analysis
cargo run -- auto-arb --min-spread-bps 5 --amount 0.1 --track-velocity --history-size 20 --force
```

## Success Criteria

1. ✅ Zero additional RPC calls (uses existing poll data)
2. ✅ Ring buffer has O(1) insert, bounded memory
3. ✅ Velocity/acceleration calculated at trigger time
4. ✅ Data logged to JSONL for post-analysis
5. ✅ Console shows spread history before each arb attempt
6. ✅ Optional velocity filter to skip gradual climbs

## Files to Modify

1. **CREATE** `src/spread_tracker.rs` - New module
2. **MODIFY** `src/main.rs` - Add CLI args, integrate tracker
3. **MODIFY** `src/stats.rs` - Add velocity fields to snapshots

## Performance Notes

- Ring buffer is pre-allocated, no runtime allocations
- `record()` must complete in <1ms
- `analyze()` only called at trigger time, can take 1-5ms
- History size of 10-20 is sufficient for 500ms-1s analysis window at 50ms polling