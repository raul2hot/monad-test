mod config;
mod dex;
mod graph;

use alloy::providers::{Provider, ProviderBuilder};
use std::time::Duration;
use tokio::time::interval;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use config::{tokens, Config};
use dex::{
    lfj::LfjClient, pancakeswap::PancakeSwapClient, uniswap_v3::UniswapV3Client,
    uniswap_v4::UniswapV4Client, DexClient,
};
use graph::{ArbitrageGraph, BoundedBellmanFord};

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
    println!("   MVP Version 0.1.0 - READ-ONLY Mode");
    println!("==================================================");
    println!();

    info!("Starting Monad Arbitrage MVP...");

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

    // Token list to monitor
    let tokens_to_monitor = vec![
        tokens::WMON,
        tokens::USDC,
        tokens::USDT,
        tokens::WETH,
        tokens::WBTC,
        tokens::SMON,
        tokens::GMON,
    ];

    info!("Monitoring {} tokens:", tokens_to_monitor.len());
    for token in &tokens_to_monitor {
        info!("  - {} ({})", tokens::symbol(*token), token);
    }

    info!("Settings:");
    info!("  - Poll interval: {}ms", config.poll_interval_ms);
    info!("  - Max hops: {}", config.max_hops);
    info!("  - Min profit: {} bps ({}%)", config.min_profit_bps, config.min_profit_bps as f64 / 100.0);

    println!();
    println!("Starting arbitrage detection loop...");
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
        let mut total_pools = 0;

        // Fetch pools from Uniswap V3
        match uniswap_v3.get_pools(&tokens_to_monitor).await {
            Ok(pools) => {
                let count = pools.len();
                for pool in &pools {
                    graph.add_pool(pool);
                }
                total_pools += count;
                if count > 0 {
                    info!("Found {} Uniswap V3 pools", count);
                }
            }
            Err(e) => {
                warn!("Failed to fetch Uniswap V3 pools: {}", e);
            }
        }

        // Fetch pools from Uniswap V4
        match uniswap_v4.get_pools(&tokens_to_monitor).await {
            Ok(pools) => {
                let count = pools.len();
                for pool in &pools {
                    graph.add_pool(pool);
                }
                total_pools += count;
                if count > 0 {
                    info!("Found {} Uniswap V4 pools", count);
                }
            }
            Err(e) => {
                warn!("Failed to fetch Uniswap V4 pools: {}", e);
            }
        }

        // Fetch pools from PancakeSwap
        match pancakeswap.get_pools(&tokens_to_monitor).await {
            Ok(pools) => {
                let count = pools.len();
                for pool in &pools {
                    graph.add_pool(pool);
                }
                total_pools += count;
                if count > 0 {
                    info!("Found {} PancakeSwap V3 pools", count);
                }
            }
            Err(e) => {
                warn!("Failed to fetch PancakeSwap pools: {}", e);
            }
        }

        // Fetch pools from LFJ
        match lfj.get_pools(&tokens_to_monitor).await {
            Ok(pools) => {
                let count = pools.len();
                for pool in &pools {
                    graph.add_pool(pool);
                }
                total_pools += count;
                if count > 0 {
                    info!("Found {} LFJ pools", count);
                }
            }
            Err(e) => {
                warn!("Failed to fetch LFJ pools: {}", e);
            }
        }

        if graph.edge_count() == 0 {
            info!(
                "[Iteration {}] No pools found yet, waiting...",
                iteration
            );
            continue;
        }

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
            info!("No arbitrage opportunities found above threshold");
        } else {
            println!();
            println!("========================================");
            println!(" ARBITRAGE OPPORTUNITIES DETECTED: {}", cycles.len());
            println!("========================================");

            // Log top opportunities
            for (i, cycle) in cycles.iter().take(10).enumerate() {
                let cross_dex = if cycle.is_cross_dex() {
                    "CROSS"
                } else {
                    "SINGLE"
                };

                println!();
                println!(
                    "--- Opportunity #{} [{}-DEX] [Confidence: {}] ---",
                    i + 1,
                    cross_dex,
                    cycle.confidence_level()
                );
                println!("   Path: {}", cycle.token_path());
                println!(
                    "   Profit: {:.4}% ({} bps)",
                    cycle.profit_percentage(),
                    cycle.profit_bps()
                );
                println!("   Hops: {}", cycle.hop_count());
                println!("   DEXes: {}", cycle.dex_path());
                println!("   Avg Fee: {:.2} bps", cycle.avg_fee_bps());
                println!("   Expected Return: {:.6}x", cycle.expected_return);
                println!("   Confidence Score: {:.2}", cycle.confidence_score());

                // Print pool addresses for debugging
                println!("   Pools:");
                for (j, pool) in cycle.pools.iter().enumerate() {
                    println!("     {}: {} ({})", j + 1, pool, cycle.dexes[j]);
                }
            }

            if cycles.len() > 10 {
                println!();
                println!("... and {} more opportunities", cycles.len() - 10);
            }
        }

        println!();
        println!("-------------------------------------------");
    }
}
