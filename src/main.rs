mod config;
mod dex;
mod graph;
mod simulation;

use alloy::primitives::U256;
use alloy::providers::{Provider, ProviderBuilder};
use std::time::Duration;
use tokio::time::interval;
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

use config::{tokens, Config};
use dex::{
    lfj::LfjClient, pancakeswap::PancakeSwapClient, uniswap_v3::UniswapV3Client,
    uniswap_v4::UniswapV4Client, DexClient,
};
use graph::{ArbitrageGraph, BoundedBellmanFord};
use simulation::{Simulator, SimulationConfidence};

/// Default simulation amount: 1 WMON (18 decimals)
const SIMULATION_AMOUNT: u128 = 100_000_000_000_000_000_000; // 1e18

#[tokio::main]
async fn main() -> eyre::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("monad_arb_mvp=info".parse()?),
        )
        .init();

    println!();
    println!("==================================================");
    println!("   MONAD MAINNET ARBITRAGE OPPORTUNITY LOGGER");
    println!("   MVP Version 0.2.0 - With Simulation & Verification");
    println!("==================================================");
    println!();

    info!("Starting Monad Arbitrage MVP with Simulation...");

    // Load configuration
    let config = Config::from_env()?;
    info!(
        "Connecting to Monad Mainnet (Chain ID: {})",
        config.chain_id
    );
    info!("RPC URL: {}", &config.rpc_url[..config.rpc_url.len().min(50)]);

    // Create provider
    let provider = ProviderBuilder::new().connect_http(config.rpc_url.parse()?);

    // Verify connection
    match provider.get_block_number().await {
        Ok(block) => {
            info!("Connected! Current block: {}", block);
        }
        Err(e) => {
            error!("Failed to connect to RPC: {}", e);
            return Err(e.into());
        }
    }

    // Initialize DEX clients
    let uniswap_v3 = UniswapV3Client::new(provider.clone());
    let uniswap_v4 = UniswapV4Client::new(provider.clone());
    let pancakeswap = PancakeSwapClient::new(provider.clone());
    let lfj = LfjClient::new(provider.clone());

    // Initialize simulator
    let simulator = Simulator::new(provider.clone());

    // Token list to monitor - expanded for better cross-DEX coverage
    let tokens_to_monitor = vec![
        // Core tokens
        tokens::WMON,
        tokens::USDC,
        tokens::USDT,
        tokens::WETH,
        tokens::WBTC,
        // Additional stablecoins
        tokens::AUSD,
        tokens::USD1,
        tokens::LVUSD,
        // LSTs (important for MON arbitrage)
        tokens::SMON,
        tokens::GMON,
        tokens::SHMON,
        tokens::APRMON,
        // Wrapped assets
        tokens::WSTETH,
        tokens::WEETH,
        tokens::SOL,
        tokens::BTCB,
        // Meme/community
        tokens::GMONAD,
    ];

    info!("Monitoring {} tokens:", tokens_to_monitor.len());
    for token in &tokens_to_monitor {
        info!("  - {} ({})", tokens::symbol(*token), token);
    }

    info!("Settings:");
    info!("  - Poll interval: {}ms", config.poll_interval_ms);
    info!("  - Max hops: {}", config.max_hops);
    info!("  - Min profit: {} bps ({}%)", config.min_profit_bps, config.min_profit_bps as f64 / 100.0);
    info!("  - Simulation amount: {} WMON", SIMULATION_AMOUNT / 10u128.pow(18));
    info!("  - Flash loan provider: Neverland (9 bps fee)");

    println!();
    println!("Starting arbitrage detection loop with simulation...");
    println!("Press Ctrl+C to stop");
    println!();

    // Main loop
    let mut poll_interval = interval(Duration::from_millis(config.poll_interval_ms));
    let mut iteration = 0u64;

    loop {
        poll_interval.tick().await;
        iteration += 1;

        // Build fresh graph each iteration
        let mut graph = ArbitrageGraph::new();
        let mut uniswap_v3_count = 0;
        let mut uniswap_v4_count = 0;
        let mut pancakeswap_count = 0;
        let mut lfj_count = 0;

        // Fetch pools from all DEXes
        // Uniswap V3
        match uniswap_v3.get_pools(&tokens_to_monitor).await {
            Ok(pools) => {
                uniswap_v3_count = pools.len();
                for pool in &pools {
                    graph.add_pool(pool);
                }
            }
            Err(e) => {
                warn!("Failed to fetch Uniswap V3 pools: {}", e);
            }
        }

        // Uniswap V4
        match uniswap_v4.get_pools(&tokens_to_monitor).await {
            Ok(pools) => {
                uniswap_v4_count = pools.len();
                for pool in &pools {
                    graph.add_pool(pool);
                }
            }
            Err(e) => {
                warn!("Failed to fetch Uniswap V4 pools: {}", e);
            }
        }

        // PancakeSwap
        match pancakeswap.get_pools(&tokens_to_monitor).await {
            Ok(pools) => {
                pancakeswap_count = pools.len();
                for pool in &pools {
                    graph.add_pool(pool);
                }
            }
            Err(e) => {
                warn!("Failed to fetch PancakeSwap pools: {}", e);
            }
        }

        // LFJ
        match lfj.get_pools(&tokens_to_monitor).await {
            Ok(pools) => {
                lfj_count = pools.len();
                for pool in &pools {
                    graph.add_pool(pool);
                }
            }
            Err(e) => {
                warn!("Failed to fetch LFJ pools: {}", e);
            }
        }

        let total_pools = uniswap_v3_count + uniswap_v4_count + pancakeswap_count + lfj_count;

        if graph.edge_count() == 0 {
            info!(
                "[Iteration {}] No pools found yet, waiting...",
                iteration
            );
            continue;
        }

        // Pool discovery summary
        info!("=== POOL DISCOVERY SUMMARY ===");
        info!("Uniswap V3: {} pools", uniswap_v3_count);
        info!("Uniswap V4: {} pools (vanilla only)", uniswap_v4_count);
        info!("PancakeSwap: {} pools", pancakeswap_count);
        info!("LFJ: {} pools", lfj_count);
        info!("Total: {} pools", total_pools);
        info!("==============================");

        info!(
            "[Iteration {}] Graph: {} nodes, {} edges ({} pools)",
            iteration,
            graph.node_count(),
            graph.edge_count(),
            total_pools
        );

        // Find arbitrage cycles
        let detector = BoundedBellmanFord::new(&graph, config.max_hops, config.min_profit_bps);
        let cycles = detector.find_all_cycles(&tokens::BASE_TOKENS);

        if cycles.is_empty() {
            debug!("No arbitrage opportunities found above threshold");
            continue;
        }

        // Analyze cycle composition
        let cross_dex_count = cycles.iter().filter(|c| c.is_cross_dex()).count();
        let v3_only = cycles.iter().filter(|c| c.dexes.iter().all(|d| *d == dex::Dex::UniswapV3)).count();
        let v4_only = cycles.iter().filter(|c| c.dexes.iter().all(|d| *d == dex::Dex::UniswapV4)).count();
        let pancake_only = cycles.iter().filter(|c| c.dexes.iter().all(|d| *d == dex::Dex::PancakeSwapV3)).count();
        let lfj_only = cycles.iter().filter(|c| c.dexes.iter().all(|d| *d == dex::Dex::LFJ)).count();

        let mut hop_counts: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
        for cycle in &cycles {
            *hop_counts.entry(cycle.hop_count()).or_insert(0) += 1;
        }

        info!("=== CYCLE ANALYSIS ===");
        info!("Total candidates: {}", cycles.len());
        info!("Cross-DEX: {}", cross_dex_count);
        info!("Uniswap V3 only: {}", v3_only);
        info!("Uniswap V4 only: {}", v4_only);
        info!("PancakeSwap only: {}", pancake_only);
        info!("LFJ only: {}", lfj_only);
        info!("Hop distribution: {:?}", hop_counts);
        info!("======================");

        // Log first 10 unique paths for debugging
        for (i, cycle) in cycles.iter().take(10).enumerate() {
            info!(
                "Candidate #{}: {} | DEXes: {} | Profit: {:.2}%",
                i + 1,
                cycle.token_path(),
                cycle.dex_path(),
                cycle.profit_percentage()
            );
        }

        info!("Found {} potential opportunities, simulating...", cycles.len());

        // Simulate each opportunity
        let simulation_amount = U256::from(SIMULATION_AMOUNT);
        let mut verified_count = 0;
        let mut rejected_count = 0;

        for (i, cycle) in cycles.iter().take(10).enumerate() {
            // Simulate the cycle
            match simulator.simulate_cycle(cycle, simulation_amount).await {
                Ok(result) => {
                    if result.above_threshold {
                        verified_count += 1;
                        print_verified_opportunity(i + 1, &result);
                    } else {
                        rejected_count += 1;
                        print_rejected_opportunity(i + 1, &result);
                    }
                }
                Err(e) => {
                    warn!("Simulation failed for opportunity #{}: {}", i + 1, e);
                    rejected_count += 1;
                }
            }
        }

        // Print summary
        println!();
        println!("========================================");
        println!(" SIMULATION SUMMARY");
        println!("========================================");
        println!("   Verified opportunities: {}", verified_count);
        println!("   Rejected opportunities: {}", rejected_count);
        println!("   Total candidates: {}", cycles.len());
        println!("========================================");
        println!();
    }
}

/// Print a verified (profitable) opportunity
fn print_verified_opportunity(num: usize, result: &simulation::SimulationResult) {
    println!();
    println!("========================================");
    println!(" VERIFIED OPPORTUNITY #{}", num);
    println!("========================================");
    println!("   Path: {}", result.token_path());
    println!("   DEXes: {}", result.dex_path());
    println!("   Block: {}", result.block_number);
    println!("   Confidence: {}", result.confidence);
    println!();
    println!("   Pool Details:");
    for (i, quote) in result.quotes.quotes.iter().enumerate() {
        let dex_name = result.dexes.get(i).map(|d| d.to_string()).unwrap_or_default();
        let pool_addr = result.pools.get(i).map(|p| format!("{}...", &format!("{:?}", p)[..10])).unwrap_or_default();
        println!("     {}: {} ({})", i + 1, pool_addr, dex_name);
        println!("        Fee: {} bps ({:.2}%)", quote.fee_bps, quote.fee_bps as f64 / 100.0);
        println!("        Quote: {} -> {}", quote.amount_in, quote.amount_out);
    }
    println!();
    println!("   Profit Breakdown:");
    println!("     Gross Profit:     {} bps", result.gross_profit_bps);
    println!("     DEX Fees:         -{} bps", result.total_dex_fees_bps);
    println!("     Flash Loan Fee:   -{} bps (Neverland)", result.flash_loan_fee_bps);
    println!("     Gas Cost:         ~{} MON", format_mon(result.gas_cost_wei));
    println!("     ─────────────────────────────");
    println!("     NET PROFIT:       {} bps", result.net_profit_bps);
    println!("     Status:           PROFITABLE ✓");
    println!("========================================");
}

/// Print a rejected (not profitable) opportunity
fn print_rejected_opportunity(num: usize, result: &simulation::SimulationResult) {
    let status = if result.is_profitable {
        "BELOW THRESHOLD"
    } else {
        "NOT PROFITABLE"
    };

    debug!(
        "Rejected #{}: {} | Gross: {} bps | DEX Fees: -{} bps | Flash Loan: -{} bps | Net: {} bps | Reason: {}",
        num,
        result.token_path(),
        result.gross_profit_bps,
        result.total_dex_fees_bps,
        result.flash_loan_fee_bps,
        result.net_profit_bps,
        result.rejection_reason.as_ref().unwrap_or(&status.to_string())
    );

    // Only print summary for rejected
    println!();
    println!("--- Rejected #{} [{}] ---", num, status);
    println!("   Path: {}", result.token_path());
    println!("   Gross: {} bps | DEX Fees: -{} bps | Flash Loan: -{} bps | Net: {} bps",
        result.gross_profit_bps,
        result.total_dex_fees_bps,
        result.flash_loan_fee_bps,
        result.net_profit_bps
    );
    if let Some(reason) = &result.rejection_reason {
        println!("   Reason: {}", reason);
    }
}

/// Format MON amount (18 decimals) for display
fn format_mon(amount: U256) -> String {
    let amount_u128: u128 = amount.try_into().unwrap_or(0);
    if amount_u128 == 0 {
        return "0".to_string();
    }

    let whole = amount_u128 / 10u128.pow(18);
    let frac = (amount_u128 % 10u128.pow(18)) / 10u128.pow(14);

    if whole > 0 {
        format!("{}.{:04}", whole, frac)
    } else {
        format!("0.{:04}", frac)
    }
}
