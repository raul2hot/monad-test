//! Token price monitor for multi-token arbitrage tracking

use crate::config::TokenInfo;
use dashmap::DashMap;

/// Individual token monitor state
pub struct TokenMonitor {
    pub token: TokenInfo,
    pub uniswap_price: Option<f64>,
    pub nadfun_price: Option<f64>,
    pub last_uniswap_update: Option<u64>,  // block number
    pub last_nadfun_update: Option<u64>,
}

/// Multi-token state tracker
pub struct MultiTokenState {
    tokens: DashMap<String, TokenMonitor>,
}

/// Venue enum for price updates
#[derive(Clone, Copy, Debug)]
pub enum Venue {
    Uniswap,
    Nadfun,
}

/// Represents an arbitrage opportunity
#[derive(Debug, Clone)]
pub struct ArbOpportunity {
    pub symbol: String,
    pub buy_venue: Venue,
    pub sell_venue: Venue,
    pub spread_pct: f64,
    pub buy_price: f64,
    pub sell_price: f64,
}

impl MultiTokenState {
    /// Create a new multi-token state tracker
    pub fn new(tokens: Vec<TokenInfo>) -> Self {
        let state = Self {
            tokens: DashMap::new(),
        };

        for token in tokens {
            state.tokens.insert(token.symbol.to_string(), TokenMonitor {
                token,
                uniswap_price: None,
                nadfun_price: None,
                last_uniswap_update: None,
                last_nadfun_update: None,
            });
        }

        state
    }

    /// Update price for a specific token and venue
    pub fn update_price(&self, symbol: &str, venue: Venue, price: f64, block: u64) {
        if let Some(mut monitor) = self.tokens.get_mut(symbol) {
            match venue {
                Venue::Uniswap => {
                    monitor.uniswap_price = Some(price);
                    monitor.last_uniswap_update = Some(block);
                }
                Venue::Nadfun => {
                    monitor.nadfun_price = Some(price);
                    monitor.last_nadfun_update = Some(block);
                }
            }
        }
    }

    /// Get the current spread for a token (as percentage)
    pub fn get_spread(&self, symbol: &str) -> Option<f64> {
        let monitor = self.tokens.get(symbol)?;
        let up = monitor.uniswap_price?;
        let np = monitor.nadfun_price?;

        if up > 0.0 && np > 0.0 {
            Some(((np - up) / up) * 100.0)
        } else {
            None
        }
    }

    /// Check for arbitrage opportunity on a specific token
    pub fn check_arbitrage(&self, symbol: &str) -> Option<ArbOpportunity> {
        let monitor = self.tokens.get(symbol)?;
        let up = monitor.uniswap_price?;
        let np = monitor.nadfun_price?;

        if up <= 0.0 || np <= 0.0 {
            return None;
        }

        let spread = ((np - up) / up) * 100.0;

        // Minimum 1.5% spread to cover fees
        const MIN_SPREAD: f64 = 1.5;

        if spread.abs() > MIN_SPREAD {
            Some(ArbOpportunity {
                symbol: symbol.to_string(),
                buy_venue: if spread > 0.0 { Venue::Uniswap } else { Venue::Nadfun },
                sell_venue: if spread > 0.0 { Venue::Nadfun } else { Venue::Uniswap },
                spread_pct: spread.abs(),
                buy_price: if spread > 0.0 { up } else { np },
                sell_price: if spread > 0.0 { np } else { up },
            })
        } else {
            None
        }
    }

    /// Scan all tokens for arbitrage opportunities
    pub fn scan_all_for_arb(&self) -> Vec<ArbOpportunity> {
        let mut opportunities = vec![];

        for entry in self.tokens.iter() {
            if let Some(arb) = self.check_arbitrage(entry.key()) {
                opportunities.push(arb);
            }
        }

        // Sort by spread percentage (highest first)
        opportunities.sort_by(|a, b| b.spread_pct.partial_cmp(&a.spread_pct).unwrap());
        opportunities
    }

    /// Get all tracked symbols
    pub fn get_symbols(&self) -> Vec<String> {
        self.tokens.iter().map(|e| e.key().clone()).collect()
    }

    /// Print prices for a specific token
    pub fn print_prices(&self, symbol: &str, block: u64, source: &str) {
        if let Some(monitor) = self.tokens.get(symbol) {
            println!("\n[Block {}] {}/WMON (updated by {})", block, symbol, source);

            if let Some(np) = monitor.nadfun_price {
                println!("  Nad.fun:   {:.10} MON", np);
            } else {
                println!("  Nad.fun:   -- (waiting for data)");
            }

            if let Some(up) = monitor.uniswap_price {
                println!("  Uniswap:   {:.10} MON", up);
            } else {
                println!("  Uniswap:   -- (waiting for data)");
            }

            if let Some(spread) = self.get_spread(symbol) {
                let spread_sign = if spread >= 0.0 { "+" } else { "" };
                println!("  Spread:    {}{:.2}%", spread_sign, spread);

                if let Some(arb) = self.check_arbitrage(symbol) {
                    println!("\n========== ARBITRAGE DETECTED ==========");
                    println!("  Token:  {}", arb.symbol);
                    println!("  Spread: {:.2}%", arb.spread_pct);
                    println!("  Action: BUY on {:?} @ {:.10}", arb.buy_venue, arb.buy_price);
                    println!("  Action: SELL on {:?} @ {:.10}", arb.sell_venue, arb.sell_price);
                    println!("  Est. Profit: {:.2}% (before gas)", arb.spread_pct - 2.0);
                    println!("=========================================\n");
                }
            }
        }
    }
}
