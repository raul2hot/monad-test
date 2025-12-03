//! Monad Arbitrage Bot - Monorail vs Direct Pool Strategy

mod config;
mod monorail;
mod pools;

use alloy::providers::{Provider, ProviderBuilder};
use eyre::Result;
use std::env;
use std::time::Duration;
use tokio::time::interval;
use tracing::{info, warn, Level};

#[derive(Debug)]
struct ArbOpportunity {
    monorail_price: f64,
    pool_price: f64,
    pool_name: String,
    spread_pct: f64,
    direction: String,
}

impl ArbOpportunity {
    fn print(&self) {
        println!("\n============ ARBITRAGE DETECTED ============");
        println!("  Monorail Price: ${:.6}", self.monorail_price);
        println!("  {} Price:  ${:.6}", self.pool_name, self.pool_price);
        println!("  Spread:         {:.3}%", self.spread_pct);
        println!("  Direction:      {}", self.direction);
        println!(
            "  Est. Profit:    {:.3}% (before gas)",
            self.spread_pct - 0.3
        );
        println!("=============================================\n");
    }
}

fn check_arbitrage(
    monorail_price: f64,
    pool_price: f64,
    pool_name: &str,
) -> Option<ArbOpportunity> {
    // Validate prices
    if monorail_price <= 0.0 || pool_price <= 0.0 {
        return None;
    }

    let spread_pct = ((monorail_price - pool_price) / pool_price) * 100.0;

    // Sanity check
    if spread_pct.abs() > config::MAX_SPREAD_PCT {
        warn!("Unrealistic spread: {:.2}% - ignoring", spread_pct);
        return None;
    }

    // Check minimum spread
    if spread_pct.abs() > config::MIN_SPREAD_PCT {
        let direction = if spread_pct > 0.0 {
            format!("BUY on {} -> SELL via Monorail", pool_name)
        } else {
            format!("BUY via Monorail -> SELL on {}", pool_name)
        };

        Some(ArbOpportunity {
            monorail_price,
            pool_price,
            pool_name: pool_name.to_string(),
            spread_pct: spread_pct.abs(),
            direction,
        })
    } else {
        None
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    dotenvy::dotenv().ok();

    println!("==========================================");
    println!("  Monad Arbitrage Bot");
    println!("  Strategy: Monorail vs Direct Pools");
    println!("  Pair: MON/USDC");
    println!("==========================================\n");

    // HTTP RPC is sufficient (no WebSocket needed)
    let rpc_url = env::var("MONAD_RPC_URL")
        .unwrap_or_else(|_| "https://monad-mainnet.g.alchemy.com/v2/YOUR_KEY".to_string());

    let provider = ProviderBuilder::new().connect_http(rpc_url.parse()?);

    let chain_id = provider.get_chain_id().await?;
    info!("Connected to chain {}", chain_id);

    // Initialize Monorail client
    let monorail = monorail::MonorailClient::new(config::MONORAIL_APP_ID);

    // Determine token0 for price calculation
    let wmon = config::WMON.to_lowercase();
    let usdc = config::USDC.to_lowercase();
    let token0_is_mon = wmon < usdc;
    info!("Token0 is MON: {}", token0_is_mon);

    // Verify Uniswap pool has liquidity
    let uni_has_liq = pools::has_liquidity(&provider, config::UNISWAP_MON_USDC_POOL).await?;
    info!("Uniswap MON/USDC pool has liquidity: {}", uni_has_liq);

    if !uni_has_liq {
        return Err(eyre::eyre!("Uniswap pool has no liquidity"));
    }

    // Main loop - poll every 2 seconds
    let mut poll_interval = interval(Duration::from_secs(2));

    println!("\nStarting price monitoring...\n");

    loop {
        poll_interval.tick().await;

        // Get Monorail aggregated price
        let monorail_price = match monorail.get_mon_price().await {
            Ok(p) => p,
            Err(e) => {
                warn!("Monorail API error: {}", e);
                continue;
            }
        };

        // Get Uniswap direct pool price
        let uniswap_price = match pools::get_pool_price(
            &provider,
            config::UNISWAP_MON_USDC_POOL,
            token0_is_mon,
        )
        .await
        {
            Ok(p) => p,
            Err(e) => {
                warn!("Uniswap pool error: {}", e);
                continue;
            }
        };

        // Print current prices
        println!(
            "[{}] MON/USDC | Monorail: ${:.6} | Uniswap: ${:.6} | Spread: {:.3}%",
            chrono::Local::now().format("%H:%M:%S"),
            monorail_price,
            uniswap_price,
            ((monorail_price - uniswap_price) / uniswap_price * 100.0)
        );

        // Check for arbitrage
        if let Some(arb) = check_arbitrage(monorail_price, uniswap_price, "Uniswap") {
            arb.print();
        }

        // TODO: Add PancakeSwap pool comparison here
        // let pancake_price = pools::get_pool_price(...).await?;
        // if let Some(arb) = check_arbitrage(monorail_price, pancake_price, "PancakeSwap") {
        //     arb.print();
        // }
    }
}
