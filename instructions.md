# Smart Spread Filter Implementation

## Objective
Add a filter to `auto-arb` that skips unprofitable opportunities based on spread velocity analysis.

## Root Cause Summary
From 16 trades analyzed:
- **Velocity >100 bps/sec** → Other bot's footprint (you're late) → **SKIP**
- **Final spread <9 bps** → Insufficient margin after ~5-7 bps fees → **SKIP**
- **Velocity ~0 (GRADUAL)** → Dead/stale spread → **SKIP**
- **Baseline >2 bps** → Already elevated, late entry → **SKIP**

**Winning pattern**: Velocity 30-80 bps/sec, final spread ≥9 bps, baseline ≤2 bps

---

## Implementation Steps

### 1. Add CLI Args to `src/main.rs`

In the `AutoArb` command enum, add:

```rust
/// Maximum spread velocity (bps/sec) - skip if exceeded (bot signature)
#[arg(long, default_value = "100")]
max_velocity: i32,

/// Minimum final spread (bps) required for margin
#[arg(long, default_value = "9")]
min_final_spread: i32,

/// Maximum baseline spread (bps) - skip if already elevated
#[arg(long, default_value = "2")]
max_baseline: i32,
```

Update the `run_auto_arb` function signature to accept these parameters.

### 2. Create Filter Module `src/spread_filter.rs`

```rust
//! Smart spread filter based on velocity analysis

use crate::spread_tracker::VelocityAnalysis;

#[derive(Debug, Clone)]
pub struct SpreadFilterConfig {
    pub min_velocity: f64,      // 15.0 - Skip dead spreads
    pub max_velocity: f64,      // 100.0 - Skip bot signatures
    pub min_final_spread: i32,  // 9 - Require margin
    pub max_baseline: i32,      // 2 - Fresh opportunities only
}

impl Default for SpreadFilterConfig {
    fn default() -> Self {
        Self {
            min_velocity: 15.0,
            max_velocity: 100.0,
            min_final_spread: 9,
            max_baseline: 2,
        }
    }
}

#[derive(Debug)]
pub enum FilterResult {
    Execute,
    Skip { reason: &'static str },
}

impl SpreadFilterConfig {
    pub fn evaluate(&self, analysis: &VelocityAnalysis) -> FilterResult {
        let velocity = analysis.velocity_bps_per_sec;
        let baseline = analysis.min_spread_in_window;
        let final_spread = analysis.spread_at_trigger;

        // REJECT: Bot signature (someone else's arb created this spread)
        if velocity > self.max_velocity {
            return FilterResult::Skip {
                reason: "velocity too high - bot signature detected"
            };
        }

        // REJECT: Dead spread (GRADUAL pattern, no momentum)
        if velocity.abs() < self.min_velocity && baseline == final_spread {
            return FilterResult::Skip {
                reason: "dead spread - no momentum"
            };
        }

        // REJECT: Insufficient margin after fees
        if final_spread < self.min_final_spread {
            return FilterResult::Skip {
                reason: "spread too thin for margin"
            };
        }

        // REJECT: Already elevated baseline (late entry)
        if baseline > self.max_baseline {
            return FilterResult::Skip {
                reason: "baseline elevated - late to opportunity"
            };
        }

        FilterResult::Execute
    }
}
```

### 3. Register Module in `src/main.rs`

Add near other module declarations:
```rust
mod spread_filter;
use spread_filter::{SpreadFilterConfig, FilterResult};
```

### 4. Integrate Filter in `run_auto_arb`

In `run_auto_arb`, after velocity analysis and before execution:

```rust
// After: if let Some(ref analysis) = velocity_analysis { ... }

// Apply smart filter if velocity tracking enabled
if track_velocity {
    if let Some(ref analysis) = velocity_analysis {
        let filter = SpreadFilterConfig {
            min_velocity: min_velocity as f64,
            max_velocity: max_velocity as f64,
            min_final_spread,
            max_baseline,
        };
        
        match filter.evaluate(analysis) {
            FilterResult::Execute => {
                println!("    FILTER: PASS - executing arb");
            }
            FilterResult::Skip { reason } => {
                println!("    FILTER: SKIP - {}", reason);
                continue; // Skip this opportunity
            }
        }
    }
}
```

### 5. Update Function Signature

```rust
async fn run_auto_arb(
    min_spread_bps: i32,
    amount: f64,
    slippage: u32,
    max_executions: u32,
    cooldown_secs: u64,
    dry_run: bool,
    force: bool,
    track_velocity: bool,
    history_size: usize,
    min_velocity: i32,
    max_velocity: i32,        // NEW
    min_final_spread: i32,    // NEW
    max_baseline: i32,        // NEW
) -> Result<()>
```

---

## Usage

```bash
cargo run -- auto-arb \
  --min-spread-bps 5 \
  --amount 0.1 \
  --track-velocity \
  --min-velocity 15 \
  --max-velocity 100 \
  --min-final-spread 9 \
  --max-baseline 2 \
  --force \
  --max-executions 50
```

---

## Expected Behavior

| Scenario | Velocity | Final | Baseline | Result |
|----------|----------|-------|----------|--------|
| Bot signature | 150 | 25 | -5 | SKIP |
| Good spike | 50 | 12 | 0 | EXECUTE |
| Dead spread | 0 | 13 | 13 | SKIP |
| Thin margin | 45 | 6 | 1 | SKIP |
| Late entry | 40 | 15 | 8 | SKIP |

---

## Files to Modify

1. `src/main.rs` - Add CLI args, import filter, integrate in `run_auto_arb`
2. `src/spread_filter.rs` - New file with filter logic

## Files to Reference (read-only)

- `src/spread_tracker.rs` - `VelocityAnalysis` struct definition
- `src/stats.rs` - Logging structures