use alloy::primitives::Address;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use std::collections::HashSet;

use super::builder::ArbitrageGraph;
use crate::config::tokens;
use crate::dex::Dex;

/// Represents a detected arbitrage cycle
#[derive(Debug, Clone)]
pub struct ArbitrageCycle {
    pub path: Vec<Address>,
    pub pools: Vec<Address>,
    pub dexes: Vec<Dex>,
    pub total_weight: f64,
    pub expected_return: f64,
    pub prices: Vec<f64>,
    pub fees: Vec<u32>,
}

impl ArbitrageCycle {
    /// Calculate profit as a percentage
    pub fn profit_percentage(&self) -> f64 {
        (self.expected_return - 1.0) * 100.0
    }

    /// Calculate profit in basis points
    pub fn profit_bps(&self) -> u32 {
        ((self.expected_return - 1.0) * 10000.0) as u32
    }

    /// Get the number of hops in the cycle
    pub fn hop_count(&self) -> usize {
        self.pools.len()
    }

    /// Check if this is a cross-DEX arbitrage
    pub fn is_cross_dex(&self) -> bool {
        if self.dexes.is_empty() {
            return false;
        }
        let first = self.dexes[0];
        self.dexes.iter().any(|d| *d != first)
    }

    /// Get a formatted string of the DEX path
    pub fn dex_path(&self) -> String {
        self.dexes
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join(" -> ")
    }

    /// Get a formatted string of the token path
    pub fn token_path(&self) -> String {
        self.path
            .iter()
            .map(|addr| tokens::symbol(*addr))
            .collect::<Vec<_>>()
            .join(" -> ")
    }

    /// Calculate average fee in basis points
    pub fn avg_fee_bps(&self) -> f64 {
        if self.fees.is_empty() {
            return 0.0;
        }
        self.fees.iter().map(|&f| f as f64).sum::<f64>() / self.fees.len() as f64 / 100.0
    }

    /// Validate the cycle structure
    pub fn is_valid(&self) -> bool {
        // Must have at least 3 nodes (A -> B -> A)
        if self.path.len() < 3 {
            return false;
        }

        // Must end where it started
        if self.path.first() != self.path.last() {
            return false;
        }

        // Path length should be pools + 1
        if self.path.len() != self.pools.len() + 1 {
            return false;
        }

        // Intermediate nodes must be unique
        let intermediate: Vec<_> = self.path[1..self.path.len() - 1].to_vec();
        let unique_intermediate: HashSet<_> = intermediate.iter().collect();
        if unique_intermediate.len() != intermediate.len() {
            return false;
        }

        // Start token should not appear in intermediate
        let start = self.path[0];
        if intermediate.contains(&start) {
            return false;
        }

        // Pools must be unique
        let unique_pools: HashSet<_> = self.pools.iter().collect();
        if unique_pools.len() != self.pools.len() {
            return false;
        }

        // Expected return must be positive and finite
        if self.expected_return <= 0.0 || !self.expected_return.is_finite() {
            return false;
        }

        // FIXED: Cap at 50% to filter noise (real arb is usually < 5%)
        // Even 50% is generous - most real opportunities are < 1%
        if self.expected_return > 1.5 {
            tracing::trace!(
                "Rejecting cycle with unrealistic return: {:.2}%",
                self.profit_percentage()
            );
            return false;
        }

        // Minimum return check (avoid dust) - less than 0.01%
        if self.expected_return < 1.0001 {
            return false;
        }

        true
    }

    /// Confidence score based on various factors
    /// Returns a score where higher is more confident the opportunity is real
    pub fn confidence_score(&self) -> f64 {
        let mut score = 1.0;

        // Cross-DEX is more likely to be real
        if self.is_cross_dex() {
            score *= 1.5;
        }

        // Lower profit is more likely to be real
        if self.profit_percentage() < 1.0 {
            score *= 1.2;
        } else if self.profit_percentage() > 5.0 {
            score *= 0.5; // Suspicious
        }

        // Fewer hops is better
        if self.hop_count() <= 3 {
            score *= 1.1;
        }

        score
    }

    /// Get confidence level as a string
    pub fn confidence_level(&self) -> &'static str {
        if self.profit_percentage() < 1.0 {
            "HIGH"
        } else if self.profit_percentage() < 5.0 {
            "MEDIUM"
        } else {
            "LOW - VERIFY"
        }
    }
}

/// Bounded depth-first search for finding negative cycles (arbitrage opportunities)
pub struct BoundedBellmanFord<'a> {
    graph: &'a ArbitrageGraph,
    max_hops: usize,
    min_return: f64, // Minimum expected return (e.g., 1.001 for 0.1%)
}

impl<'a> BoundedBellmanFord<'a> {
    /// Create a new cycle detector
    pub fn new(graph: &'a ArbitrageGraph, max_hops: usize, min_profit_bps: u32) -> Self {
        let min_return = 1.0 + (min_profit_bps as f64 / 10000.0);
        Self {
            graph,
            max_hops,
            min_return,
        }
    }

    /// Find all profitable cycles starting from a specific token
    pub fn find_cycles_from(&self, start_token: Address) -> Vec<ArbitrageCycle> {
        let mut cycles = Vec::new();
        let Some(start_node) = self.graph.get_node(start_token) else {
            return cycles;
        };

        self.dfs_find_cycles(
            start_node,
            start_node,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            HashSet::new(),
            0.0,
            &mut cycles,
            1,
        );

        cycles
    }

    #[allow(clippy::too_many_arguments)]
    fn dfs_find_cycles(
        &self,
        start_node: NodeIndex,
        current_node: NodeIndex,
        mut path: Vec<Address>,
        pools: Vec<Address>,
        dexes: Vec<Dex>,
        prices: Vec<f64>,
        fees: Vec<u32>,
        mut visited: HashSet<NodeIndex>,
        total_weight: f64,
        cycles: &mut Vec<ArbitrageCycle>,
        depth: usize,
    ) {
        let current_token = match self.graph.get_token(current_node) {
            Some(t) => t,
            None => return,
        };

        path.push(current_token);
        if depth > 1 {
            visited.insert(current_node);
        }
        if depth > self.max_hops {
            return;
        }

        for edge in self.graph.graph.edges(current_node) {
            let target = edge.target();
            let edge_data = edge.weight();
            let new_weight = total_weight + edge_data.weight;

            // Check if we've completed a cycle back to start
            if target == start_node && depth >= 2 {
                let expected_return = (-new_weight).exp();

                // Only record profitable cycles above threshold
                if expected_return >= self.min_return {
                    let mut final_path = path.clone();
                    final_path.push(self.graph.get_token(start_node).unwrap());

                    let mut final_pools = pools.clone();
                    final_pools.push(edge_data.pool_address);

                    let mut final_dexes = dexes.clone();
                    final_dexes.push(edge_data.dex);

                    let mut final_prices = prices.clone();
                    final_prices.push(edge_data.price);

                    let mut final_fees = fees.clone();
                    final_fees.push(edge_data.fee);

                    let cycle = ArbitrageCycle {
                        path: final_path,
                        pools: final_pools,
                        dexes: final_dexes,
                        total_weight: new_weight,
                        expected_return,
                        prices: final_prices,
                        fees: final_fees,
                    };

                    if cycle.is_valid() {
                        cycles.push(cycle);
                    }
                }
            } else if !visited.contains(&target) && depth < self.max_hops {
                let mut new_pools = pools.clone();
                new_pools.push(edge_data.pool_address);

                let mut new_dexes = dexes.clone();
                new_dexes.push(edge_data.dex);

                let mut new_prices = prices.clone();
                new_prices.push(edge_data.price);

                let mut new_fees = fees.clone();
                new_fees.push(edge_data.fee);

                self.dfs_find_cycles(
                    start_node,
                    target,
                    path.clone(),
                    new_pools,
                    new_dexes,
                    new_prices,
                    new_fees,
                    visited.clone(),
                    new_weight,
                    cycles,
                    depth + 1,
                );
            }
        }
    }

    /// Find all profitable cycles starting from base tokens
    pub fn find_all_cycles(&self, base_tokens: &[Address]) -> Vec<ArbitrageCycle> {
        let mut all_cycles = Vec::new();
        let mut seen_signatures: HashSet<String> = HashSet::new();

        tracing::info!(
            "Searching for cycles from {} base tokens in graph with {} nodes, {} edges",
            base_tokens.len(),
            self.graph.node_count(),
            self.graph.edge_count()
        );

        for &token in base_tokens {
            let token_symbol = crate::config::tokens::symbol(token);
            let cycles = self.find_cycles_from(token);

            tracing::debug!(
                "Found {} raw cycles starting from {}",
                cycles.len(),
                token_symbol
            );

            for cycle in cycles {
                let signature = create_cycle_signature(&cycle);
                if !seen_signatures.contains(&signature) {
                    seen_signatures.insert(signature.clone());

                    // Log each unique cycle found
                    tracing::info!(
                        "Unique cycle: {} | {} hops | {:.2}% profit | {}",
                        cycle.token_path(),
                        cycle.hop_count(),
                        cycle.profit_percentage(),
                        if cycle.is_cross_dex() { "CROSS-DEX" } else { "single-dex" }
                    );

                    all_cycles.push(cycle);
                }
            }
        }

        tracing::info!(
            "Total unique cycles found: {} (from {} base tokens)",
            all_cycles.len(),
            base_tokens.len()
        );

        // Sort by expected return (best first)
        all_cycles.sort_by(|a, b| {
            b.expected_return
                .partial_cmp(&a.expected_return)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        all_cycles
    }
}

/// Create a unique signature for a cycle (independent of starting point)
fn create_cycle_signature(cycle: &ArbitrageCycle) -> String {
    let mut pool_strs: Vec<String> = cycle.pools.iter().map(|p| format!("{:?}", p)).collect();
    pool_strs.sort();
    pool_strs.join("-")
}
