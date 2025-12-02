use alloy::primitives::Address;
use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::HashMap;

use super::types::EdgeData;
use crate::config::thresholds;
use crate::config::tokens;
use crate::dex::Pool;

/// Directed graph representing token swap relationships across DEXes
pub struct ArbitrageGraph {
    pub graph: DiGraph<Address, EdgeData>,
    token_to_node: HashMap<Address, NodeIndex>,
}

impl Default for ArbitrageGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl ArbitrageGraph {
    /// Create a new empty arbitrage graph
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            token_to_node: HashMap::new(),
        }
    }

    /// Get or create a node for a token address
    fn get_or_create_node(&mut self, token: Address) -> NodeIndex {
        if let Some(&node) = self.token_to_node.get(&token) {
            node
        } else {
            let node = self.graph.add_node(token);
            self.token_to_node.insert(token, node);
            node
        }
    }

    /// Add a pool to the graph, creating edges in both directions
    pub fn add_pool(&mut self, pool: &Pool) {
        // Skip pools with invalid prices
        if !pool.is_price_valid() {
            tracing::trace!("Skipping pool {} - invalid price", pool.address);
            return;
        }

        // NEW: Skip pools where round-trip price deviates significantly from 1.0
        // This catches pools with inconsistent or corrupted price data
        // price_0_to_1 * price_1_to_0 should equal ~1.0 (minus fees)
        let price_0_to_1 = pool.price_0_to_1();
        let price_1_to_0 = pool.price_1_to_0();
        let round_trip = price_0_to_1 * price_1_to_0;
        if (round_trip - 1.0).abs() > thresholds::MAX_ROUND_TRIP_DEVIATION {
            tracing::warn!(
                "Skipping pool {} ({}) - round trip price deviation: {:.4} (expected ~1.0, got {})",
                pool.address,
                pool.dex,
                (round_trip - 1.0).abs(),
                round_trip
            );
            return;
        }

        let node0 = self.get_or_create_node(pool.token0);
        let node1 = self.get_or_create_node(pool.token1);

        // Get liquidity as f64 for edge data
        let liquidity = pool.liquidity.to::<u128>() as f64;

        // Add edge from token0 -> token1
        let price_0_to_1 = pool.effective_price_0_to_1();

        // Validate price is in reasonable range (1e-10 to 1e10)
        if price_0_to_1.is_finite() && price_0_to_1 > 1e-10 && price_0_to_1 < 1e10 {
            let edge_data = EdgeData::new(pool.address, pool.dex, price_0_to_1, pool.fee, liquidity);
            self.graph.add_edge(node0, node1, edge_data);
        } else {
            tracing::trace!(
                "Skipping edge {} -> {} - price {} out of range",
                tokens::symbol(pool.token0),
                tokens::symbol(pool.token1),
                price_0_to_1
            );
        }

        // Add edge from token1 -> token0
        let price_1_to_0 = pool.effective_price_1_to_0();
        if price_1_to_0.is_finite() && price_1_to_0 > 1e-10 && price_1_to_0 < 1e10 {
            let edge_data = EdgeData::new(pool.address, pool.dex, price_1_to_0, pool.fee, liquidity);
            self.graph.add_edge(node1, node0, edge_data);
        } else {
            tracing::trace!(
                "Skipping edge {} -> {} - price {} out of range",
                tokens::symbol(pool.token1),
                tokens::symbol(pool.token0),
                price_1_to_0
            );
        }
    }

    /// Get the node index for a token address
    pub fn get_node(&self, token: Address) -> Option<NodeIndex> {
        self.token_to_node.get(&token).copied()
    }

    /// Get the token address for a node index
    pub fn get_token(&self, node: NodeIndex) -> Option<Address> {
        self.graph.node_weight(node).copied()
    }

    /// Get the number of nodes (tokens) in the graph
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Get the number of edges (swap paths) in the graph
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }
}
