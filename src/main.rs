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

use config::{get_v3_pools, POLL_INTERVAL_MS};
use display::display_prices;
use multicall::fetch_prices_batched;
use pools::create_slot0_call;

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

    // Get V3 pool configurations
    let v3_pools = get_v3_pools();

    info!("Monitoring {} V3 pools", v3_pools.len());

    // Create price calls for all V3 pools
    let price_calls: Vec<_> = v3_pools.iter().map(create_slot0_call).collect();

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
