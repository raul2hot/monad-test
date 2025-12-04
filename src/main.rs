use alloy::providers::ProviderBuilder;
use eyre::Result;
use std::time::Duration;
use tokio::time::interval;
use tracing::{error, info, Level};
use tracing_subscriber::FmtSubscriber;

mod config;
mod display;
mod multicall;
mod pools;
mod price;

use config::{get_all_pools, get_lfj_pool, get_monday_trade_pool, get_v3_pools, POLL_INTERVAL_MS};
use display::{display_prices, init_arb_log};
use multicall::fetch_prices_batched;
use pools::{
    create_lfj_active_id_call, create_lfj_bin_step_call, create_slot0_call, PriceCall,
};

#[tokio::main]
async fn main() -> Result<()> {
    // Load environment variables
    dotenvy::dotenv().ok();

    // Setup logging (only show errors in production, use RUST_LOG for debug)
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::WARN)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    // Get RPC URL
    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set in .env file");

    info!("Connecting to Monad RPC: {}", rpc_url);

    // Create provider
    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url);

    // Get all pool configurations
    let all_pools = get_all_pools();

    info!("Monitoring {} pools", all_pools.len());

    // Initialize ARB log file
    let arb_log_path = init_arb_log();
    info!("ARB opportunities will be logged to: {}", arb_log_path.display());
    eprintln!(
        "\x1b[1;33mARB opportunities are logged to: {}\x1b[0m",
        arb_log_path.canonicalize().unwrap_or(arb_log_path).display()
    );

    // Create price calls for all pools based on their type
    let mut price_calls: Vec<PriceCall> = Vec::new();

    // V3 pools use slot0()
    for pool in get_v3_pools() {
        price_calls.push(create_slot0_call(&pool));
    }

    // LFJ pool needs both getActiveId() and getBinStep()
    let lfj_pool = get_lfj_pool();
    price_calls.push(create_lfj_active_id_call(&lfj_pool));
    price_calls.push(create_lfj_bin_step_call(&lfj_pool));

    // Monday Trade uses slot0() (V3-style, inspired by Uniswap V3)
    let monday_pool = get_monday_trade_pool();
    price_calls.push(create_slot0_call(&monday_pool));

    // Polling loop
    let mut poll_interval = interval(Duration::from_millis(POLL_INTERVAL_MS));

    loop {
        poll_interval.tick().await;

        match fetch_prices_batched(&provider, price_calls.clone()).await {
            Ok((prices, elapsed_ms)) => {
                display_prices(&prices, elapsed_ms);
            }
            Err(e) => {
                error!("Failed to fetch prices: {}", e);
                display::clear_screen();
                println!("\x1b[1;31mError fetching prices: {}\x1b[0m", e);
                println!("\nRetrying in {} ms...", POLL_INTERVAL_MS);
            }
        }
    }
}
