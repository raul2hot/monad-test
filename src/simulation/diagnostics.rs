//! Price Discrepancy Diagnostic Module
//!
//! This module helps identify why graph-detected opportunities don't match
//! actual quoter results. It compares:
//! 1. Pool prices stored in the graph
//! 2. Actual quoter results for the same amounts
//! 3. Identifies the source of discrepancy

use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use eyre::Result;
use std::sync::Arc;

use crate::config::tokens;
use crate::dex::{Dex, Pool};
use crate::graph::ArbitrageCycle;
use crate::simulation::quote_fetcher::{PoolInfo, QuoteFetcher};

/// Diagnostic result for a single pool
#[derive(Debug)]
pub struct PoolDiagnostic {
    pub pool_address: Address,
    pub dex: Dex,
    pub token0_symbol: &'static str,
    pub token1_symbol: &'static str,
    /// Price from Pool struct (used in graph)
    pub graph_price_0_to_1: f64,
    pub graph_price_1_to_0: f64,
    /// Price from effective_price (with fees)
    pub graph_effective_price_0_to_1: f64,
    pub graph_effective_price_1_to_0: f64,
    /// Actual quoter price for 1 token
    pub quoter_price_0_to_1: Option<f64>,
    pub quoter_price_1_to_0: Option<f64>,
    /// Discrepancy in basis points
    pub discrepancy_0_to_1_bps: Option<i32>,
    pub discrepancy_1_to_0_bps: Option<i32>,
    /// Fee information
    pub stored_fee_raw: u32,
    pub stored_fee_bps: u32,
    pub quoter_fee_bps: Option<u32>,
    /// Liquidity
    pub liquidity: U256,
    /// Raw sqrtPriceX96
    pub sqrt_price_x96: U256,
    /// Decimals
    pub decimals0: u8,
    pub decimals1: u8,
}

impl PoolDiagnostic {
    pub fn print(&self) {
        println!("========================================");
        println!("Pool: {} ({}-{})", self.pool_address, self.token0_symbol, self.token1_symbol);
        println!("DEX: {}", self.dex);
        println!("----------------------------------------");
        println!("GRAPH PRICES (from Pool struct):");
        println!("  price_0_to_1 (raw):       {:.10}", self.graph_price_0_to_1);
        println!("  price_1_to_0 (raw):       {:.10}", self.graph_price_1_to_0);
        println!("  effective_price_0_to_1:   {:.10}", self.graph_effective_price_0_to_1);
        println!("  effective_price_1_to_0:   {:.10}", self.graph_effective_price_1_to_0);
        println!("----------------------------------------");
        println!("QUOTER PRICES (actual simulation):");
        if let Some(p) = self.quoter_price_0_to_1 {
            println!("  price_0_to_1:             {:.10}", p);
        } else {
            println!("  price_0_to_1:             FAILED");
        }
        if let Some(p) = self.quoter_price_1_to_0 {
            println!("  price_1_to_0:             {:.10}", p);
        } else {
            println!("  price_1_to_0:             FAILED");
        }
        println!("----------------------------------------");
        println!("DISCREPANCY:");
        if let Some(d) = self.discrepancy_0_to_1_bps {
            let sign = if d >= 0 { "+" } else { "" };
            println!("  0_to_1: {} {} bps ({}{:.2}%)", 
                if d.abs() > 100 { "⚠️ LARGE" } else { "  " },
                d, sign, d as f64 / 100.0);
        }
        if let Some(d) = self.discrepancy_1_to_0_bps {
            let sign = if d >= 0 { "+" } else { "" };
            println!("  1_to_0: {} {} bps ({}{:.2}%)", 
                if d.abs() > 100 { "⚠️ LARGE" } else { "  " },
                d, sign, d as f64 / 100.0);
        }
        println!("----------------------------------------");
        println!("FEE INFO:");
        println!("  stored_fee_raw:           {} (hundredths of bps)", self.stored_fee_raw);
        println!("  stored_fee_bps:           {} bps ({:.2}%)", self.stored_fee_bps, self.stored_fee_bps as f64 / 100.0);
        if let Some(f) = self.quoter_fee_bps {
            println!("  quoter_fee_bps:           {} bps ({:.2}%)", f, f as f64 / 100.0);
        }
        println!("----------------------------------------");
        println!("RAW DATA:");
        println!("  sqrtPriceX96:             {}", self.sqrt_price_x96);
        println!("  liquidity:                {}", self.liquidity);
        println!("  decimals0:                {}", self.decimals0);
        println!("  decimals1:                {}", self.decimals1);
        println!("========================================");
    }
}

/// Diagnose a single pool
pub async fn diagnose_pool<P: Provider + Clone + 'static>(
    provider: P,
    pool: &Pool,
) -> Result<PoolDiagnostic> {
    let quoter = QuoteFetcher::new(provider);

    // Standard test amount: 1 token with appropriate decimals
    let test_amount_0 = U256::from(10u128.pow(pool.decimals0 as u32));
    let test_amount_1 = U256::from(10u128.pow(pool.decimals1 as u32));

    // Get quoter prices in both directions
    let pool_info_0_to_1 = PoolInfo {
        address: pool.address,
        dex: pool.dex,
        token0: pool.token0,
        token1: pool.token1,
        fee: pool.fee,
        liquidity: pool.liquidity,
        zero_for_one: true,
        tick_spacing: None,
        hooks: None,
    };

    let pool_info_1_to_0 = PoolInfo {
        address: pool.address,
        dex: pool.dex,
        token0: pool.token0,
        token1: pool.token1,
        fee: pool.fee,
        liquidity: pool.liquidity,
        zero_for_one: false,
        tick_spacing: None,
        hooks: None,
    };

    let quote_0_to_1 = quoter
        .get_atomic_quotes(&[pool_info_0_to_1], test_amount_0, None)
        .await
        .ok();

    let quote_1_to_0 = quoter
        .get_atomic_quotes(&[pool_info_1_to_0], test_amount_1, None)
        .await
        .ok();

    let quoter_price_0_to_1 = quote_0_to_1.as_ref().and_then(|q| {
        q.quotes.first().map(|quote| {
            let out = quote.amount_out.to::<u128>() as f64;
            let in_amt = quote.amount_in.to::<u128>() as f64;
            if in_amt > 0.0 { out / in_amt } else { 0.0 }
        })
    });

    let quoter_price_1_to_0 = quote_1_to_0.as_ref().and_then(|q| {
        q.quotes.first().map(|quote| {
            let out = quote.amount_out.to::<u128>() as f64;
            let in_amt = quote.amount_in.to::<u128>() as f64;
            if in_amt > 0.0 { out / in_amt } else { 0.0 }
        })
    });

    let quoter_fee_bps = quote_0_to_1.as_ref().and_then(|q| {
        q.quotes.first().map(|quote| quote.fee_bps)
    });

    // Calculate discrepancy
    let discrepancy_0_to_1_bps = quoter_price_0_to_1.map(|qp| {
        let gp = pool.effective_price_0_to_1();
        if gp > 0.0 {
            ((gp - qp) / qp * 10000.0) as i32
        } else {
            0
        }
    });

    let discrepancy_1_to_0_bps = quoter_price_1_to_0.map(|qp| {
        let gp = pool.effective_price_1_to_0();
        if gp > 0.0 {
            ((gp - qp) / qp * 10000.0) as i32
        } else {
            0
        }
    });

    Ok(PoolDiagnostic {
        pool_address: pool.address,
        dex: pool.dex,
        token0_symbol: tokens::symbol(pool.token0),
        token1_symbol: tokens::symbol(pool.token1),
        graph_price_0_to_1: pool.price_0_to_1(),
        graph_price_1_to_0: pool.price_1_to_0(),
        graph_effective_price_0_to_1: pool.effective_price_0_to_1(),
        graph_effective_price_1_to_0: pool.effective_price_1_to_0(),
        quoter_price_0_to_1,
        quoter_price_1_to_0,
        discrepancy_0_to_1_bps,
        discrepancy_1_to_0_bps,
        stored_fee_raw: pool.fee,
        stored_fee_bps: pool.fee / 100,
        quoter_fee_bps,
        liquidity: pool.liquidity,
        sqrt_price_x96: pool.sqrt_price_x96,
        decimals0: pool.decimals0,
        decimals1: pool.decimals1,
    })
}

/// Diagnose an entire arbitrage cycle
pub async fn diagnose_cycle<P: Provider + Clone + 'static>(
    provider: P,
    cycle: &ArbitrageCycle,
    pools: &[Pool],
) -> Result<()> {
    println!("\n");
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║           ARBITRAGE CYCLE DIAGNOSTIC REPORT                  ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!("\nCycle: {}", cycle.token_path());
    println!("DEXes: {}", cycle.dex_path());
    println!("Graph Expected Return: {:.4}% ({} bps)", cycle.profit_percentage(), cycle.profit_bps());
    println!("\n");

    // Find the pools in the cycle
    for (i, pool_addr) in cycle.pools.iter().enumerate() {
        let pool = pools.iter().find(|p| p.address == *pool_addr);
        
        if let Some(pool) = pool {
            println!("\n--- Hop {} ---", i + 1);
            let diagnostic = diagnose_pool(provider.clone(), pool).await?;
            diagnostic.print();
        } else {
            println!("\n--- Hop {} ---", i + 1);
            println!("⚠️ Pool {} not found in pool list", pool_addr);
        }
    }

    // Summary
    println!("\n");
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║                        SUMMARY                               ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!("\nIf discrepancies are large (>100 bps), possible causes:");
    println!("  1. Price calculation bug in graph building");
    println!("  2. Stale price data (pool state changed between fetch and quote)");
    println!("  3. Decimal handling mismatch");
    println!("  4. Fee calculation differences");
    println!("\nFor LFJ pools specifically:");
    println!("  - Check if Q128.128 price conversion is correct");
    println!("  - Verify sqrtPriceX96 conversion preserves decimal info");
    println!("  - Ensure fee is in correct units (hundredths of bps vs bps)");

    Ok(())
}

/// Compare two prices and describe the discrepancy
pub fn describe_price_discrepancy(graph_price: f64, quoter_price: f64) -> String {
    if quoter_price == 0.0 {
        return "Quoter returned zero".to_string();
    }

    let ratio = graph_price / quoter_price;
    let diff_bps = ((ratio - 1.0) * 10000.0) as i32;

    if diff_bps.abs() < 5 {
        format!("✅ Match ({}bps)", diff_bps)
    } else if diff_bps.abs() < 50 {
        format!("⚠️ Minor discrepancy ({}bps)", diff_bps)
    } else if diff_bps.abs() < 500 {
        format!("🔶 Significant discrepancy ({}bps)", diff_bps)
    } else {
        format!("🔴 MAJOR discrepancy ({}bps) - likely bug!", diff_bps)
    }
}
