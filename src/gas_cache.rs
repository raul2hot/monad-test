//! Adaptive Gas Cache with Spread-Aware Invalidation
//!
//! This module implements intelligent gas caching that accounts for market volatility.
//! High spread = volatile pool state = stale gas estimates, so we invalidate more aggressively.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Cache TTL in milliseconds (base: 30 seconds)
const GAS_CACHE_TTL_MS: u128 = 30_000;

/// Short TTL for medium spread conditions
const GAS_CACHE_TTL_MEDIUM_MS: u128 = 10_000;

/// Spread threshold for cache invalidation (if spread increased by this much, invalidate)
const SPREAD_DELTA_THRESHOLD_BPS: i32 = 20;

/// Low spread threshold (bps)
const LOW_SPREAD_BPS: i32 = 15;

/// Medium spread threshold (bps)
const MEDIUM_SPREAD_BPS: i32 = 30;

/// Gas cache entry with spread context
#[derive(Debug, Clone)]
pub struct GasCacheEntry {
    pub gas_estimate: u64,
    pub timestamp_ms: u128,
    /// Spread when this estimate was captured
    pub spread_bps_at_cache: i32,
}

/// Gas strategy decision
#[derive(Debug, Clone)]
pub enum GasDecision {
    /// Use cached gas estimate with specified buffer
    UseCached {
        gas_limit: u64,
        source: GasSource,
    },
    /// Fetch fresh gas estimate with specified buffer percentage
    FetchFresh {
        buffer_percent: u64,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum GasSource {
    Cached,
    CachedWithBuffer,
}

/// Route key for gas cache
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct RouteKey {
    pub sell_router: u8,
    pub buy_router: u8,
}

impl RouteKey {
    pub fn new(sell_router: u8, buy_router: u8) -> Self {
        Self { sell_router, buy_router }
    }
}

/// Global gas cache
lazy_static::lazy_static! {
    static ref GAS_CACHE: RwLock<HashMap<RouteKey, GasCacheEntry>> = RwLock::new(HashMap::new());
}

/// Get current timestamp in milliseconds
fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

/// Check if cache entry is valid for current market conditions
fn is_cache_valid(entry: &GasCacheEntry, current_spread_bps: i32) -> bool {
    let now = now_ms();
    let spread_delta = current_spread_bps - entry.spread_bps_at_cache;

    // Determine TTL based on spread at cache time
    let ttl = if entry.spread_bps_at_cache < LOW_SPREAD_BPS {
        GAS_CACHE_TTL_MS
    } else if entry.spread_bps_at_cache < MEDIUM_SPREAD_BPS {
        GAS_CACHE_TTL_MEDIUM_MS
    } else {
        // High spread entries should never be cached (but just in case)
        0
    };

    // TTL check
    if now - entry.timestamp_ms > ttl {
        return false;
    }

    // CRITICAL: Invalidate if spread increased significantly
    // High spread = volatile state = gas estimate is stale
    if spread_delta > SPREAD_DELTA_THRESHOLD_BPS {
        return false;
    }

    true
}

/// Get cached gas estimate for a route
pub fn get_cached_gas(route: &RouteKey, current_spread_bps: i32) -> Option<u64> {
    let cache = GAS_CACHE.read().ok()?;
    let entry = cache.get(route)?;

    if is_cache_valid(entry, current_spread_bps) {
        Some(entry.gas_estimate)
    } else {
        None
    }
}

/// Store gas estimate in cache
pub fn cache_gas_estimate(route: RouteKey, gas_estimate: u64, spread_bps: i32) {
    // Don't cache high spread estimates - they're too volatile
    if spread_bps >= MEDIUM_SPREAD_BPS {
        return;
    }

    if let Ok(mut cache) = GAS_CACHE.write() {
        cache.insert(route, GasCacheEntry {
            gas_estimate,
            timestamp_ms: now_ms(),
            spread_bps_at_cache: spread_bps,
        });
    }
}

/// Determine gas strategy based on current spread
///
/// - Low spread (<15 bps): Use cache aggressively with 8% buffer
/// - Medium spread (15-30 bps): Use cache with 15% buffer and shorter TTL
/// - High spread (>30 bps): ALWAYS fresh estimate - this is where profit is
pub fn gas_strategy(spread_bps: i32, route: &RouteKey) -> GasDecision {
    match spread_bps {
        // Low spread: use cache aggressively (profit is low anyway)
        s if s < LOW_SPREAD_BPS => {
            if let Some(cached) = get_cached_gas(route, spread_bps) {
                GasDecision::UseCached {
                    gas_limit: cached * 108 / 100, // 8% buffer
                    source: GasSource::Cached,
                }
            } else {
                GasDecision::FetchFresh { buffer_percent: 10 }
            }
        }

        // Medium spread: use cache but with larger buffer
        s if s < MEDIUM_SPREAD_BPS => {
            if let Some(cached) = get_cached_gas(route, spread_bps) {
                GasDecision::UseCached {
                    gas_limit: cached * 115 / 100, // 15% buffer
                    source: GasSource::CachedWithBuffer,
                }
            } else {
                GasDecision::FetchFresh { buffer_percent: 15 }
            }
        }

        // High spread: ALWAYS fresh estimate - this is where the money is
        _ => GasDecision::FetchFresh { buffer_percent: 20 },
    }
}

/// Calculate gas price with spread-aware priority fee bidding
///
/// When spreads are high, other bots are competing. Adjust priority fee accordingly.
pub fn calculate_gas_price(base_gas_price: u128, spread_bps: i32) -> (u128, u128) {
    // Base priority fee (10% of base gas price)
    let base_priority = base_gas_price / 10;

    // Boost priority fee based on spread (more competition = higher spread)
    // Add 1 gwei per 10 bps of spread
    let priority_boost = (spread_bps as u128 / 10) * 1_000_000_000; // gwei to wei

    let priority_fee = base_priority + priority_boost;
    let max_fee = base_gas_price + priority_fee;

    (max_fee, priority_fee)
}

/// Clear the gas cache (useful for testing or after errors)
#[allow(dead_code)]
pub fn clear_cache() {
    if let Ok(mut cache) = GAS_CACHE.write() {
        cache.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gas_strategy_low_spread() {
        let route = RouteKey::new(0, 1);
        let decision = gas_strategy(10, &route);

        match decision {
            GasDecision::FetchFresh { buffer_percent } => {
                assert_eq!(buffer_percent, 10);
            }
            _ => panic!("Expected FetchFresh for low spread with no cache"),
        }
    }

    #[test]
    fn test_gas_strategy_high_spread() {
        let route = RouteKey::new(0, 1);
        let decision = gas_strategy(50, &route);

        match decision {
            GasDecision::FetchFresh { buffer_percent } => {
                assert_eq!(buffer_percent, 20);
            }
            _ => panic!("Expected FetchFresh for high spread"),
        }
    }

    #[test]
    fn test_calculate_gas_price() {
        let base = 1_000_000_000u128; // 1 gwei
        let (max_fee, priority) = calculate_gas_price(base, 30);

        // priority = base/10 + (30/10)*1gwei = 0.1gwei + 3gwei = 3.1gwei
        assert_eq!(priority, 100_000_000 + 3_000_000_000);
        assert_eq!(max_fee, base + priority);
    }
}
