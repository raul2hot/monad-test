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
